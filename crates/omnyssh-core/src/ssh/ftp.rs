//! Plain FTP / explicit FTPS backend for the Files tab.
//!
//! Used when a host's [`crate::ssh::client::FileAccess`] is `Ftp` or `Ftps` —
//! typically a device that answers SSH for a shell but has no SFTP subsystem
//! and only exposes plain FTP for file access. Implicit FTPS (port 990) is
//! deprecated and intentionally not supported; only explicit `AUTH TLS` is.
//!
//! FTP has no equivalent of SSH's port-22-for-everything: it always dials the
//! well-known FTP port (21), independent of the host's SSH `port` field, and
//! reuses the SSH `user`/`password` for login since there is currently no
//! separate FTP credential field on `Host`.

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use suppaftp::list::File as FtpListEntry;
use suppaftp::tokio::{AsyncFtpStream, AsyncRustlsConnector, AsyncRustlsFtpStream};
use suppaftp::tokio_rustls::{self, rustls};
use suppaftp::types::FileType as FtpFileType;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::event::{CoreEvent, TransferId};
use crate::ssh::backend::{guard_local_path, FileTransferBackend};
use crate::ssh::client::{FileAccess, Host};
use crate::ssh::sftp::FileEntry;

/// The FTP protocol's own well-known port; unrelated to `Host::port`, which
/// is the SSH port used for the shell.
const FTP_PORT: u16 = 21;

/// A plain-FTP or an explicit-FTPS control connection.
///
/// Both variants expose the same command set (they only differ in the
/// generic TLS-stream type parameter of `suppaftp`'s client), so every
/// operation below matches and delegates to whichever is active.
enum FtpConn {
    Plain(AsyncFtpStream),
    Tls(Box<AsyncRustlsFtpStream>),
}

/// FTP/FTPS implementation of [`FileTransferBackend`].
pub struct FtpBackend {
    conn: FtpConn,
}

impl FtpBackend {
    /// Connects and logs in to `host` per its [`FileAccess`] (`Ftp` or `Ftps`).
    ///
    /// # Errors
    /// Returns an error if `file_access` isn't an FTP variant, the TCP
    /// connect / TLS handshake fails, or `host.password` is unset (FTP has
    /// no equivalent of SSH key auth, so a password is required).
    pub async fn connect(host: &Host) -> anyhow::Result<Self> {
        let addr = format!("{}:{FTP_PORT}", host.hostname);
        let mut conn = match host.file_access {
            FileAccess::Ftp => FtpConn::Plain(
                AsyncFtpStream::connect(&addr)
                    .await
                    .with_context(|| format!("FTP connect to {addr}"))?,
            ),
            FileAccess::Ftps => {
                let stream = AsyncRustlsFtpStream::connect(&addr)
                    .await
                    .with_context(|| format!("FTP connect to {addr}"))?;
                let connector = AsyncRustlsConnector::from(build_rustls_connector()?);
                let secured = stream
                    .into_secure(connector, &host.hostname)
                    .await
                    .context("FTPS AUTH TLS upgrade")?;
                FtpConn::Tls(Box::new(secured))
            }
            FileAccess::Sftp | FileAccess::None => {
                anyhow::bail!("FtpBackend::connect called for a non-FTP file_access")
            }
        };

        let password = host.password.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "host '{}' has no password set — FTP login needs one \
                 (SSH key auth doesn't carry over to FTP)",
                host.name
            )
        })?;
        match &mut conn {
            FtpConn::Plain(c) => c.login(host.user.as_str(), password).await,
            FtpConn::Tls(c) => c.login(host.user.as_str(), password).await,
        }
        .context("FTP login")?;

        // Binary by default: FTP's ASCII mode would corrupt anything that
        // isn't text (and even mangle text with the wrong line endings).
        match &mut conn {
            FtpConn::Plain(c) => c.transfer_type(FtpFileType::Binary).await,
            FtpConn::Tls(c) => c.transfer_type(FtpFileType::Binary).await,
        }
        .context("set binary transfer type")?;

        Ok(Self { conn })
    }
}

/// Builds a rustls client TLS connector trusting the standard Mozilla root
/// CAs (via `webpki-roots`). A self-signed FTPS server won't validate against
/// this — there is no UI yet to pin or override a certificate, so that case
/// is a known limitation rather than a bug.
fn build_rustls_connector() -> anyhow::Result<tokio_rustls::TlsConnector> {
    let root_store = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// Joins a directory `path` and an entry `name` into a full remote path,
/// matching the join convention used for SFTP listings.
fn join_remote(path: &str, name: &str) -> String {
    if path.ends_with('/') {
        format!("{path}{name}")
    } else {
        format!("{path}/{name}")
    }
}

/// Reads from `src` into `dst`, sending [`CoreEvent::FileTransferProgress`]
/// after each chunk. Shared by the download path for both `FtpConn` variants.
async fn copy_with_progress<R, W>(
    mut src: R,
    dst: &mut W,
    transfer_id: TransferId,
    total: u64,
    event_tx: &mpsc::Sender<CoreEvent>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 65_536];
    let mut done: u64 = 0;
    loop {
        let n = src.read(&mut buf).await.context("read source")?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await.context("write destination")?;
        done += n as u64;
        let _ = event_tx
            .send(CoreEvent::FileTransferProgress(transfer_id, done, total))
            .await;
    }
    Ok(())
}

