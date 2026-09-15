//! [`FileTransferBackend`] — the protocol-agnostic seam behind the Files tab.
//!
//! [`crate::ssh::sftp::SftpManager`] talks to whichever backend a host's
//! [`crate::ssh::client::FileAccess`] selects (SFTP today; FTP/FTPS in
//! [`crate::ssh::ftp`]) through this trait, so the command loop, events, and
//! every frontend stay unaware of which wire protocol is actually in use.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::event::{CoreEvent, TransferId};
use crate::ssh::sftp::FileEntry;

/// Remote file operations needed by the Files tab, independent of transport.
#[async_trait]
pub trait FileTransferBackend: Send {
    /// Lists the entries of a remote directory (dirs first, then files,
    /// case-insensitively alphabetical; a `".."` parent entry included except
    /// at root — mirrors [`crate::ssh::sftp::list_local_dir`]'s convention).
    async fn list_dir(&mut self, path: &str) -> anyhow::Result<Vec<FileEntry>>;

    /// Downloads `remote` to `local`, sending [`CoreEvent::FileTransferProgress`]
    /// on `event_tx` as bytes arrive.
    async fn download(
        &mut self,
        remote: &str,
        local: &str,
        transfer_id: TransferId,
        event_tx: &mpsc::Sender<CoreEvent>,
    ) -> anyhow::Result<()>;

    /// Uploads `local` to `remote`, sending [`CoreEvent::FileTransferProgress`]
    /// on `event_tx` as bytes are sent.
    async fn upload(
        &mut self,
        local: &str,
        remote: &str,
        transfer_id: TransferId,
        event_tx: &mpsc::Sender<CoreEvent>,
    ) -> anyhow::Result<()>;

    /// Deletes a remote file, falling back to removing an empty directory.
    async fn delete(&mut self, path: &str) -> anyhow::Result<()>;

    /// Creates a remote directory.
    async fn mkdir(&mut self, path: &str) -> anyhow::Result<()>;

    /// Renames / moves a remote path.
    async fn rename(&mut self, from: &str, to: &str) -> anyhow::Result<()>;

    /// Reads the first bytes of a remote file for the preview pane.
    async fn read_preview(&mut self, path: &str) -> anyhow::Result<String>;
}

/// Rejects a `local` path that could escape the intended directory (`..`) or
/// either path carrying an embedded NUL byte. Shared by every backend's
/// download/upload before they touch the local filesystem.
pub(crate) fn guard_local_path(local: &str, remote: &str) -> anyhow::Result<()> {
    if std::path::Path::new(local)
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        anyhow::bail!("local path contains '..': {local}");
    }
    if local.contains('\0') || remote.contains('\0') {
        anyhow::bail!("path contains null bytes");
    }
    Ok(())
}
