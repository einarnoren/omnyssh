//! File manager operations for the Files tab.
//!
//! Provides [`SftpManager`] — a persistent background task that owns a
//! [`crate::ssh::backend::FileTransferBackend`] (SFTP over SSH, or plain
//! FTP/FTPS — picked from the host's [`crate::ssh::client::FileAccess`]) and
//! processes [`SftpCommand`] messages sent from the UI thread.
//!
//! All operations are non-blocking from the UI perspective.
//! Progress is reported via [`CoreEvent::FileTransferProgress`].

use anyhow::Context;
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::event::{CoreEvent, TransferId};
use crate::ssh::backend::{guard_local_path, FileTransferBackend};
use crate::ssh::client::{FileAccess, Host};
use crate::ssh::ftp::FtpBackend;
use crate::ssh::session::SshSession;

// ---------------------------------------------------------------------------
// FileEntry — represents one file or directory in a panel listing
// ---------------------------------------------------------------------------

/// Metadata for a single file or directory in a file panel.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Base file name (not the full path).
    pub name: String,
    /// Absolute path string (used as the stable identifier for marked sets).
    pub path: String,
    /// File size in bytes (`0` for directories).
    pub size: u64,
    /// `true` when this entry is a directory.
    pub is_dir: bool,
}

// ---------------------------------------------------------------------------
// SftpCommand — sent from UI thread → SftpManager background task
// ---------------------------------------------------------------------------

/// Commands processed by the [`SftpManager`] background task.
pub enum SftpCommand {
    /// List the entries in a remote directory.
    ListDir(String),
    /// Download a remote file to a local path.
    Download {
        remote: String,
        local: String,
        transfer_id: TransferId,
    },
    /// Upload a local file to a remote path.
    Upload {
        local: String,
        remote: String,
        transfer_id: TransferId,
    },
    /// Delete a remote file (falls back to removing an empty directory).
    Delete(String),
    /// Create a remote directory.
    MkDir(String),
    /// Rename / move a remote path.
    Rename { from: String, to: String },
    /// Read the first 4 096 bytes of a remote file for preview.
    ReadPreview(String),
    /// Shut down the task gracefully.
    Disconnect,
}

// ---------------------------------------------------------------------------
// SftpManager — handle held by App to communicate with the background task
// ---------------------------------------------------------------------------

/// Manages a persistent file-transfer background task.
///
/// Use [`SftpManager::connect`] to create, [`SftpManager::send`] to enqueue
/// commands, and [`SftpManager::disconnect`] for a clean shutdown.
#[derive(Debug)]
pub struct SftpManager {
    cmd_tx: mpsc::Sender<SftpCommand>,
}

impl SftpManager {
    /// Connects to `host`'s Files backend (picked from `host.file_access`)
    /// and spawns the background task that serves [`SftpCommand`]s.
    ///
    /// On success sends [`CoreEvent::SftpConnected`] through `event_tx`.
    /// On failure the task sends [`CoreEvent::SftpDisconnected`].
    ///
    /// # Errors
    /// Returns an error if `file_access` is [`FileAccess::None`], or the
    /// connection fails before the task is spawned.
    pub async fn connect(host: &Host, event_tx: mpsc::Sender<CoreEvent>) -> anyhow::Result<Self> {
        let backend: Box<dyn FileTransferBackend> = match host.file_access {
            FileAccess::Sftp => Box::new(SftpBackend::connect(host).await?),
            FileAccess::Ftp | FileAccess::Ftps => Box::new(FtpBackend::connect(host).await?),
            FileAccess::None => {
                anyhow::bail!("'{}' has file access disabled", host.name)
            }
        };

        let (cmd_tx, cmd_rx) = mpsc::channel::<SftpCommand>(64);
        let host_name = host.name.clone();

        // If the task panics, Rust's unwind machinery drops `backend` (and
        // with it the underlying connection) before the panic propagates to
        // tokio — no explicit catch_unwind needed.
        tokio::spawn(async move {
            let _ = event_tx
                .send(CoreEvent::SftpConnected {
                    host_name: host_name.clone(),
                })
                .await;
            task_loop(backend, cmd_rx, event_tx.clone()).await;
            tracing::info!("File transfer task for '{}' exited", host_name);
        });

        Ok(Self { cmd_tx })
    }

