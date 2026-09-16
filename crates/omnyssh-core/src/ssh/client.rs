//! SSH connection management.
//!
//! Connections delegated to the system SSH binary.
//! Also provides russh-based client for live metrics.

use serde::{Deserialize, Serialize};

/// Indicates where a host entry originated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HostSource {
    /// Imported from `~/.ssh/config` at startup.
    SshConfig,
    /// Added manually through the TUI form.
    #[default]
    Manual,
}

/// How a host is watched on the dashboard.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MonitorMode {
    /// A live SSH session with shell metrics — the default.
    #[default]
    Ssh,
    /// A plain TCP connect, with no login. For devices that answer SSH but have
    /// no POSIX shell to collect metrics from, such as network appliances.
    // The wire and the TUI form spell this differently; accept both, because a
    // rejected value fails the whole file and takes every manual host with it.
    #[serde(alias = "tcp", alias = "tcpPort")]
    TcpPort,
}

impl MonitorMode {
    /// Lets the default stay out of `hosts.toml` entirely.
    fn is_ssh(&self) -> bool {
        matches!(self, Self::Ssh)
    }
}

/// How a host's Files tab connects for file transfer.
///
/// Independent of the shell connection: some devices (routers, NAS boxes,
/// embedded Linux) answer SSH for a shell but have no SFTP subsystem, and
/// only expose plain FTP for file access.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileAccess {
    /// SFTP over the same SSH connection used for the shell — the default.
    #[default]
    Sftp,
    /// Plain FTP.
    Ftp,
    /// FTP with explicit TLS (`AUTH TLS`, RFC 4217). Implicit FTPS (port 990)
    /// is deprecated and not supported.
    Ftps,
    /// No file transfer for this host; the Files tab is disabled.
    None,
}

impl FileAccess {
    /// Lets the default stay out of `hosts.toml` entirely.
    fn is_sftp(&self) -> bool {
        matches!(self, Self::Sftp)
    }
}

/// A single host entry used for SSH connections.
///
/// Populated either from `~/.ssh/config` (via the parser) or from
/// `~/.config/omnyssh/hosts.toml` (manual entries added through the TUI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    /// Display name / alias (e.g. `"web-prod-1"`).
    pub name: String,
    /// Hostname or IP address to connect to.
    pub hostname: String,
    /// SSH user. Defaults to `"root"` when not specified.
    #[serde(default = "default_user")]
    pub user: String,
    /// SSH port. Defaults to `22` when not specified.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Path to the private key file (e.g. `~/.ssh/id_ed25519`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// Password for password-based authentication (not recommended, used for initial setup).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// ProxyJump host alias (for bastion / jump-host setups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_jump: Option<String>,
    /// Organisational tags (e.g. `["production", "web"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Free-text notes about this host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Where this entry came from.
    #[serde(default)]
    pub source: HostSource,
    /// Original host name from `~/.ssh/config` if this host was renamed.
    /// Used to prevent duplicate entries when a SSH-config host is renamed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_ssh_host: Option<String>,
    /// How this host is watched.
    #[serde(default, skip_serializing_if = "MonitorMode::is_ssh")]
    pub monitoring: MonitorMode,
    /// Port for the reachability probe. Falls back to `port` when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_port: Option<u16>,
    /// How the Files tab connects for this host.
    #[serde(default, skip_serializing_if = "FileAccess::is_sftp")]
    pub file_access: FileAccess,
    /// FTP login override — only consulted when `file_access` is `Ftp`/`Ftps`.
    /// Falls back to `user` when unset (a device with a separate FTP account
    /// needs this; most don't).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ftp_user: Option<String>,
    /// FTP password override. Falls back to `password` when unset; FTP still
    /// requires *some* password even then, since SSH key auth has no FTP
    /// equivalent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ftp_password: Option<String>,
    /// FTP port override. Falls back to the standard FTP port (21) when
    /// unset — never to `port`, which is the SSH port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ftp_port: Option<u16>,

    // -----------------------------------------------------------------------
    // Auto SSH Key Setup metadata
    // -----------------------------------------------------------------------
    /// Date when SSH key was configured by OmnySSH (ISO 8601 format).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_setup_date: Option<String>,
    /// Whether password authentication has been disabled on the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_auth_disabled: Option<bool>,
}

fn default_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| String::from("root"))
}

fn default_port() -> u16 {
    22
}

impl Default for Host {
    fn default() -> Self {
        Self {
            name: String::new(),
            hostname: String::new(),
            user: default_user(),
            port: default_port(),
            identity_file: None,
            password: None,
            proxy_jump: None,
            tags: Vec::new(),
            notes: None,
            source: HostSource::default(),
            original_ssh_host: None,
            monitoring: MonitorMode::default(),
            monitor_port: None,
            file_access: FileAccess::default(),
            ftp_user: None,
            ftp_password: None,
            ftp_port: None,
            key_setup_date: None,
            password_auth_disabled: None,
        }
    }
}

