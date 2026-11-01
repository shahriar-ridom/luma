//! Google Drive (and other rclone "drive" remotes) as a native folder.
//!
//! We never handle Google credentials ourselves: `rclone` runs the OAuth
//! sign-in in the browser and keeps the token in its own config. The drive
//! is then mounted with FUSE under our data directory, so every view,
//! copy/paste and drag-and-drop works on it like a local folder.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use crate::services::storage::parse_mountinfo;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub kind: String,
}

/// Where drives are mounted. Content search and thumbnails skip this tree so
/// browsing never triggers mass downloads.
pub fn base_dir() -> PathBuf {
    gtk::glib::user_data_dir().join("file-manager").join("cloud")
}

pub fn mount_point(name: &str) -> PathBuf {
    base_dir().join(name)
}

/// Remote names are used in a path and on a command line: keep them boring.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn rclone() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("rclone")).find(|f| f.is_file()))
}

pub fn parse_listremotes(out: &str) -> Vec<Remote> {
    out.lines()
        .filter_map(|l| {
            let (name, kind) = l.split_once(':')?;
            Some(Remote { name: name.trim().to_string(), kind: kind.trim().to_string() })
        })
        .filter(|r| valid_name(&r.name))
        .collect()
}

pub fn create_args(name: &str, read_only: bool) -> Vec<String> {
    ["config", "create", name, "drive", "scope", if read_only { "drive.readonly" } else { "drive" }]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn mount_args(name: &str, dir: &Path) -> Vec<String> {
    vec![
        "mount".into(),
        format!("{name}:"),
        dir.to_string_lossy().into_owned(),
        "--vfs-cache-mode".into(),
        "full".into(),
        "--dir-cache-time".into(),
        "5m".into(),
        "--daemon".into(),
    ]
}

pub fn is_mounted_in(mountinfo: &str, dir: &Path) -> bool {
    parse_mountinfo(mountinfo).iter().any(|m| Path::new(&m.target) == dir && m.fstype.contains("rclone"))
}

pub fn is_mounted(name: &str) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|t| is_mounted_in(&t, &mount_point(name)))
}

fn run(mut cmd: Command, cancel: &AtomicBool, timeout: Duration) -> Result<(), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not start {:?}: {e}", cmd.get_program()))?;
    let mut err = child.stderr.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let stderr = reader.join().unwrap_or_default();
                return if st.success() {
                    Ok(())
                } else {
                    Err(stderr.lines().last().unwrap_or("failed").trim().to_string())
                };
            }
            Ok(None) => {
                if cancel.load(Ordering::Relaxed) || start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(if cancel.load(Ordering::Relaxed) {
                        "Cancelled".into()
                    } else {
                        "Timed out waiting for sign-in".into()
                    });
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

pub fn list_drive_remotes() -> Vec<Remote> {
    let Some(r) = rclone() else { return vec![] };
    Command::new(r)
        .args(["listremotes", "--long"])
        .stdin(Stdio::null())
        .output()
        .map(|o| {
            parse_listremotes(&String::from_utf8_lossy(&o.stdout)).into_iter().filter(|r| r.kind == "drive").collect()
        })
        .unwrap_or_default()
}

/// Sign in with Google (rclone opens the browser) and save the remote.
pub fn create_remote(name: &str, read_only: bool, cancel: &AtomicBool) -> Result<(), String> {
    if !valid_name(name) {
        return Err("Use letters, numbers, - and _ only".into());
    }
    let rc = rclone().ok_or("rclone is not installed")?;
    let mut cmd = Command::new(rc);
    cmd.args(create_args(name, read_only));
    run(cmd, cancel, Duration::from_secs(300))
}

/// Mount (if needed) and return the folder to browse.
pub fn mount(name: &str, cancel: &AtomicBool) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err("Invalid drive name".into());
    }
    let dir = mount_point(name);
    if is_mounted(name) {
        return Ok(dir);
    }
    let rc = rclone().ok_or("rclone is not installed")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut cmd = Command::new(rc);
    cmd.args(mount_args(name, &dir));
    run(cmd, cancel, Duration::from_secs(60))?;
    for _ in 0..50 {
        if is_mounted(name) {
            return Ok(dir);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("The drive did not finish mounting. Is FUSE (fuse3) available?".into())
}

pub fn unmount(name: &str) -> Result<(), String> {
    let dir = mount_point(name);
    for tool in ["fusermount3", "fusermount"] {
        match Command::new(tool).arg("-u").arg(&dir).stdin(Stdio::null()).output() {
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => return Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(_) => continue,
        }
    }
    Err("fusermount is not installed".into())
}

/// Forget the account (removes the saved token from rclone's config).
pub fn delete_remote(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("Invalid drive name".into());
    }
    let _ = unmount(name);
    let rc = rclone().ok_or("rclone is not installed")?;
    let mut cmd = Command::new(rc);
    cmd.args(["config", "delete", name]);
    run(cmd, &AtomicBool::new(false), Duration::from_secs(20))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remotes_and_rejects_odd_names() {
        let out = "gdrive: drive\nwork-drive: drive\nnas: sftp\nbad name: drive\n; weird: drive\n";
        let r = parse_listremotes(out);
        assert_eq!(r.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["gdrive", "work-drive", "nas"]);
        assert_eq!(r.iter().filter(|r| r.kind == "drive").count(), 2);
        assert!(valid_name("my_drive-2") && !valid_name("a b") && !valid_name("../x") && !valid_name(""));
    }

    #[test]
    fn command_lines() {
        assert_eq!(create_args("g", true), ["config", "create", "g", "drive", "scope", "drive.readonly"]);
        assert_eq!(create_args("g", false).last().unwrap(), "drive");
        let m = mount_args("g", Path::new("/data/cloud/g"));
        assert_eq!(&m[..3], ["mount", "g:", "/data/cloud/g"]);
        assert!(m.contains(&"--daemon".to_string()));
    }

    #[test]
    fn detects_mount_from_mountinfo() {
        let t = "50 1 0:45 / /home/me/.local/share/file-manager/cloud/g rw,nosuid - fuse.rclone g: rw,user_id=1000\n";
        assert!(is_mounted_in(t, Path::new("/home/me/.local/share/file-manager/cloud/g")));
        assert!(!is_mounted_in(t, Path::new("/home/me/.local/share/file-manager/cloud/other")));
        let plain = "50 1 0:45 / /home/me/.local/share/file-manager/cloud/g rw - ext4 /dev/sda1 rw\n";
        assert!(
            !is_mounted_in(plain, Path::new("/home/me/.local/share/file-manager/cloud/g")),
            "must be an rclone mount"
        );
    }
}
