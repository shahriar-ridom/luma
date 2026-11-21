//! Reading the results of filesystem checks. Pure parsing, no commands run
//! here. The commands themselves are fixed in [`super::privileged`], and every
//! one of them only reads: `fsck -n`, `xfs_repair -n`, a btrfs scrub.

use super::{devices::Volume, privileged::RootAction};

/// How to check a volume without changing it, or why it cannot be checked now.
pub fn check_action(v: &Volume) -> Result<RootAction, String> {
    let dev = || v.path.clone();
    match (v.fstype.as_str(), v.mounted()) {
        ("btrfs", true) => Ok(RootAction::BtrfsScrub { mount: v.mounts[0].clone() }),
        ("btrfs", false) => Err("Mount this drive first: a scrub checks a mounted btrfs filesystem.".into()),
        ("xfs", false) => Ok(RootAction::XfsRepairDry { dev: dev() }),
        ("ext2" | "ext3" | "ext4" | "vfat", false) => Ok(RootAction::FsCheck { dev: dev() }),
        ("xfs" | "ext2" | "ext3" | "ext4" | "vfat", true) => {
            Err("Unmount this drive first: it is not safe to check while it is in use.".into())
        }
        (other, _) if !v.checkable() => Err(format!("There is no filesystem to check ({other}).")),
        (other, _) => Err(format!("Checking {other} filesystems is not supported.")),
    }
}

/// Read a finished check, whichever tool produced it.
pub fn parse_result(action: &RootAction, code: Option<i32>, out: &str, err: &str) -> Option<CheckResult> {
    Some(match action {
        RootAction::BtrfsScrub { .. } => parse_btrfs_scrub(code, out, err),
        RootAction::FsCheck { .. } => parse_fsck(code, out, err),
        RootAction::XfsRepairDry { .. } => parse_xfs_repair(code, out, err),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    /// The check ran and found damage.
    Problems,
    /// The check could not complete.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    pub verdict: Verdict,
    pub summary: String,
    /// The tail of the tool's own output, for the curious.
    pub details: String,
}

/// Last lines of output, enough to see what went wrong without a wall of text.
fn tail(out: &str, err: &str) -> String {
    let all: Vec<&str> = out.lines().chain(err.lines()).map(str::trim_end).filter(|l| !l.is_empty()).collect();
    all[all.len().saturating_sub(12)..].join("\n")
}

fn result(verdict: Verdict, summary: &str, out: &str, err: &str) -> CheckResult {
    CheckResult { verdict, summary: summary.to_string(), details: tail(out, err) }
}

/// `btrfs scrub start -B`: waits for the end and prints a summary.
pub fn parse_btrfs_scrub(code: Option<i32>, out: &str, err: &str) -> CheckResult {
    let field = |name: &str| out.lines().find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim().to_string()));
    let status = field("Status:").unwrap_or_default();
    let errors = field("Error summary:");
    let clean_summary = errors.as_deref().is_some_and(|e| e.eq_ignore_ascii_case("no errors found"));
    if matches!(status.as_str(), "aborted" | "interrupted") {
        return result(Verdict::Failed, "The scrub was stopped before it finished.", out, err);
    }
    // Exit status 3 means uncorrectable errors, even if the summary is missing.
    if code == Some(3) || (errors.is_some() && !clean_summary) {
        let found = errors.filter(|_| !clean_summary).map(|e| format!(": {e}")).unwrap_or_default();
        return result(Verdict::Problems, &format!("Scrub found errors{found}"), out, err);
    }
    match code {
        Some(0) if clean_summary => result(Verdict::Clean, "Scrub finished: no errors found.", out, err),
        Some(0) => result(Verdict::Clean, "Scrub finished without errors.", out, err),
        _ => result(Verdict::Failed, "The scrub could not be completed.", out, err),
    }
}

/// `fsck -n`: exit status is a bit mask.
pub fn parse_fsck(code: Option<i32>, out: &str, err: &str) -> CheckResult {
    match code {
        Some(0) => result(Verdict::Clean, "No problems found.", out, err),
        Some(c) if c & 4 != 0 => {
            result(Verdict::Problems, "Errors were found. Run a repair from a rescue system.", out, err)
        }
        Some(c) if c & 1 != 0 => result(Verdict::Problems, "Errors were found.", out, err),
        Some(c) if c & 32 != 0 => result(Verdict::Failed, "The check was cancelled.", out, err),
        _ => result(Verdict::Failed, "The check could not be completed.", out, err),
    }
}

