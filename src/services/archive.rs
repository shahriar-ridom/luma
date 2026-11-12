//! Archive support via libarchive's `bsdtar` (zip, tar.*, 7z, rar read, …).
//!
//! Extraction never overwrites: it always lands in a path that did not exist
//! (a new folder, or a single top-level entry that was absent). libarchive's
//! own defaults reject `..` components and unsafe symlinks.

use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("this archive is password protected")]
    NeedsPassphrase,
    #[error("cancelled")]
    Cancelled,
    #[error("bsdtar (libarchive) is not installed")]
    Missing,
    #[error("{0}")]
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarXz,
    TarZst,
    SevenZ,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Fast,
    Normal,
    Best,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Fast, Level::Normal, Level::Best];
    pub fn label(self) -> &'static str {
        match self {
            Level::Fast => "Fast",
            Level::Normal => "Normal",
            Level::Best => "Smallest",
        }
    }
}

impl Format {
    pub const ALL: [Format; 6] =
        [Format::Zip, Format::TarGz, Format::TarXz, Format::TarZst, Format::SevenZ, Format::Tar];
    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::TarXz => "tar.xz",
            Format::TarZst => "tar.zst",
            Format::SevenZ => "7z",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "ZIP (.zip)",
            Format::Tar => "TAR (.tar)",
            Format::TarGz => "TAR + gzip (.tar.gz)",
            Format::TarXz => "TAR + xz (.tar.xz)",
            Format::TarZst => "TAR + zstd (.tar.zst)",
            Format::SevenZ => "7-Zip (.7z)",
        }
    }
    pub fn has_levels(self) -> bool {
        self != Format::Tar
    }
}

const SUFFIXES: &[&str] = &[
    ".tar.gz",
    ".tar.xz",
    ".tar.zst",
    ".tar.bz2",
    ".tar.lz4",
    ".tar.lzma",
    ".tgz",
    ".txz",
    ".tzst",
    ".tbz2",
    ".tar",
    ".zip",
    ".7z",
    ".rar",
    ".jar",
    ".apk",
    ".cab",
    ".iso",
];

pub fn is_archive(name: &str) -> bool {
    let n = name.to_lowercase();
    SUFFIXES.iter().any(|s| n.ends_with(s))
}

/// Archives that Enter / double-click should extract. Formats that have
/// their own meaning when opened (.jar, .apk, .iso, .cab) are left alone.
pub fn is_extractable_on_open(name: &str) -> bool {
    const OK: &[&str] = &[
        ".zip",
        ".tar",
        ".tar.gz",
        ".tgz",
        ".tar.xz",
        ".txz",
        ".tar.zst",
        ".tzst",
        ".tar.bz2",
        ".tbz2",
        ".tar.lz4",
        ".tar.lzma",
        ".7z",
        ".rar",
    ];
    let n = name.to_lowercase();
    OK.iter().any(|s| n.ends_with(s) && n.len() > s.len())
}

/// "photos.tar.gz" → "photos".
pub fn stem(name: &str) -> String {
    let n = name.to_lowercase();
    for s in SUFFIXES {
        if n.ends_with(s) && name.len() > s.len() {
            return name[..name.len() - s.len()].to_string();
        }
    }
    name.to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

fn bsdtar() -> Command {
    Command::new("bsdtar")
}

/// Start `cmd`, handing it the passphrase on stdin.
///
/// The passphrase must never be a command-line argument: every user on the
/// machine can read those. bsdtar asks for it on the terminal when it has one
/// and falls back to stdin when it has none, so the child gets its own
/// session (no controlling terminal) and a pipe. That also stops a Luma that
/// was started from a shell from waiting on that shell's terminal.
fn spawn_bsdtar(mut cmd: Command, passphrase: Option<&str>) -> Result<std::process::Child, ArchiveError> {
    if passphrase.is_some_and(|p| p.contains(['\n', '\r'])) {
        return Err(ArchiveError::Failed("a passphrase can’t contain a line break".into()));
    }
    cmd.stdin(if passphrase.is_some() { Stdio::piped() } else { Stdio::null() });
    // SAFETY: only `setsid`, which is async-signal-safe, runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(drop).map_err(std::io::Error::from));
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::Missing
        } else {
            ArchiveError::Failed(e.to_string())
        }
    })?;
    if let (Some(p), Some(mut stdin)) = (passphrase, child.stdin.take()) {
        // bsdtar may ask more than once (it retries on a wrong passphrase) or
        // never (unencrypted archive); a closed pipe is not an error.
        let _ = stdin.write_all(format!("{p}\n").as_bytes());
    }
    Ok(child)
}

