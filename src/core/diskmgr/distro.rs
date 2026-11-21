//! Distro and package manager detection from `os-release`.

use std::fs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageManager {
    Pacman,
    Apt,
    Dnf,
    Zypper,
    Apk,
    Xbps,
    Portage,
    Unknown,
}

impl PackageManager {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pacman => "pacman",
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Zypper => "zypper",
            Self::Apk => "apk",
            Self::Xbps => "xbps",
            Self::Portage => "portage",
            Self::Unknown => "unknown",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "arch" | "cachyos" | "manjaro" | "endeavouros" | "garuda" | "artix" => Self::Pacman,
            "debian" | "ubuntu" | "linuxmint" | "pop" | "raspbian" | "elementary" | "kali" => Self::Apt,
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "nobara" | "ol" => Self::Dnf,
            "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "sles" | "suse" => Self::Zypper,
            "alpine" | "postmarketos" => Self::Apk,
            "void" => Self::Xbps,
            "gentoo" => Self::Portage,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Distro {
    pub id: String,
    pub id_like: Vec<String>,
    pub name: String,
    pub pm: PackageManager,
}

impl Distro {
    /// Reads `/etc/os-release`, falling back to `/usr/lib/os-release`.
    pub fn detect() -> Self {
        let text = fs::read_to_string("/etc/os-release")
            .or_else(|_| fs::read_to_string("/usr/lib/os-release"))
            .unwrap_or_default();
        parse_os_release(&text)
    }
}

/// `ID` is tried first, then every `ID_LIKE` entry, so derivatives resolve
/// (CachyOS says `ID_LIKE=arch`). An unrecognised system gets `Unknown`
/// rather than a guess.
pub fn parse_os_release(text: &str) -> Distro {
    let (mut id, mut like, mut name) = (String::new(), String::new(), String::new());
    for line in text.lines() {
        let line = line.trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = unquote(v.trim());
        match k.trim() {
            "ID" => id = v.to_ascii_lowercase(),
            "ID_LIKE" => like = v.to_ascii_lowercase(),
            "PRETTY_NAME" => name = v,
            "NAME" if name.is_empty() => name = v,
            _ => {}
        }
    }
    let id_like: Vec<String> = like.split_whitespace().map(str::to_string).collect();
    let pm = std::iter::once(id.as_str())
        .chain(id_like.iter().map(String::as_str))
        .find_map(PackageManager::from_id)
        .unwrap_or(PackageManager::Unknown);
    if name.is_empty() {
        name = if id.is_empty() { "Linux".into() } else { id.clone() };
    }
    Distro { id, id_like, name, pm }
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return inner.to_string();
        }
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pm(s: &str) -> PackageManager {
        parse_os_release(s).pm
    }

    #[test]
    fn arch_and_derivatives() {
        assert_eq!(pm("ID=arch\nNAME=\"Arch Linux\""), PackageManager::Pacman);
        let d = parse_os_release("NAME=\"CachyOS Linux\"\nPRETTY_NAME=\"CachyOS\"\nID=cachyos\nID_LIKE=\"arch\"\n");
        assert_eq!(d.pm, PackageManager::Pacman);
        assert_eq!(d.name, "CachyOS");
        assert_eq!(d.id_like, vec!["arch"]);
    }

    #[test]
    fn id_like_fallback() {
        assert_eq!(pm("ID=zorin\nID_LIKE=\"ubuntu debian\""), PackageManager::Apt);
        assert_eq!(pm("ID=ubuntu\nID_LIKE=debian"), PackageManager::Apt);
    }

    #[test]
    fn other_families() {
        assert_eq!(pm("ID=fedora"), PackageManager::Dnf);
        assert_eq!(pm("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\""), PackageManager::Zypper);
        assert_eq!(pm("ID=alpine"), PackageManager::Apk);
        assert_eq!(pm("ID=void"), PackageManager::Xbps);
        assert_eq!(pm("ID=gentoo"), PackageManager::Portage);
    }

    #[test]
    fn unknown_and_empty_do_not_guess() {
        assert_eq!(pm("ID=templeos"), PackageManager::Unknown);
        let d = parse_os_release("");
        assert_eq!(d.pm, PackageManager::Unknown);
        assert_eq!(d.name, "Linux");
    }

    #[test]
    fn quotes_comments_and_garbage() {
        let d = parse_os_release("# comment\nnonsense\nID='debian'\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n");
        assert_eq!(d.id, "debian");
        assert_eq!(d.pm, PackageManager::Apt);
        assert!(d.name.starts_with("Debian"));
    }
}
