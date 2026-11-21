//! Administrator actions. A closed list: nothing outside it can be run as
//! root, because the pkexec runner accepts a command only if it equals the
//! one rebuilt from a [`RootAction`] (see [`RootAction::from_spec`]).

use super::{
    CmdSpec, ToolSet,
    devices::{is_block_path, is_mount_path},
    distro::PackageManager,
};

/// Journal size limits accepted for a vacuum, in MB.
const JOURNAL_MB: std::ops::RangeInclusive<u32> = 1..=102_400;

pub const COREDUMP_DIR: &str = "/var/lib/systemd/coredump";
/// Coredumps older than this are removed.
pub const COREDUMP_DAYS: u32 = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootAction {
    PkgCacheClean(PackageManager),
    JournalVacuum {
        max_mb: u32,
    },
    CoredumpsPrune,
    /// Read the drive's SMART data (needs root to open the device).
    SmartRead {
        dev: String,
    },
    /// Start a short self-test; the drive runs it by itself.
    SmartSelfTest {
        dev: String,
    },
    /// Read-only `fsck -n` of an unmounted device.
    FsCheck {
        dev: String,
    },
    /// Read-only `xfs_repair -n` of an unmounted device.
    XfsRepairDry {
        dev: String,
    },
    /// Foreground scrub of a mounted btrfs filesystem (reads and verifies).
    BtrfsScrub {
        mount: String,
    },
}

const ALL_PMS: [PackageManager; 6] = [
    PackageManager::Pacman,
    PackageManager::Apt,
    PackageManager::Dnf,
    PackageManager::Zypper,
    PackageManager::Apk,
    PackageManager::Xbps,
];