#[async_trait]
impl FileTransferBackend for FtpBackend {
    async fn list_dir(&mut self, path: &str) -> anyhow::Result<Vec<FileEntry>> {
        let lines = match &mut self.conn {
            FtpConn::Plain(c) => c.list(Some(path)).await,
            FtpConn::Tls(c) => c.list(Some(path)).await,
        }
        .with_context(|| format!("LIST '{path}'"))?;

        let mut entries: Vec<FileEntry> = Vec::new();

        if let Some(parent) = std::path::Path::new(path).parent() {
            let parent_str = parent.to_string_lossy();
            let parent_str = if parent_str.is_empty() { "/" } else { &parent_str };
            entries.push(FileEntry {
                name: "..".to_string(),
                path: parent_str.to_string(),
                size: 0,
                is_dir: true,
            });
        }

        for line in lines {
            let file: FtpListEntry = match line.parse() {
                Ok(f) => f,
                Err(_) => {
                    tracing::debug!("unparsable LIST line from '{path}': {line}");
                    continue;
                }
            };
            let name = file.name().to_string();
            if name == "." || name == ".." {
                continue;
            }
            entries.push(FileEntry {
                path: join_remote(path, &name),
                name,
                size: file.size() as u64,
                is_dir: file.is_directory(),
            });
        }

        entries.sort_by(|a, b| {
            if a.name == ".." {
                return std::cmp::Ordering::Less;
            }
            if b.name == ".." {
                return std::cmp::Ordering::Greater;
            }
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });

        Ok(entries)
    }

    async fn download(
        &mut self,
        remote: &str,
        local: &str,
        transfer_id: TransferId,
        event_tx: &mpsc::Sender<CoreEvent>,
    ) -> anyhow::Result<()> {
        guard_local_path(local, remote)?;

        let mut local_file = tokio::fs::File::create(local)
            .await
            .context("create local file")?;

        match &mut self.conn {
            FtpConn::Plain(c) => {
                let total = c.size(remote).await.unwrap_or(0) as u64;
                let mut stream = c
                    .retr_as_stream(remote)
                    .await
                    .context("open remote file for download")?;
                copy_with_progress(&mut stream, &mut local_file, transfer_id, total, event_tx)
                    .await?;
                stream.finish().await.context("finish FTP download")?;
            }
            FtpConn::Tls(c) => {
                let total = c.size(remote).await.unwrap_or(0) as u64;
                let mut stream = c
                    .retr_as_stream(remote)
                    .await
                    .context("open remote file for download")?;
                copy_with_progress(&mut stream, &mut local_file, transfer_id, total, event_tx)
                    .await?;
                stream.finish().await.context("finish FTP download")?;
            }
        }

        Ok(())
    }

    async fn upload(
        &mut self,
        local: &str,
        remote: &str,
        transfer_id: TransferId,
        event_tx: &mpsc::Sender<CoreEvent>,
    ) -> anyhow::Result<()> {
        guard_local_path(local, remote)?;

        let mut local_file = tokio::fs::File::open(local)
            .await
            .context("open local file for upload")?;
        let total = local_file.metadata().await.map(|m| m.len()).unwrap_or(0);

        match &mut self.conn {
            FtpConn::Plain(c) => {
                let mut stream = c
                    .put_with_stream(remote)
                    .await
                    .context("create remote file for upload")?;
                copy_with_progress(&mut local_file, &mut stream, transfer_id, total, event_tx)
                    .await?;
                stream.finish().await.context("finish FTP upload")?;
            }
            FtpConn::Tls(c) => {
                let mut stream = c
                    .put_with_stream(remote)
                    .await
                    .context("create remote file for upload")?;
                copy_with_progress(&mut local_file, &mut stream, transfer_id, total, event_tx)
                    .await?;
                stream.finish().await.context("finish FTP upload")?;
            }
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        let file_result = match &mut self.conn {
            FtpConn::Plain(c) => c.rm(path).await,
            FtpConn::Tls(c) => c.rm(path).await,
        };
        if file_result.is_ok() {
            return Ok(());
        }
        match &mut self.conn {
            FtpConn::Plain(c) => c.rmdir(path).await,
            FtpConn::Tls(c) => c.rmdir(path).await,
        }
        .with_context(|| format!("delete '{path}'"))
    }

    async fn mkdir(&mut self, path: &str) -> anyhow::Result<()> {
        match &mut self.conn {
            FtpConn::Plain(c) => c.mkdir(path).await,
            FtpConn::Tls(c) => c.mkdir(path).await,
        }
        .with_context(|| format!("mkdir '{path}'"))
    }

    async fn rename(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        match &mut self.conn {
            FtpConn::Plain(c) => c.rename(from, to).await,
            FtpConn::Tls(c) => c.rename(from, to).await,
        }
        .with_context(|| format!("rename '{from}' -> '{to}'"))
    }

    async fn read_preview(&mut self, path: &str) -> anyhow::Result<String> {
        let mut buf = vec![0u8; 4_096];
        let n = match &mut self.conn {
            FtpConn::Plain(c) => {
                let mut stream = c.retr_as_stream(path).await.context("open for preview")?;
                let n = stream.read(&mut buf).await.context("read preview bytes")?;
                // Best-effort close: a partial read often makes the server reply
                // 426 instead of 226, which would otherwise fail an otherwise
                // successful preview.
                let _ = stream.finish().await;
                n
            }
            FtpConn::Tls(c) => {
                let mut stream = c.retr_as_stream(path).await.context("open for preview")?;
                let n = stream.read(&mut buf).await.context("read preview bytes")?;
                let _ = stream.finish().await;
                n
            }
        };
        buf.truncate(n);
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}