/// `xfs_repair -n`: 0 clean, 1 damage found, 2 the log is dirty.
pub fn parse_xfs_repair(code: Option<i32>, out: &str, err: &str) -> CheckResult {
    match code {
        Some(0) => result(Verdict::Clean, "No problems found.", out, err),
        Some(1) => result(Verdict::Problems, "Damage was found.", out, err),
        Some(2) => result(
            Verdict::Failed,
            "The journal has unfinished changes. Mount the drive once so they are applied, then check again.",
            out,
            err,
        ),
        _ => result(Verdict::Failed, "The check could not be completed.", out, err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRUB_OK: &str = "Starting scrub on devid 1\nscrub done for 1b2c\nScrub started:    Sat Nov 21 10:00:00 2026\nStatus:           finished\nDuration:         0:00:12\nTotal to scrub:   10.00GiB\nError summary:    no errors found\n";
    const SCRUB_BAD: &str = "scrub done for 1b2c\nStatus:           finished\nError summary:    read=2 csum=5\n  Corrected:      4\n  Uncorrectable:  3\n";

    #[test]
    fn btrfs_scrub_outcomes() {
        assert_eq!(parse_btrfs_scrub(Some(0), SCRUB_OK, "").verdict, Verdict::Clean);
        let bad = parse_btrfs_scrub(Some(3), SCRUB_BAD, "");
        assert_eq!(bad.verdict, Verdict::Problems);
        assert!(bad.summary.contains("read=2 csum=5"), "{}", bad.summary);
        // Exit 3 alone is enough, even when the summary line is missing.
        assert_eq!(parse_btrfs_scrub(Some(3), "", "").verdict, Verdict::Problems);
        let stopped = parse_btrfs_scrub(Some(1), "Status:           aborted\n", "");
        assert_eq!(stopped.verdict, Verdict::Failed);
        assert_eq!(parse_btrfs_scrub(Some(1), "", "ERROR: not a btrfs filesystem").verdict, Verdict::Failed);
        assert_eq!(parse_btrfs_scrub(None, "", "").verdict, Verdict::Failed);
    }

    #[test]
    fn fsck_exit_mask() {
        assert_eq!(parse_fsck(Some(0), "clean", "").verdict, Verdict::Clean);
        assert_eq!(parse_fsck(Some(4), "", "").verdict, Verdict::Problems);
        assert_eq!(parse_fsck(Some(5), "", "").verdict, Verdict::Problems);
        assert_eq!(parse_fsck(Some(8), "", "").verdict, Verdict::Failed);
        assert_eq!(parse_fsck(Some(32), "", "").verdict, Verdict::Failed);
        assert_eq!(parse_fsck(None, "", "").verdict, Verdict::Failed);
    }

    #[test]
    fn xfs_exit_codes() {
        assert_eq!(parse_xfs_repair(Some(0), "", "").verdict, Verdict::Clean);
        assert_eq!(parse_xfs_repair(Some(1), "", "").verdict, Verdict::Problems);
        let dirty = parse_xfs_repair(Some(2), "", "");
        assert_eq!(dirty.verdict, Verdict::Failed);
        assert!(dirty.summary.contains("Mount the drive"));
        assert_eq!(parse_xfs_repair(Some(127), "", "").verdict, Verdict::Failed);
    }

    fn vol(fstype: &str, mounts: &[&str]) -> Volume {
        Volume {
            path: "/dev/sdb1".into(),
            fstype: fstype.into(),
            size: 1,
            mounts: mounts.iter().map(|m| m.to_string()).collect(),
        }
    }

    #[test]
    fn picks_a_read_only_check_that_suits_the_state() {
        let dev = |a: RootAction| matches!(a, RootAction::FsCheck { dev } | RootAction::XfsRepairDry { dev } if dev == "/dev/sdb1");
        assert!(dev(check_action(&vol("ext4", &[])).unwrap()));
        assert!(dev(check_action(&vol("vfat", &[])).unwrap()));
        assert!(dev(check_action(&vol("xfs", &[])).unwrap()));
        assert_eq!(
            check_action(&vol("btrfs", &["/mnt/a", "/mnt/b"])).unwrap(),
            RootAction::BtrfsScrub { mount: "/mnt/a".into() }
        );
        // Never touch a filesystem that is in use, except btrfs scrub, which is made for that.
        assert!(check_action(&vol("ext4", &["/mnt/x"])).unwrap_err().contains("Unmount"));
        assert!(check_action(&vol("xfs", &["/"])).is_err());
        assert!(check_action(&vol("btrfs", &[])).unwrap_err().contains("Mount"));
        assert!(check_action(&vol("crypto_LUKS", &[])).unwrap_err().contains("no filesystem"));
        assert!(check_action(&vol("ntfs", &[])).unwrap_err().contains("not supported"));
    }

    #[test]
    fn results_are_read_by_the_right_parser() {
        let scrub = RootAction::BtrfsScrub { mount: "/m".into() };
        assert_eq!(parse_result(&scrub, Some(0), SCRUB_OK, "").unwrap().verdict, Verdict::Clean);
        let fsck = RootAction::FsCheck { dev: "/dev/sdb1".into() };
        assert_eq!(parse_result(&fsck, Some(4), "", "").unwrap().verdict, Verdict::Problems);
        assert!(parse_result(&RootAction::CoredumpsPrune, Some(0), "", "").is_none());
    }

    #[test]
    fn details_keep_only_the_tail() {
        let out: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let r = parse_fsck(Some(0), &out, "warn");
        let lines: Vec<_> = r.details.lines().collect();
        assert_eq!(lines.len(), 12);
        assert_eq!(*lines.last().unwrap(), "warn");
        assert_eq!(lines[0], "line 89");
    }
}