    /// Enqueues a command (fire-and-forget). Silently drops if the task exited.
    pub fn send(&self, cmd: SftpCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    /// Sends [`SftpCommand::Disconnect`] and drops the sender.
    pub fn disconnect(self) {
        let _ = self.cmd_tx.try_send(SftpCommand::Disconnect);
    }
}

// ---------------------------------------------------------------------------
// Background task loop — generic over the active FileTransferBackend
// ---------------------------------------------------------------------------

async fn task_loop(
    mut backend: Box<dyn FileTransferBackend>,
    mut cmd_rx: mpsc::Receiver<SftpCommand>,
    event_tx: mpsc::Sender<CoreEvent>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            SftpCommand::ListDir(path) => match backend.list_dir(&path).await {
                Ok(entries) => {
                    let _ = event_tx
                        .send(CoreEvent::FileDirListed { path, entries })
                        .await;
                }
                Err(e) => {
                    let _ = event_tx
                        .send(CoreEvent::SftpDisconnected {
                            reason: format!("ListDir failed: {e}"),
                        })
                        .await;
                }
            },

            SftpCommand::Download {
                remote,
                local,
                transfer_id,
            } => {
                let result = backend
                    .download(&remote, &local, transfer_id, &event_tx)
                    .await
                    .map_err(|e| e.to_string());
                let _ = event_tx.send(CoreEvent::SftpOpDone { result }).await;
            }

            SftpCommand::Upload {
                local,
                remote,
                transfer_id,
            } => {
                let result = backend
                    .upload(&local, &remote, transfer_id, &event_tx)
                    .await
                    .map_err(|e| e.to_string());
                let _ = event_tx.send(CoreEvent::SftpOpDone { result }).await;
            }

            SftpCommand::Delete(path) => {
                let result = backend.delete(&path).await.map_err(|e| e.to_string());
                let _ = event_tx.send(CoreEvent::SftpOpDone { result }).await;
            }

            SftpCommand::MkDir(path) => {
                let result = backend.mkdir(&path).await.map_err(|e| e.to_string());
                let _ = event_tx.send(CoreEvent::SftpOpDone { result }).await;
            }

            SftpCommand::Rename { from, to } => {
                let result = backend.rename(&from, &to).await.map_err(|e| e.to_string());
                let _ = event_tx.send(CoreEvent::SftpOpDone { result }).await;
            }

            SftpCommand::ReadPreview(path) => {
                if let Ok(content) = backend.read_preview(&path).await {
                    let _ = event_tx
                        .send(CoreEvent::FilePreviewReady { path, content })
                        .await;
                }
            }

            SftpCommand::Disconnect => break,
        }
    }
}

// ---------------------------------------------------------------------------
// SftpBackend — SFTP-over-SSH implementation of FileTransferBackend
// ---------------------------------------------------------------------------

/// SFTP implementation of [`FileTransferBackend`]. Holds the SSH session
/// alive for as long as the SFTP channel needs it.
struct SftpBackend {
    _ssh: SshSession,
    sftp: russh_sftp::client::SftpSession,
}

impl SftpBackend {
    async fn connect(host: &Host) -> anyhow::Result<Self> {
        let ssh = SshSession::connect(host).await.context("SFTP SSH connect")?;
        let stream = ssh.open_sftp_channel().await.context("open SFTP channel")?;
        let sftp = russh_sftp::client::SftpSession::new(stream)
            .await
            .context("create SFTP session")?;
        Ok(Self { _ssh: ssh, sftp })
    }
}

