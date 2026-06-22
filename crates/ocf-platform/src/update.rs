//! Package-update model and the per-manager output parsers.
//!
//! The async `PackageManager` methods shell out to the host tool; the parsing of
//! that output lives here as **pure functions** so it can be unit-tested without a
//! package manager present. Each manager differs in how it reports updates and
//! whether it distinguishes *security* updates (apt and dnf do; pacman and apk
//! roll everything together).

use ocf_core::prelude::*;

/// An available update for one installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageUpdate {
    pub name: String,
    /// The currently installed version, when the tool reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    /// The version available to upgrade to.
    pub available_version: String,
    /// Whether this update comes from a security source/advisory.
    pub security: bool,
}

/// An installed package and its version — the input to vulnerability scanning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
}

/// Parse `apt list --upgradable` output. Lines look like:
/// `zlib1g/jammy-updates,jammy-security 1.2-3 amd64 [upgradable from: 1.2-2]`.
/// A suite component containing `security` marks a security update.
pub fn parse_apt_upgradable(stdout: &str) -> Vec<PackageUpdate> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        // Skip the "Listing..." header and blanks.
        if line.is_empty() || !line.contains('/') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(name_suite) = parts.next() else { continue };
        let Some((name, suite)) = name_suite.split_once('/') else { continue };
        let Some(available_version) = parts.next() else { continue };
        let security = suite.to_ascii_lowercase().contains("security");
        // `[upgradable from: <ver>]` → current version, when present.
        let current_version = line
            .split_once("upgradable from:")
            .and_then(|(_, rest)| rest.trim().trim_end_matches(']').split_whitespace().next())
            .map(str::to_string);
        out.push(PackageUpdate {
            name: name.to_string(),
            current_version,
            available_version: available_version.to_string(),
            security,
        });
    }
    out
}

/// Parse `dnf -q list --upgrades` (`name.arch  version  repo`), marking a package
/// as a security update when its name is in `security_names` (from
/// `dnf updateinfo list security`).
pub fn parse_dnf_upgrades(stdout: &str, security_names: &[String]) -> Vec<PackageUpdate> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Last metadata") || line.starts_with("Available") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(name_arch) = parts.next() else { continue };
        let Some(version) = parts.next() else { continue };
        // Strip the `.arch` suffix to get the bare package name.
        let name = name_arch.rsplit_once('.').map(|(n, _)| n).unwrap_or(name_arch);
        if name.is_empty() || version.is_empty() {
            continue;
        }
        out.push(PackageUpdate {
            name: name.to_string(),
            current_version: None,
            available_version: version.to_string(),
            security: security_names.iter().any(|s| s == name),
        });
    }
    out
}

/// Extract the security-affected package names from `dnf updateinfo list security`
/// output (`ADVISORY  Severity/Type  package`).
pub fn parse_dnf_security_names(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let pkg = line.split_whitespace().last()?;
            // The package column looks like `name-version.arch`; take the name.
            // Heuristic: drop the trailing `-version...` once a digit run begins.
            if pkg.is_empty() || !line.to_ascii_lowercase().contains("sec") {
                return None;
            }
            Some(strip_nevra(pkg))
        })
        .collect()
}

/// Reduce an RPM `name-version-release.arch` (NEVRA) string to the package name —
/// everything before the last two `-`-delimited fields (version, release), so a
/// name that itself contains dashes (`java-11-openjdk`) survives.
fn strip_nevra(s: &str) -> String {
    let no_arch = s.rsplit_once('.').map(|(n, _)| n).unwrap_or(s);
    let mut fields = no_arch.rsplitn(3, '-');
    let _release = fields.next();
    let _version = fields.next();
    match fields.next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => no_arch.to_string(),
    }
}

/// Parse `pacman -Qu` / `checkupdates` (`name oldver -> newver`). Arch is rolling,
/// so there is no security/non-security distinction.
pub fn parse_pacman_updates(stdout: &str) -> Vec<PackageUpdate> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut p = line.split_whitespace();
            let name = p.next()?;
            let current = p.next()?;
            let _arrow = p.next()?; // "->"
            let new = p.next()?;
            Some(PackageUpdate {
                name: name.to_string(),
                current_version: Some(current.to_string()),
                available_version: new.to_string(),
                security: false,
            })
        })
        .collect()
}

/// Parse `apk version -l '<'` (`name-ver < newver`). apk does not flag security.
pub fn parse_apk_updates(stdout: &str) -> Vec<PackageUpdate> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (left, right) = line.split_once('<')?;
            let new = right.trim().split_whitespace().next()?;
            let pkgver = left.trim();
            // `name-version` → split at the last `-<digit>`.
            let (name, current) = split_name_version(pkgver)?;
            Some(PackageUpdate {
                name,
                current_version: Some(current),
                available_version: new.to_string(),
                security: false,
            })
        })
        .collect()
}

