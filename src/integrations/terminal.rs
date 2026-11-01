//! Open a terminal emulator in a directory. No shell string is ever built:
//! the directory is passed as an argument or as the child's working dir.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const CANDIDATES: &[&str] = &[
    "xdg-terminal-exec",
    "kitty",
    "alacritty",
    "foot",
    "wezterm",
    "ghostty",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "tilix",
    "kgx",
    "xterm",
];

fn in_path(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        return Path::new(prog).is_file().then(|| PathBuf::from(prog));
    }
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(prog)).find(|f| f.is_file()))
}

/// Preferred emulator (preference, then `$TERMINAL`, then a known list).
pub fn detect(preferred: &str) -> Option<String> {
    let env = std::env::var("TERMINAL").unwrap_or_default();
    [preferred, env.as_str()]
        .into_iter()
        .filter(|c| !c.trim().is_empty())
        .map(str::to_string)
        .chain(CANDIDATES.iter().map(|s| s.to_string()))
        .find(|c| in_path(c).is_some())
}

/// Extra arguments some emulators need to start in a directory; the rest
/// inherit it from the child's working directory.
fn dir_args(term: &str, dir: &Path) -> Vec<String> {
    let base = Path::new(term).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let d = dir.to_string_lossy();
    match base.as_str() {
        "gnome-terminal" | "xfce4-terminal" | "tilix" => vec![format!("--working-directory={d}")],
        "konsole" => vec!["--workdir".into(), d.into_owned()],
        "wezterm" => vec!["start".into(), "--cwd".into(), d.into_owned()],
        "kgx" => vec![format!("--working-directory={d}")],
        _ => vec![],
    }
}

pub fn open_in(dir: &Path, preferred: &str) -> Result<String, String> {
    let term = detect(preferred).ok_or("No terminal emulator found. Set $TERMINAL or install one.")?;
    Command::new(&term)
        .args(dir_args(&term, dir))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| term.clone())
        .map_err(|e| format!("Could not start {term}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_args_per_emulator() {
        let d = Path::new("/tmp/a b");
        assert_eq!(dir_args("konsole", d), ["--workdir", "/tmp/a b"]);
        assert_eq!(dir_args("/usr/bin/gnome-terminal", d), ["--working-directory=/tmp/a b"]);
        assert!(dir_args("kitty", d).is_empty());
    }
}