#[async_trait]
impl FileTransferBackend for SftpBackend {
    async fn list_dir(&mut self, path: &str) -> anyhow::Result<Vec<FileEntry>> {
        let read_dir = self
            .sftp
            .read_dir(path)
            .await
            .with_context(|| format!("read remote dir '{path}'"))?;

        let mut entries: Vec<FileEntry> = Vec::new();

        // ".." parent entry (omit at root "/")
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

        for entry in read_dir {
            let name = entry.file_name();
            let ft = entry.file_type();
            let meta = entry.metadata();

            let full_path = if path.ends_with('/') {
                format!("{path}{name}")
            } else {
                format!("{path}/{name}")
            };

            entries.push(FileEntry {
                name,
                path: full_path,
                size: meta.size.unwrap_or(0),
                is_dir: ft.is_dir(),
            });
        }

        // Sort: ".." first, then dirs, then files — all alphabetically.
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

        // Fetch size for progress (best-effort).
        let total = self
            .sftp
            .metadata(remote)
            .await
            .map(|m| m.size.unwrap_or(0))
            .unwrap_or(0);

        let mut remote_file = self
            .sftp
            .open(remote)
            .await
            .context("open remote file for download")?;
        let mut local_file = tokio::fs::File::create(local)
            .await
            .context("create local file")?;

        let mut buf = vec![0u8; 65_536];
        let mut done: u64 = 0;

        loop {
            let n = remote_file
                .read(&mut buf)
                .await
                .context("read remote file")?;
            if n == 0 {
                break;
            }
            local_file
                .write_all(&buf[..n])
                .await
                .context("write local file")?;
            done += n as u64;
            let _ = event_tx
                .send(CoreEvent::FileTransferProgress(transfer_id, done, total))
                .await;
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

        let mut remote_file = self
            .sftp
            .create(remote)
            .await
            .context("create remote file for upload")?;

        let mut buf = vec![0u8; 65_536];
        let mut done: u64 = 0;

        loop {
            let n = local_file.read(&mut buf).await.context("read local file")?;
            if n == 0 {
                break;
            }
            remote_file
                .write_all(&buf[..n])
                .await
                .context("write remote file")?;
            done += n as u64;
            let _ = event_tx
                .send(CoreEvent::FileTransferProgress(transfer_id, done, total))
                .await;
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        // Try remove_file first; on failure try remove_dir (empty dirs only).
        match self.sftp.remove_file(path).await {
            Ok(()) => Ok(()),
            Err(_) => self
                .sftp
                .remove_dir(path)
                .await
                .map_err(|e| anyhow::anyhow!("delete '{path}': {e}")),
        }
    }

    async fn mkdir(&mut self, path: &str) -> anyhow::Result<()> {
        self.sftp
            .create_dir(path)
            .await
            .with_context(|| format!("mkdir '{path}'"))
    }

    async fn rename(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.sftp
            .rename(from, to)
            .await
            .with_context(|| format!("rename '{from}' -> '{to}'"))
    }

    async fn read_preview(&mut self, path: &str) -> anyhow::Result<String> {
        let mut file = self.sftp.open(path).await.context("open for preview")?;
        let mut buf = vec![0u8; 4_096];
        let n = file.read(&mut buf).await.context("read preview bytes")?;
        buf.truncate(n);
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

// ---------------------------------------------------------------------------
// Local filesystem helpers (called via inline tokio::spawn in App)
// ---------------------------------------------------------------------------

/// Lists the entries of a local directory, sorted dirs-first then alphabetically.
///
/// Prepends a `".."` entry for the parent directory (omitted at filesystem root).
///
/// # Errors
/// Returns an error if the directory cannot be read (e.g. permission denied).
pub async fn list_local_dir(path: &str) -> anyhow::Result<Vec<FileEntry>> {
    let mut read_dir = tokio::fs::read_dir(path)
        .await
        .with_context(|| format!("read local dir '{path}'"))?;

    let mut entries: Vec<FileEntry> = Vec::new();

    // ".." parent entry.
    if let Some(parent) = std::path::Path::new(path).parent() {
        let parent_str = parent.to_string_lossy();
        let parent_str = if parent_str.is_empty() {
            "/"
        } else {
            &parent_str
        };
        entries.push(FileEntry {
            name: "..".to_string(),
            path: parent_str.to_string(),
            size: 0,
            is_dir: true,
        });
    }

    while let Some(entry) = read_dir
        .next_entry()
        .await
        .context("read local dir entry")?
    {
        let file_type = entry.file_type().await.ok();
        let is_dir = file_type.as_ref().map(|ft| ft.is_dir()).unwrap_or(false);
        let meta = entry.metadata().await.ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

        let name = entry.file_name().to_string_lossy().into_owned();
        let path_str = entry.path().to_string_lossy().into_owned();

        entries.push(FileEntry {
            name,
            path: path_str,
            size,
            is_dir,
        });
    }

    // Sort: ".." first, then dirs, then files — case-insensitive alphabetically.
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

/// Reads up to 4 096 bytes from a local file and returns them as a UTF-8 string.
///
/// Non-UTF-8 bytes are replaced with the Unicode replacement character.
///
/// # Errors
/// Returns an error if the file cannot be opened or read.
pub async fn preview_local_file(path: &str) -> anyhow::Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .context("open local file for preview")?;
    let mut buf = vec![0u8; 4_096];
    let n = file
        .read(&mut buf)
        .await
        .context("read local preview bytes")?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
