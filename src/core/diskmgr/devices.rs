//! Physical disks and their filesystems, from `lsblk --json`.
//!
//! Also the one place that decides what counts as a block device or mount
//! point worth handing to an administrator command.

use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

use super::{CmdSpec, ToolSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
    pub path: String,
    pub model: String,
    pub size: u64,
    pub rotational: bool,
    pub removable: bool,
    pub transport: Option<String>,
    pub volumes: Vec<Volume>,
}

/// Something on a disk that holds a filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub path: String,
    pub fstype: String,
    pub size: u64,
    pub mounts: Vec<String>,
}

impl Volume {
    pub fn mounted(&self) -> bool {
        !self.mounts.is_empty()
    }

    /// Swap and container formats have nothing to check.
    pub fn checkable(&self) -> bool {
        !matches!(self.fstype.as_str(), "swap" | "crypto_LUKS" | "LVM2_member" | "linux_raid_member" | "")
    }
}

impl Disk {
    pub fn name(&self) -> String {
        if self.model.is_empty() { self.path.clone() } else { self.model.clone() }
    }
}

#[derive(Deserialize)]
struct Lsblk {
    blockdevices: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    path: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    size: Option<u64>,
    model: Option<String>,
    fstype: Option<String>,
    #[serde(default)]
    mountpoints: Vec<Option<String>>,
    rm: Option<bool>,
    rota: Option<bool>,
    tran: Option<String>,
    #[serde(default)]
    children: Vec<Node>,
}

pub fn lsblk_cmd(tools: &ToolSet) -> Option<CmdSpec> {
    let mut argv = vec![tools.path("lsblk")?.to_string_lossy().into_owned()];
    argv.extend(
        ["--json", "--bytes", "--output", "NAME,PATH,TYPE,SIZE,MODEL,FSTYPE,MOUNTPOINTS,RM,ROTA,TRAN"]
            .iter()
            .map(|s| s.to_string()),
    );
    Some(CmdSpec::user(argv))
}

fn volume(n: &Node) -> Option<Volume> {
    let v = Volume {
        path: n.path.clone()?,
        fstype: n.fstype.clone()?,
        size: n.size.unwrap_or(0),
        mounts: n.mountpoints.iter().flatten().filter(|m| m.starts_with('/')).cloned().collect(),
    };
    Some(v)
}

fn collect(n: &Node, out: &mut Vec<Volume>) {
    out.extend(volume(n));
    n.children.iter().for_each(|c| collect(c, out));
}

/// Real disks only: RAM disks (zram), loop devices and optical drives are left out.
pub fn parse_lsblk(json: &str) -> Result<Vec<Disk>, String> {
    let l: Lsblk = serde_json::from_str(json).map_err(|e| format!("unreadable lsblk output: {e}"))?;
    Ok(l.blockdevices
        .iter()
        .filter(|n| n.kind.as_deref() == Some("disk"))
        .filter_map(|n| {
            let path = n.path.clone().filter(|p| is_block_path(p))?;
            let mut volumes = vec![];
            collect(n, &mut volumes);
            Some(Disk {
                path,
                model: n.model.as_deref().map(str::trim).unwrap_or_default().to_string(),
                size: n.size.unwrap_or(0),
                rotational: n.rota.unwrap_or(false),
                removable: n.rm.unwrap_or(false),
                transport: n.tran.clone(),
                volumes,
            })
        })
        .collect())
}

static BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/dev/(?:(?:sd|vd|xvd)[a-z]{1,3}[0-9]{0,3}|hd[a-z][0-9]{0,3}|nvme[0-9]{1,3}n[0-9]{1,3}(?:p[0-9]{1,3})?|mmcblk[0-9]{1,3}(?:p[0-9]{1,3})?)$")
        .expect("valid regex")
});

/// A disk or partition node of a kind we know how to manage.
pub fn is_block_path(p: &str) -> bool {
    BLOCK.is_match(p)
}

