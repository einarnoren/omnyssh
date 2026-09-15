//! Manual smoke test against a real public FTP server, to check the
//! `FtpBackend` wiring independent of the TUI/GUI. Not run by default
//! (network + external service): `cargo test -p omnyssh-core --test ftp_smoke -- --ignored --nocapture`.

use omnyssh_core::ssh::backend::FileTransferBackend;
use omnyssh_core::ssh::client::{FileAccess, Host};
use omnyssh_core::ssh::ftp::FtpBackend;

#[tokio::test]
#[ignore]
async fn connects_lists_and_downloads_from_a_public_ftp_server() {
    let host = Host {
        name: "rebex-smoke-test".to_string(),
        hostname: "test.rebex.net".to_string(),
        user: "demo".to_string(),
        password: Some("password".to_string()),
        file_access: FileAccess::Ftp,
        ..Host::default()
    };

    let mut backend = FtpBackend::connect(&host)
        .await
        .expect("connect + login to test.rebex.net");

    let entries = backend
        .list_dir("/")
        .await
        .expect("list the root directory");
    println!("entries: {entries:#?}");
    assert!(
        entries.iter().any(|e| e.name == "readme.txt"),
        "expected the well-known readme.txt on test.rebex.net"
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let local_path = std::env::temp_dir().join("omnyssh_ftp_smoke_readme.txt");
    backend
        .download(
            "/readme.txt",
            local_path.to_str().unwrap(),
            1,
            &tx,
        )
        .await
        .expect("download readme.txt");
    drop(tx);
    while rx.recv().await.is_some() {}

    let content = std::fs::read_to_string(&local_path).expect("read downloaded file");
    println!("downloaded content:\n{content}");
    assert!(!content.is_empty());
    let _ = std::fs::remove_file(&local_path);
}

#[tokio::test]
#[ignore]
async fn connects_over_explicit_ftps_to_a_public_server() {
    let host = Host {
        name: "rebex-ftps-smoke-test".to_string(),
        hostname: "test.rebex.net".to_string(),
        user: "demo".to_string(),
        password: Some("password".to_string()),
        file_access: FileAccess::Ftps,
        ..Host::default()
    };

    let mut backend = FtpBackend::connect(&host)
        .await
        .expect("connect + AUTH TLS + login to test.rebex.net");

    let entries = backend
        .list_dir("/")
        .await
        .expect("list the root directory over FTPS");
    println!("entries: {entries:#?}");
    assert!(
        entries.iter().any(|e| e.name == "readme.txt"),
        "expected the well-known readme.txt on test.rebex.net"
    );
}