fn classify(stderr: &str) -> ArchiveError {
    let l = stderr.to_lowercase();
    if l.contains("passphrase") || l.contains("encrypted") && l.contains("password") {
        ArchiveError::NeedsPassphrase
    } else {
        ArchiveError::Failed(stderr.lines().next().unwrap_or("archive operation failed").trim().to_string())
    }
}

/// Run to completion, polling `cancel`. stderr is drained on a thread so a
/// chatty tool can never block on a full pipe.
fn run(mut cmd: Command, passphrase: Option<&str>, cancel: &AtomicBool) -> Result<(), ArchiveError> {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = spawn_bsdtar(cmd, passphrase)?;
    let mut err = child.stderr.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = reader.join().unwrap_or_default();
                return if status.success() { Ok(()) } else { Err(classify(&stderr)) };
            }
            Ok(None) => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ArchiveError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(ArchiveError::Failed(e.to_string())),
        }
    }
}

pub fn list(archive: &Path, passphrase: Option<&str>) -> Result<Vec<Entry>, ArchiveError> {
    let mut cmd = bsdtar();
    cmd.arg("-tvf").arg(archive).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = spawn_bsdtar(cmd, passphrase)?.wait_with_output().map_err(|e| ArchiveError::Failed(e.to_string()))?;
    if !out.status.success() {
        return Err(classify(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().filter_map(parse_line).collect())
}

/// `-rw-r--r--  0 user group  1234 Jan  1 12:00 some dir/file name`
fn parse_line(line: &str) -> Option<Entry> {
    let mut it = line.split_whitespace();
    let perms = it.next()?;
    let (_links, _user, _group) = (it.next()?, it.next()?, it.next()?);
    let size: u64 = it.next()?.parse().ok()?;
    let (_m, _d, _t) = (it.next()?, it.next()?, it.next()?);
    // The path is whatever follows the 8th field; recover it by position.
    let mut rest = line;
    for _ in 0..8 {
        rest = rest.trim_start();
        rest = &rest[rest.find(char::is_whitespace)?..];
    }
    let mut path = rest.trim_start().to_string();
    if perms.starts_with('l')
        && let Some(i) = path.find(" -> ")
    {
        path.truncate(i); // symlink: keep only the entry name
    }
    let is_dir = perms.starts_with('d') || path.ends_with('/');
    Some(Entry { path: path.trim_end_matches('/').to_string(), size, is_dir })
}

fn unique_dir(parent: &Path, base: &str) -> PathBuf {
    let first = parent.join(base);
    if !first.exists() {
        return first;
    }
    (2..).map(|n| parent.join(format!("{base} {n}"))).find(|p| !p.exists()).unwrap()
}

/// A folder next to the extraction target that nothing uses yet; the archive
/// is unpacked here first, so a failed or cancelled extraction leaves nothing
/// behind. Same folder as the destination, so the final rename is atomic.
fn staging_dir(dest_parent: &Path) -> Result<PathBuf, ArchiveError> {
    let pid = std::process::id();
    for n in 0u32..1000 {
        let d = dest_parent.join(format!(".luma-extract-{pid}-{n}"));
        match std::fs::create_dir(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(ArchiveError::Failed(e.to_string())),
        }
    }
    Err(ArchiveError::Failed("could not create a temporary folder".into()))
}

/// Extract next to the archive without ever overwriting. The archive is
/// unpacked into a hidden staging folder; only when that succeeded does a lone
/// top-level entry that doesn't exist yet move into `dest_parent`, and
/// anything else become a fresh `<archive name>` folder. Returns the path to
/// reveal.
pub fn extract_smart(
    archive: &Path,
    dest_parent: &Path,
    passphrase: Option<&str>,
    cancel: &AtomicBool,
) -> Result<PathBuf, ArchiveError> {
    let stage = staging_dir(dest_parent)?;
    let result = unpack_into(archive, &stage, passphrase, cancel).and_then(|()| place(archive, &stage, dest_parent));
    // Whatever is left of the staging folder (everything, after a failure) goes.
    let _ = crate::core::filesystem::remove_tree(&stage);
    result
}

fn unpack_into(
    archive: &Path,
    stage: &Path,
    passphrase: Option<&str>,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    let mut cmd = bsdtar();
    cmd.arg("-xf").arg(archive).arg("-C").arg(stage).arg("--no-same-owner");
    run(cmd, passphrase, cancel)
}

/// Move what was unpacked in `stage` to its final place under `dest_parent`.
fn place(archive: &Path, stage: &Path, dest_parent: &Path) -> Result<PathBuf, ArchiveError> {
    use crate::core::filesystem::rename_noreplace;
    use std::io::ErrorKind::AlreadyExists;
    let fail = |e: std::io::Error| ArchiveError::Failed(e.to_string());
    let names: Vec<_> = std::fs::read_dir(stage).map_err(fail)?.flatten().map(|e| e.file_name()).collect();
    if let [one] = names.as_slice() {
        let target = dest_parent.join(one);
        match rename_noreplace(&stage.join(one), &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == AlreadyExists => {} // wrap it in a folder instead
            Err(e) => return Err(fail(e)),
        }
    }
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for _ in 0..1000 {
        let dir = unique_dir(dest_parent, &stem(&name));
        match rename_noreplace(stage, &dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == AlreadyExists => continue, // taken in the meantime: pick the next name
            Err(e) => return Err(fail(e)),
        }
    }
    Err(ArchiveError::Failed("could not find a free folder name".into()))
}

/// Create `out` from `names` (relative to `base_dir`). Writes to a temporary
/// `.part` file and renames on success; refuses to overwrite `out`.
pub fn create(
    format: Format,
    level: Level,
    base_dir: &Path,
    names: &[String],
    out: &Path,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    if out.exists() {
        return Err(ArchiveError::Failed(format!("{} already exists", out.display())));
    }
    let part = PathBuf::from(format!("{}.part", out.display()));
    let (n_fast, n_norm, n_best) = match format {
        Format::TarZst => (1, 3, 19),
        _ => (1, 6, 9),
    };
    let lv = match level {
        Level::Fast => n_fast,
        Level::Normal => n_norm,
        Level::Best => n_best,
    };
    let mut cmd = bsdtar();
    match format {
        Format::Zip => cmd.args(["--format", "zip", "--options", &format!("zip:compression-level={lv}")]),
        Format::Tar => cmd.args(["--format", "pax"]),
        Format::TarGz => cmd.args(["-z", "--options", &format!("gzip:compression-level={lv}")]),
        Format::TarXz => cmd.args(["-J", "--options", &format!("xz:compression-level={lv}")]),
        Format::TarZst => cmd.args(["--zstd", "--options", &format!("zstd:compression-level={lv}")]),
        Format::SevenZ => cmd.args(["--format", "7zip", "--options", &format!("7zip:compression-level={lv}")]),
    };
    cmd.arg("-cf").arg(&part).arg("-C").arg(base_dir).arg("--");
    cmd.args(names);
    match run(cmd, None, cancel) {
        Ok(()) => crate::core::filesystem::rename_noreplace(&part, out).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            ArchiveError::Failed(e.to_string())
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn names() {
        assert!(is_archive("a.TAR.GZ") && is_archive("x.zip") && !is_archive("x.txt"));
        assert!(is_extractable_on_open("a.ZIP") && is_extractable_on_open("b.tar.zst"));
        assert!(
            !is_extractable_on_open("app.jar")
                && !is_extractable_on_open("disk.iso")
                && !is_extractable_on_open(".zip")
        );
        assert_eq!(stem("photos.tar.gz"), "photos");
        assert_eq!(stem("a.b.zip"), "a.b");
    }

    fn no_leftovers(dir: &Path) {
        let left: Vec<_> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    #[test]
    fn failed_extraction_leaves_nothing_behind() {
        let root = tmp("archive-fail");
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        // Poorly compressible data so the archive is big enough to cut in the middle.
        let data: Vec<u8> = (0..400_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        fs::write(src.join("one.bin"), &data).unwrap();
        fs::write(src.join("two.bin"), &data).unwrap();
        let c = AtomicBool::new(false);
        let good = root.join("good.tar.gz");
        create(Format::TarGz, Level::Fast, &src, &["one.bin".into(), "two.bin".into()], &good, &c).unwrap();
        let bytes = fs::read(&good).unwrap();
        let cut = root.join("cut.tar.gz");
        fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).unwrap();
        let garbage = root.join("garbage.zip");
        fs::write(&garbage, b"this is not an archive").unwrap();
        for bad in [&cut, &garbage] {
            let dest = root.join(format!("dest-{}", bad.file_name().unwrap().to_string_lossy()));
            fs::create_dir_all(&dest).unwrap();
            assert!(extract_smart(bad, &dest, None, &c).is_err());
            no_leftovers(&dest);
        }
        // A cancelled extraction cleans up too.
        let dest = root.join("dest-cancel");
        fs::create_dir_all(&dest).unwrap();
        assert!(matches!(extract_smart(&good, &dest, None, &AtomicBool::new(true)), Err(ArchiveError::Cancelled)));
        no_leftovers(&dest);
    }

    #[test]
    fn passphrase_travels_on_stdin() {
        let root = tmp("archive-pass");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/secret.txt"), "classified").unwrap();
        let zip = root.join("locked.zip");
        // Built by hand: Luma itself never creates encrypted archives.
        let made = Command::new("bsdtar")
            .args(["--format", "zip", "--options", "zip:encryption=zipcrypt", "--passphrase", "hunter2", "-cf"])
            .arg(&zip)
            .arg("-C")
            .arg(root.join("src"))
            .arg("secret.txt")
            .status()
            .unwrap();
        assert!(made.success());
        let c = AtomicBool::new(false);
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        assert!(matches!(extract_smart(&zip, &dest, None, &c), Err(ArchiveError::NeedsPassphrase)));
        assert!(matches!(extract_smart(&zip, &dest, Some("wrong"), &c), Err(ArchiveError::NeedsPassphrase)));
        no_leftovers(&dest);
        let shown = extract_smart(&zip, &dest, Some("hunter2"), &c).unwrap();
        assert_eq!(fs::read_to_string(shown).unwrap(), "classified");
        assert!(matches!(
            extract_smart(&zip, &dest, Some("two\nlines"), &c),
            Err(ArchiveError::Failed(m)) if m.contains("line break")
        ));
    }

    #[test]
    fn parses_listing_with_spaces_and_links() {
        let e = parse_line("-rw-r--r--  0 me  me      1234 Jan  1 12:00 my dir/file name.txt").unwrap();
        assert_eq!((e.path.as_str(), e.size, e.is_dir), ("my dir/file name.txt", 1234, false));
        let d = parse_line("drwxr-xr-x  0 me  me         0 Jan  1 12:00 my dir/").unwrap();
        assert!(d.is_dir && d.path == "my dir");
        let l = parse_line("lrwxrwxrwx  0 me  me         0 Jan  1 12:00 link -> target").unwrap();
        assert_eq!(l.path, "link");
        let odd = parse_line("-rw-r--r--  0 me  me         5 Jan  1 12:00 a -> b.txt").unwrap();
        assert_eq!(odd.path, "a -> b.txt", "only symlinks carry a target");
    }

    #[test]
    fn roundtrip_all_formats_and_no_overwrite() {
        let root = tmp("archive");
        let src = root.join("src");
        fs::create_dir_all(src.join("pics/sub")).unwrap();
        fs::write(src.join("pics/a.txt"), "alpha").unwrap();
        fs::write(src.join("pics/sub/b.txt"), "beta").unwrap();
        fs::write(src.join("loose.txt"), "loose").unwrap();
        let c = AtomicBool::new(false);
        for f in Format::ALL {
            let out = root.join(format!("test.{}", f.ext()));
            create(f, Level::Normal, &src, &["pics".into()], &out, &c).unwrap_or_else(|e| panic!("{f:?}: {e}"));
            let entries = list(&out, None).unwrap();
            assert!(entries.iter().any(|e| e.path == "pics/sub/b.txt"), "{f:?}: {entries:?}");
            // Single top-level folder → extracted straight into the destination.
            let dest = root.join(format!("out-{}", f.ext()));
            fs::create_dir_all(&dest).unwrap();
            let shown = extract_smart(&out, &dest, None, &c).unwrap();
            assert_eq!(shown, dest.join("pics"));
            assert_eq!(fs::read_to_string(dest.join("pics/sub/b.txt")).unwrap(), "beta");
            // Extracting again must not overwrite: it goes to a fresh folder.
            fs::write(dest.join("pics/a.txt"), "MODIFIED").unwrap();
            let again = extract_smart(&out, &dest, None, &c).unwrap();
            assert_ne!(again, dest.join("pics"));
            assert_eq!(fs::read_to_string(dest.join("pics/a.txt")).unwrap(), "MODIFIED");
            // create() refuses to clobber an existing archive.
            assert!(create(f, Level::Fast, &src, &["pics".into()], &out, &c).is_err());
        }
        // Several top-level entries → wrapped in a new folder named after the archive.
        let multi = root.join("multi.tar.gz");
        create(Format::TarGz, Level::Fast, &src, &["pics".into(), "loose.txt".into()], &multi, &c).unwrap();
        let dest = root.join("multi-out");
        fs::create_dir_all(&dest).unwrap();
        assert_eq!(extract_smart(&multi, &dest, None, &c).unwrap(), dest.join("multi"));
        assert!(dest.join("multi/loose.txt").exists());
    }
}