/// Runtime connection status for a host. Never serialised to disk.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ConnectionStatus {
    /// No connection attempt has been made yet.
    #[default]
    Unknown,
    /// A connection attempt is currently in progress.
    Connecting,
    /// The host is connected.
    Connected,
    /// The last connection attempt failed with the given message.
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An existing `hosts.toml` predates the monitoring fields, and writing one
    /// back must not add them — the file has to stay byte-identical for a host
    /// nobody changed.
    #[test]
    fn the_monitoring_default_round_trips_without_touching_the_file() {
        let host: Host = toml::from_str("name = \"web\"\nhostname = \"10.0.0.1\"\n")
            .expect("a host without the monitoring fields still parses");
        assert_eq!(host.monitoring, MonitorMode::Ssh);
        assert_eq!(host.monitor_port, None);

        let written = toml::to_string(&host).expect("serialize");
        assert!(!written.contains("monitoring"), "{written}");
        assert!(!written.contains("monitor_port"), "{written}");
    }

    /// Same guarantee as the monitoring default, for `file_access`: existing
    /// files predate it and must round-trip without gaining the field.
    #[test]
    fn the_file_access_default_round_trips_without_touching_the_file() {
        let host: Host = toml::from_str("name = \"web\"\nhostname = \"10.0.0.1\"\n")
            .expect("a host without file_access still parses");
        assert_eq!(host.file_access, FileAccess::Sftp);

        let written = toml::to_string(&host).expect("serialize");
        assert!(!written.contains("file_access"), "{written}");
    }

    /// A host whose Files tab was pointed at plain FTP persists that choice.
    #[test]
    fn a_non_default_file_access_persists() {
        let host = Host {
            name: String::from("nas"),
            hostname: String::from("10.0.0.5"),
            file_access: FileAccess::Ftp,
            ..Host::default()
        };

        let written = toml::to_string(&host).expect("serialize");
        let read: Host = toml::from_str(&written).expect("deserialize");
        assert_eq!(read.file_access, FileAccess::Ftp);
    }

    /// The FTP override fields stay out of `hosts.toml` when unset, and persist
    /// when a host actually needs a separate FTP login.
    #[test]
    fn ftp_overrides_round_trip_and_stay_out_when_unset() {
        let host: Host = toml::from_str("name = \"web\"\nhostname = \"10.0.0.1\"\n")
            .expect("a host without FTP overrides still parses");
        assert_eq!(host.ftp_user, None);
        assert_eq!(host.ftp_password, None);
        assert_eq!(host.ftp_port, None);
        let written = toml::to_string(&host).expect("serialize");
        assert!(!written.contains("ftp_"), "{written}");

        let overridden = Host {
            name: String::from("nas"),
            hostname: String::from("10.0.0.5"),
            file_access: FileAccess::Ftp,
            ftp_user: Some(String::from("ftpuser")),
            ftp_password: Some(String::from("ftppass")),
            ftp_port: Some(2121),
            ..Host::default()
        };
        let written = toml::to_string(&overridden).expect("serialize");
        let read: Host = toml::from_str(&written).expect("deserialize");
        assert_eq!(read.ftp_user.as_deref(), Some("ftpuser"));
        assert_eq!(read.ftp_password.as_deref(), Some("ftppass"));
        assert_eq!(read.ftp_port, Some(2121));
    }

    /// The wire and the TUI form spell the mode differently, so a hand-edited
    /// file is likely to carry either — and a rejected value fails the whole
    /// file, taking every manual host with it.
    #[test]
    fn the_other_spellings_of_the_mode_are_accepted() {
        for spelling in ["tcp_port", "tcp", "tcpPort"] {
            let host: Host = toml::from_str(&format!(
                "name = \"fw\"\nhostname = \"10.0.0.9\"\nmonitoring = \"{spelling}\"\n"
            ))
            .unwrap_or_else(|e| panic!("'{spelling}' should parse: {e}"));
            assert_eq!(host.monitoring, MonitorMode::TcpPort);
        }
    }

    #[test]
    fn a_reachability_host_persists_its_mode_and_probe_port() {
        let host = Host {
            name: String::from("fw"),
            hostname: String::from("10.0.0.9"),
            monitoring: MonitorMode::TcpPort,
            monitor_port: Some(8443),
            ..Host::default()
        };

        let written = toml::to_string(&host).expect("serialize");
        let read: Host = toml::from_str(&written).expect("deserialize");

        assert_eq!(read.monitoring, MonitorMode::TcpPort);
        assert_eq!(read.monitor_port, Some(8443));
    }
}