/// Program and fixed arguments that clear a package manager's download cache.
pub fn pkg_command(pm: PackageManager) -> Option<(&'static str, &'static [&'static str])> {
    Some(match pm {
        // Keeps the three most recent versions of every package.
        PackageManager::Pacman => ("paccache", &["-r"]),
        PackageManager::Apt => ("apt-get", &["clean"]),
        PackageManager::Dnf => ("dnf", &["clean", "packages"]),
        PackageManager::Zypper => ("zypper", &["clean", "--all"]),
        PackageManager::Apk => ("apk", &["cache", "clean"]),
        PackageManager::Xbps => ("xbps-remove", &["-O"]),
        PackageManager::Portage | PackageManager::Unknown => return None,
    })
}

impl RootAction {
    /// Program name (looked up in the tool set) and its fixed arguments.
    pub fn command(&self) -> Option<(&'static str, Vec<String>)> {
        let strs = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let on_dev = |args: &[&str], dev: &str| {
            is_block_path(dev).then(|| {
                let mut v = strs(args);
                v.push(dev.to_string());
                v
            })
        };
        match self {
            Self::PkgCacheClean(pm) => pkg_command(*pm).map(|(p, a)| (p, a.iter().map(|s| s.to_string()).collect())),
            Self::JournalVacuum { max_mb } => {
                JOURNAL_MB.contains(max_mb).then(|| ("journalctl", vec![format!("--vacuum-size={max_mb}M")]))
            }
            Self::CoredumpsPrune => Some((
                "find",
                [COREDUMP_DIR, "-xdev", "-type", "f", "-mtime"]
                    .iter()
                    .map(|s| s.to_string())
                    .chain([format!("+{COREDUMP_DAYS}"), "-delete".to_string()])
                    .collect(),
            )),
            Self::SmartRead { dev } => on_dev(&["--json=c", "-a"], dev).map(|a| ("smartctl", a)),
            Self::SmartSelfTest { dev } => on_dev(&["-t", "short"], dev).map(|a| ("smartctl", a)),
            Self::FsCheck { dev } => on_dev(&["-n"], dev).map(|a| ("fsck", a)),
            Self::XfsRepairDry { dev } => on_dev(&["-n"], dev).map(|a| ("xfs_repair", a)),
            Self::BtrfsScrub { mount } => is_mount_path(mount)
                .then(|| ("btrfs", strs(&["scrub", "start", "-B"]).into_iter().chain([mount.clone()]).collect())),
        }
    }

    /// The command with the program resolved to the absolute path found at
    /// detection time. `None` if the tool is not installed.
    pub fn spec(&self, tools: &ToolSet) -> Option<CmdSpec> {
        let (prog, args) = self.command()?;
        let path = tools.path(prog)?;
        let mut argv = vec![path.to_string_lossy().into_owned()];
        argv.extend(args);
        Some(CmdSpec { argv, privileged: true })
    }

    /// Which action `spec` is, if it is exactly one of ours.
    pub fn from_spec(spec: &CmdSpec, tools: &ToolSet) -> Option<Self> {
        if !spec.privileged {
            return None;
        }
        // The only variable part of any command is its last argument.
        let last = spec.argv.last().cloned().unwrap_or_default();
        let journal = spec
            .argv
            .get(1)
            .and_then(|a| a.strip_prefix("--vacuum-size="))
            .and_then(|a| a.strip_suffix('M'))
            .and_then(|n| n.parse::<u32>().ok())
            .map(|max_mb| Self::JournalVacuum { max_mb });
        ALL_PMS
            .iter()
            .map(|&pm| Self::PkgCacheClean(pm))
            .chain(journal)
            .chain([
                Self::CoredumpsPrune,
                Self::SmartRead { dev: last.clone() },
                Self::SmartSelfTest { dev: last.clone() },
                Self::FsCheck { dev: last.clone() },
                Self::XfsRepairDry { dev: last.clone() },
                Self::BtrfsScrub { mount: last },
            ])
            .find(|c| c.spec(tools).as_ref() == Some(spec))
    }
}

/// What the confirm dialog and previews show for an administrator command.
pub fn display(spec: &CmdSpec) -> String {
    format!("pkexec {}", spec.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    pub(crate) fn tools_with(name: &str, progs: &[&str]) -> ToolSet {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-priv").join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        for p in progs {
            fs::write(d.join(p), "#!/bin/sh\n").unwrap();
            fs::set_permissions(d.join(p), fs::Permissions::from_mode(0o755)).unwrap();
        }
        ToolSet::detect_in(
            &[d],
            &[
                "paccache",
                "apt-get",
                "dnf",
                "zypper",
                "apk",
                "xbps-remove",
                "journalctl",
                "find",
                "pkexec",
                "smartctl",
                "fsck",
                "xfs_repair",
                "btrfs",
            ],
        )
    }

    #[test]
    fn every_package_manager_has_a_fixed_command() {
        let t = tools_with("pm", &["paccache", "apt-get", "dnf", "zypper", "apk", "xbps-remove"]);
        for pm in ALL_PMS {
            let spec = RootAction::PkgCacheClean(pm).spec(&t).unwrap();
            assert!(spec.privileged && spec.argv[0].starts_with('/'), "{}", pm.name());
        }
        let apt = RootAction::PkgCacheClean(PackageManager::Apt).spec(&t).unwrap();
        assert_eq!(&apt.argv[1..], ["clean"]);
        assert!(RootAction::PkgCacheClean(PackageManager::Portage).spec(&t).is_none());
        assert!(RootAction::PkgCacheClean(PackageManager::Unknown).spec(&t).is_none());
    }

    #[test]
    fn missing_tool_gives_no_command() {
        let t = tools_with("none", &[]);
        assert!(RootAction::PkgCacheClean(PackageManager::Pacman).spec(&t).is_none());
        assert!(RootAction::CoredumpsPrune.spec(&t).is_none());
    }

    #[test]
    fn journal_limit_is_validated() {
        let t = tools_with("journal", &["journalctl"]);
        let ok = RootAction::JournalVacuum { max_mb: 500 }.spec(&t).unwrap();
        assert_eq!(ok.argv[1], "--vacuum-size=500M");
        assert!(RootAction::JournalVacuum { max_mb: 0 }.spec(&t).is_none());
        assert!(RootAction::JournalVacuum { max_mb: 200_000 }.spec(&t).is_none());
    }

    #[test]
    fn coredump_command_is_pinned_to_its_folder() {
        let t = tools_with("core", &["find"]);
        let s = RootAction::CoredumpsPrune.spec(&t).unwrap();
        assert_eq!(&s.argv[1..], ["/var/lib/systemd/coredump", "-xdev", "-type", "f", "-mtime", "+7", "-delete"]);
    }

    #[test]
    fn only_exact_commands_are_recognised() {
        let t = tools_with("allow", &["paccache", "journalctl", "find", "apt-get"]);
        for a in [
            RootAction::PkgCacheClean(PackageManager::Pacman),
            RootAction::JournalVacuum { max_mb: 750 },
            RootAction::CoredumpsPrune,
        ] {
            let spec = a.spec(&t).unwrap();
            assert_eq!(RootAction::from_spec(&spec, &t), Some(a));

            let mut extra = spec.clone();
            extra.argv.push("--everything".into());
            assert_eq!(RootAction::from_spec(&extra, &t), None, "extra argument");

            let mut other = spec.clone();
            other.argv[0] = "/tmp/evil".into();
            assert_eq!(RootAction::from_spec(&other, &t), None, "different program");

            let mut unpriv = spec.clone();
            unpriv.privileged = false;
            assert_eq!(RootAction::from_spec(&unpriv, &t), None, "not marked privileged");
        }
        let rm = CmdSpec { argv: vec!["/usr/bin/rm".into(), "-rf".into(), "/".into()], privileged: true };
        assert_eq!(RootAction::from_spec(&rm, &t), None);
        let big = CmdSpec {
            argv: vec![t.path("journalctl").unwrap().to_string_lossy().into_owned(), "--vacuum-size=0M".into()],
            privileged: true,
        };
        assert_eq!(RootAction::from_spec(&big, &t), None, "out-of-range journal size");
    }

    #[test]
    fn device_actions_have_fixed_read_only_commands() {
        let t = tools_with("dev", &["smartctl", "fsck", "xfs_repair", "btrfs"]);
        let dev = |s: &str| s.to_string();
        let argv = |a: RootAction| a.spec(&t).unwrap().argv[1..].to_vec();
        assert_eq!(argv(RootAction::SmartRead { dev: dev("/dev/sda") }), ["--json=c", "-a", "/dev/sda"]);
        assert_eq!(argv(RootAction::SmartSelfTest { dev: dev("/dev/nvme0n1") }), ["-t", "short", "/dev/nvme0n1"]);
        assert_eq!(argv(RootAction::FsCheck { dev: dev("/dev/sdb1") }), ["-n", "/dev/sdb1"]);
        assert_eq!(argv(RootAction::XfsRepairDry { dev: dev("/dev/sdb2") }), ["-n", "/dev/sdb2"]);
        assert_eq!(argv(RootAction::BtrfsScrub { mount: dev("/mnt/data") }), ["scrub", "start", "-B", "/mnt/data"]);
    }

    #[test]
    fn device_actions_reject_anything_but_a_real_device_or_mount() {
        let t = tools_with("devbad", &["smartctl", "fsck", "xfs_repair", "btrfs"]);
        for bad in ["/dev/sda; rm -rf /", "/dev/sda ", "../dev/sda", "/dev/../etc/passwd", "sda", "--help", "-n", ""] {
            let d = bad.to_string();
            for a in [
                RootAction::SmartRead { dev: d.clone() },
                RootAction::SmartSelfTest { dev: d.clone() },
                RootAction::FsCheck { dev: d.clone() },
                RootAction::XfsRepairDry { dev: d },
            ] {
                assert!(a.spec(&t).is_none(), "{a:?}");
            }
        }
        for bad in ["", "mnt", "/mnt/../etc", "-x", "/a\nb"] {
            assert!(RootAction::BtrfsScrub { mount: bad.to_string() }.spec(&t).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn device_commands_are_recognised_only_when_exact() {
        let t = tools_with("devallow", &["smartctl", "fsck", "xfs_repair", "btrfs"]);
        for a in [
            RootAction::SmartRead { dev: "/dev/sda".into() },
            RootAction::SmartSelfTest { dev: "/dev/sda".into() },
            RootAction::FsCheck { dev: "/dev/sdb1".into() },
            RootAction::XfsRepairDry { dev: "/dev/sdb1".into() },
            RootAction::BtrfsScrub { mount: "/mnt/data".into() },
        ] {
            let spec = a.spec(&t).unwrap();
            assert_eq!(RootAction::from_spec(&spec, &t), Some(a.clone()));
            // Dropping the read-only flag or adding one is not one of ours.
            let mut stripped = spec.clone();
            if stripped.argv.len() > 2 {
                stripped.argv.remove(1);
                assert_eq!(RootAction::from_spec(&stripped, &t), None, "{a:?} without its flags");
            }
            let mut extra = spec.clone();
            extra.argv.insert(1, "--repair".into());
            assert_eq!(RootAction::from_spec(&extra, &t), None, "{a:?} with an extra flag");
        }
        // A repair is not on the list.
        let fix = CmdSpec {
            argv: vec![t.path("fsck").unwrap().to_string_lossy().into_owned(), "-y".into(), "/dev/sdb1".into()],
            privileged: true,
        };
        assert_eq!(RootAction::from_spec(&fix, &t), None);
        let repair = CmdSpec {
            argv: vec![t.path("xfs_repair").unwrap().to_string_lossy().into_owned(), "/dev/sdb1".into()],
            privileged: true,
        };
        assert_eq!(RootAction::from_spec(&repair, &t), None, "xfs_repair without -n would modify the disk");
    }

    #[test]
    fn display_shows_pkexec() {
        let s = CmdSpec { argv: vec!["/usr/bin/apt-get".into(), "clean".into()], privileged: true };
        assert_eq!(display(&s), "pkexec /usr/bin/apt-get clean");
    }
}