/// Parse `dpkg-query -W -f '${Package} ${Version}\n'`.
pub fn parse_dpkg_installed(stdout: &str) -> Vec<InstalledPackage> {
    line_pairs(stdout)
}

/// Parse `rpm -qa --qf '%{NAME} %{VERSION}\n'`.
pub fn parse_rpm_installed(stdout: &str) -> Vec<InstalledPackage> {
    line_pairs(stdout)
}

/// Parse `pacman -Q` (`name version`).
pub fn parse_pacman_installed(stdout: &str) -> Vec<InstalledPackage> {
    line_pairs(stdout)
}

/// Parse `apk info -v` (`name-version` per line) into installed packages.
pub fn parse_apk_installed(stdout: &str) -> Vec<InstalledPackage> {
    stdout
        .lines()
        .filter_map(|line| {
            split_name_version(line.trim()).map(|(name, version)| InstalledPackage { name, version })
        })
        .collect()
}

/// `name version` per line → installed packages.
fn line_pairs(stdout: &str) -> Vec<InstalledPackage> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut p = line.split_whitespace();
            let name = p.next()?;
            let version = p.next()?;
            (!name.is_empty() && !version.is_empty()).then(|| InstalledPackage {
                name: name.to_string(),
                version: version.to_string(),
            })
        })
        .collect()
}

/// Split a combined `name-version` (apk style) at the last `-` that starts a
/// version (a digit), since package names themselves may contain dashes.
fn split_name_version(s: &str) -> Option<(String, String)> {
    let idx = s.char_indices().rev().find_map(|(i, c)| {
        if c == '-' && s[i + 1..].chars().next().is_some_and(|d| d.is_ascii_digit()) {
            Some(i)
        } else {
            None
        }
    })?;
    Some((s[..idx].to_string(), s[idx + 1..].to_string()))
}

/// Whether a host package manager exposes a security-only update path.
pub fn supports_security_filter(manager: &str) -> bool {
    matches!(manager, "apt" | "dnf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apt_marks_security_suite() {
        let out = "Listing...\n\
            openssl/jammy-security 3.0.2-0ubuntu1.15 amd64 [upgradable from: 3.0.2-0ubuntu1.10]\n\
            vim/jammy-updates 2:8.2 amd64 [upgradable from: 2:8.1]\n";
        let ups = parse_apt_upgradable(out);
        assert_eq!(ups.len(), 2);
        let ssl = ups.iter().find(|u| u.name == "openssl").unwrap();
        assert!(ssl.security);
        assert_eq!(ssl.available_version, "3.0.2-0ubuntu1.15");
        assert_eq!(ssl.current_version.as_deref(), Some("3.0.2-0ubuntu1.10"));
        assert!(!ups.iter().find(|u| u.name == "vim").unwrap().security);
    }

    #[test]
    fn dnf_upgrades_with_security_set() {
        let list = "Last metadata expiration check...\n\
            kernel.x86_64    5.14.0-427.el9    baseos\n\
            curl.x86_64      7.76.1-29.el9     appstream\n";
        let sec = vec!["kernel".to_string()];
        let ups = parse_dnf_upgrades(list, &sec);
        assert_eq!(ups.len(), 2);
        assert!(ups.iter().find(|u| u.name == "kernel").unwrap().security);
        assert!(!ups.iter().find(|u| u.name == "curl").unwrap().security);
    }

    #[test]
    fn dnf_security_names_extracted() {
        let out = "RHSA-2024:1234 Important/Sec kernel-5.14.0-427.el9.x86_64\n\
                   RHSA-2024:5678 Moderate/Sec openssl-3.0.7-1.el9.x86_64\n";
        let names = parse_dnf_security_names(out);
        assert!(names.contains(&"kernel".to_string()));
        assert!(names.contains(&"openssl".to_string()));
    }

    #[test]
    fn pacman_and_apk_have_no_security_flag() {
        let pac = parse_pacman_updates("linux 6.9.1 -> 6.9.2\nfirefox 126.0 -> 126.0.1\n");
        assert_eq!(pac.len(), 2);
        assert!(pac.iter().all(|u| !u.security));
        assert_eq!(pac[0].available_version, "6.9.2");

        let apk = parse_apk_updates("musl-1.2.4-r0 < 1.2.4-r2\nbusybox-1.36.0 < 1.36.1\n");
        assert_eq!(apk.len(), 2);
        assert_eq!(apk[0].name, "musl");
        assert_eq!(apk[0].available_version, "1.2.4-r2");
    }

    #[test]
    fn installed_pairs_parse() {
        let dpkg = parse_dpkg_installed("bash 5.1-6ubuntu1\nopenssl 3.0.2-0ubuntu1.10\n");
        assert_eq!(dpkg.len(), 2);
        assert_eq!(dpkg[1], InstalledPackage { name: "openssl".into(), version: "3.0.2-0ubuntu1.10".into() });
    }
}
