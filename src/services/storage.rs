//! Filesystem capacity and device information.

use gtk::gio::{self, prelude::*};
use gtk::glib;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct FsInfo {
    pub size: u64,
    pub free: u64,
    pub used: u64,
    pub fs_type: String,
    pub readonly: bool,
}

impl FsInfo {
    pub fn used_fraction(&self) -> f64 {
        if self.size == 0 { 0.0 } else { (self.used as f64 / self.size as f64).clamp(0.0, 1.0) }
    }
}

pub async fn fs_info(file: &gio::File) -> Option<FsInfo> {
    let i = file
        .query_filesystem_info_future(
            "filesystem::size,filesystem::free,filesystem::used,filesystem::type,filesystem::readonly",
            glib::Priority::LOW,
        )
        .await
        .ok()?;
    let size = i.attribute_uint64("filesystem::size");
    let free = i.attribute_uint64("filesystem::free");
    let used = if i.has_attribute("filesystem::used") {
        i.attribute_uint64("filesystem::used")
    } else {
        size.saturating_sub(free)
    };
    Some(FsInfo {
        size,
        free,
        used,
        fs_type: i.attribute_string("filesystem::type").map(|s| s.to_string()).unwrap_or_default(),
        readonly: i.boolean("filesystem::readonly"),
    })
}

pub async fn free_space(path: &Path) -> Option<u64> {
    fs_info(&gio::File::for_path(path)).await.map(|i| i.free)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcMount {
    pub source: String,
    pub target: String,
    pub fstype: String,
}

fn unescape(s: &str) -> String {
    // /proc escapes space, tab, newline and backslash as \ooo.
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `/proc/self/mountinfo`.
pub fn parse_mountinfo(text: &str) -> Vec<ProcMount> {
    text.lines()
        .filter_map(|l| {
            let (head, tail) = l.split_once(" - ")?;
            let target = head.split_whitespace().nth(4)?;
            let mut t = tail.split_whitespace();
            let fstype = t.next()?;
            let source = t.next()?;
            Some(ProcMount { source: unescape(source), target: unescape(target), fstype: fstype.to_string() })
        })
        .collect()
}

pub fn proc_mounts() -> Vec<ProcMount> {
    std::fs::read_to_string("/proc/self/mountinfo").map(|t| parse_mountinfo(&t)).unwrap_or_default()
}

#[derive(Clone)]
pub struct MountDetail {
    pub mount: gio::Mount,
    pub name: String,
    pub root: Option<std::path::PathBuf>,
    pub device: Option<String>,
    pub fs_type: String,
    pub info: Option<FsInfo>,
    pub removable: bool,
    pub uuid: Option<String>,
}

/// Every user-visible mount with capacity and device details.
pub async fn mount_details() -> Vec<MountDetail> {
    let procs = proc_mounts();
    let mut out = Vec::new();
    for m in gio::VolumeMonitor::get().mounts() {
        if m.is_shadowed() {
            continue;
        }
        let root_file = m.root();
        let root = root_file.path();
        let info = fs_info(&root_file).await;
        let p = root.as_ref().and_then(|r| procs.iter().rev().find(|p| Path::new(&p.target) == r.as_path()));
        let fs_type = info
            .as_ref()
            .map(|i| i.fs_type.clone())
            .filter(|t| !t.is_empty())
            .or_else(|| p.map(|p| p.fstype.clone()))
            .unwrap_or_default();
        out.push(MountDetail {
            name: m.name().to_string(),
            device: p.map(|p| p.source.clone()).filter(|s| s.starts_with('/')),
            removable: m.drive().is_some_and(|d| d.is_removable()) || m.can_eject(),
            uuid: m.uuid().map(|u| u.to_string()).or_else(|| m.volume().and_then(|v| v.uuid()).map(|u| u.to_string())),
            mount: m,
            root,
            fs_type,
            info,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_parsing_with_escapes_and_optional_fields() {
        let t = "36 35 98:0 /mnt1 /mnt/my\\040disk rw,noatime master:1 - ext4 /dev/sda1 rw,errors=continue\n\
                 25 1 0:23 / /run/user/1000 rw,nosuid shared:1 - tmpfs tmpfs rw\n\
                 garbage line\n";
        let m = parse_mountinfo(t);
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[0],
            ProcMount { source: "/dev/sda1".into(), target: "/mnt/my disk".into(), fstype: "ext4".into() }
        );
        assert_eq!(m[1].fstype, "tmpfs");
    }

    #[test]
    fn used_fraction_clamps() {
        let i = FsInfo { size: 100, used: 150, ..Default::default() };
        assert_eq!(i.used_fraction(), 1.0);
        assert_eq!(FsInfo::default().used_fraction(), 0.0);
    }
}
