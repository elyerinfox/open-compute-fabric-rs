//! Host operating-system detection and binary-availability probing.

use ocf_core::prelude::*;
use std::path::Path;

/// A snapshot of the host's operating system, enough to choose a package
/// manager and decide which capabilities are even applicable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostOs {
    /// The Rust target OS string: `"linux"`, `"windows"`, `"macos"`, …
    pub os: String,
    /// The distro id from `/etc/os-release` `ID=` (e.g. `"ubuntu"`); empty when
    /// not a Linux distro or the file is absent.
    pub distro: String,
    /// `ID_LIKE=` tokens (e.g. `["debian"]` for Ubuntu) — used so a derivative
    /// distro inherits its parent's package manager.
    pub id_like: Vec<String>,
    /// `PRETTY_NAME=` for display, e.g. `"Ubuntu 24.04 LTS"`.
    pub pretty: String,
}

impl HostOs {
    /// Detect the host OS. On Linux this reads `/etc/os-release` (falling back to
    /// `/usr/lib/os-release`); on other platforms only `os` is meaningful.
    pub fn detect() -> Self {
        let os = std::env::consts::OS.to_string();
        let mut host = HostOs {
            os,
            distro: String::new(),
            id_like: Vec::new(),
            pretty: String::new(),
        };
        if host.os == "linux" {
            let text = std::fs::read_to_string("/etc/os-release")
                .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
                .unwrap_or_default();
            let (distro, id_like, pretty) = parse_os_release(&text);
            host.distro = distro;
            host.id_like = id_like;
            host.pretty = pretty;
        }
        host
    }

    pub fn is_linux(&self) -> bool {
        self.os == "linux"
    }

    /// Whether this host's distro *is* `id` or is *like* it (covers derivatives).
    pub fn matches(&self, id: &str) -> bool {
        self.distro.eq_ignore_ascii_case(id)
            || self.id_like.iter().any(|l| l.eq_ignore_ascii_case(id))
    }
}

/// Parse the `ID`, `ID_LIKE`, and `PRETTY_NAME` fields out of os-release content.
/// Values may be quoted; `ID_LIKE` is space-separated.
pub fn parse_os_release(text: &str) -> (String, Vec<String>, String) {
    let mut distro = String::new();
    let mut id_like = Vec::new();
    let mut pretty = String::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').to_string();
        match key.trim() {
            "ID" => distro = value,
            "ID_LIKE" => {
                id_like = value
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
            }
            "PRETTY_NAME" => pretty = value,
            _ => {}
        }
    }
    (distro, id_like, pretty)
}

/// Whether an executable named `name` is available on `PATH`. This is a probe —
/// it does **not** run the program. Cross-platform: splits `PATH` on the OS
/// separator and, on Windows, also tries the `.exe`/`.bat`/`.cmd` extensions.
pub fn binary_available(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for cand in candidate_filenames(name) {
            if Path::new(&dir).join(&cand).is_file() {
                return true;
            }
        }
    }
    false
}

/// The filenames to look for in a PATH directory for executable `name`. On
/// Windows this includes the common executable extensions.
fn candidate_filenames(name: &str) -> Vec<String> {
    if cfg!(windows) {
        let mut v = vec![name.to_string()];
        for ext in ["exe", "bat", "cmd"] {
            v.push(format!("{name}.{ext}"));
        }
        v
    } else {
        vec![name.to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ubuntu_os_release() {
        let text = r#"
NAME="Ubuntu"
ID=ubuntu
ID_LIKE=debian
PRETTY_NAME="Ubuntu 24.04 LTS"
VERSION_ID="24.04"
"#;
        let (distro, like, pretty) = parse_os_release(text);
        assert_eq!(distro, "ubuntu");
        assert_eq!(like, vec!["debian".to_string()]);
        assert_eq!(pretty, "Ubuntu 24.04 LTS");
    }

    #[test]
    fn parses_rhel_multi_id_like() {
        let text = "ID=\"centos\"\nID_LIKE=\"rhel fedora\"\n";
        let (distro, like, _) = parse_os_release(text);
        assert_eq!(distro, "centos");
        assert_eq!(like, vec!["rhel".to_string(), "fedora".to_string()]);
    }

    #[test]
    fn matches_distro_and_id_like() {
        let host = HostOs {
            os: "linux".into(),
            distro: "ubuntu".into(),
            id_like: vec!["debian".into()],
            pretty: String::new(),
        };
        assert!(host.matches("ubuntu"));
        assert!(host.matches("debian")); // via ID_LIKE
        assert!(host.matches("DEBIAN")); // case-insensitive
        assert!(!host.matches("arch"));
    }

    #[test]
    fn candidate_filenames_match_platform() {
        let c = candidate_filenames("nft");
        assert!(c.contains(&"nft".to_string()));
        if cfg!(windows) {
            assert!(c.contains(&"nft.exe".to_string()));
        }
    }
}