/// An absolute path with no empty, `.` or `..` parts without control characters.
pub fn is_mount_path(p: &str) -> bool {
    p.starts_with('/')
        && !p.chars().any(char::is_control)
        && (p == "/" || p[1..].split('/').all(|s| !s.is_empty() && s != "." && s != ".."))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSBLK: &str = r#"{"blockdevices":[
      {"path":"/dev/sda","type":"disk","size":1000204886016,"model":"Patriot P210 ","fstype":null,"mountpoints":[null],
       "rm":false,"rota":false,"tran":"sata","children":[
         {"path":"/dev/sda1","type":"part","size":1000203091968,"model":null,"fstype":"ext4","mountpoints":["/mnt/storage"],"rm":false,"rota":false,"tran":null}]},
      {"path":"/dev/nvme0n1","type":"disk","size":500107862016,"model":"Samsung SSD 980","fstype":null,"mountpoints":[null],
       "rm":false,"rota":false,"tran":"nvme","children":[
         {"path":"/dev/nvme0n1p1","type":"part","size":536870912,"fstype":"vfat","mountpoints":["/boot"]},
         {"path":"/dev/nvme0n1p2","type":"part","size":499000000000,"fstype":"crypto_LUKS","mountpoints":[null],"children":[
            {"path":"/dev/mapper/root","type":"crypt","size":498000000000,"fstype":"btrfs","mountpoints":["/","/home"]}]}]},
      {"path":"/dev/zram0","type":"disk","size":29064429568,"fstype":"swap","mountpoints":["[SWAP]"]},
      {"path":"/dev/loop0","type":"loop","size":1000,"fstype":"squashfs","mountpoints":["/snap/x"]},
      {"path":"/dev/sr0","type":"rom","size":1073741312,"model":"DVD","fstype":null,"mountpoints":[null]}
    ]}"#;

    #[test]
    fn lists_real_disks_with_their_volumes() {
        let d = parse_lsblk(LSBLK).unwrap();
        let paths: Vec<_> = d.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["/dev/sda", "/dev/nvme0n1"], "zram, loop and optical are skipped");
        assert_eq!(d[0].model, "Patriot P210");
        assert_eq!(d[0].transport.as_deref(), Some("sata"));
        assert_eq!(d[0].volumes.len(), 1);
        assert_eq!(d[0].volumes[0].mounts, ["/mnt/storage"]);

        let nv = &d[1];
        let v: Vec<_> = nv.volumes.iter().map(|v| (v.path.as_str(), v.fstype.as_str(), v.mounted())).collect();
        assert_eq!(
            v,
            [
                ("/dev/nvme0n1p1", "vfat", true),
                ("/dev/nvme0n1p2", "crypto_LUKS", false),
                ("/dev/mapper/root", "btrfs", true)
            ]
        );
        assert!(!nv.volumes[1].checkable(), "a LUKS container has no filesystem to check");
        assert!(nv.volumes[2].checkable());
    }

    #[test]
    fn the_command_asks_for_a_tree() {
        // Without the NAME column lsblk prints a flat list and partitions stop
        // being children of their disk.
        use std::{fs, os::unix::fs::PermissionsExt};
        let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-lsblk");
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("lsblk"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("lsblk"), fs::Permissions::from_mode(0o755)).unwrap();
        let t = ToolSet::detect_in(&[d], &["lsblk"]);
        let cmd = lsblk_cmd(&t).unwrap();
        assert!(!cmd.privileged);
        assert!(cmd.argv.iter().any(|a| a.split(',').next() == Some("NAME")), "{:?}", cmd.argv);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_lsblk("not json").is_err());
        assert!(parse_lsblk("{}").is_err());
        assert!(parse_lsblk(r#"{"blockdevices":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn block_paths_are_strict() {
        for ok in ["/dev/sda", "/dev/sda1", "/dev/nvme0n1", "/dev/nvme0n1p2", "/dev/mmcblk0p1", "/dev/vdb"] {
            assert!(is_block_path(ok), "{ok}");
        }
        for bad in [
            "/dev/sda; rm -rf /",
            "/dev/sda ",
            "/dev/sda\n",
            "/dev/../etc/passwd",
            "/dev/sda/../sdb",
            "sda",
            "/dev/",
            "/dev/null",
            "/dev/mapper/root",
            "/tmp/sda",
            "/dev/sda1p",
        ] {
            assert!(!is_block_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn mount_paths_are_normalised() {
        assert!(is_mount_path("/"));
        assert!(is_mount_path("/mnt/my disk"));
        for bad in ["", "mnt", "/mnt/../etc", "/mnt/./x", "/mnt/a\nb", "/mnt//x", "/mnt/x/"] {
            assert!(!is_mount_path(bad), "{bad:?}");
        }
    }
}
