//! Batch rename planning and a safe multi-file rename.
//!
//! Planning is pure (names in, names out) so it can drive a live preview.
//! Applying renames in two phases (via temporary names) makes swaps and
//! cycles work and guarantees nothing is overwritten.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseMode {
    Lower,
    Upper,
    Title,
}

#[derive(Clone, Debug)]
pub enum Rule {
    /// Replace text (optionally a regex, optionally case-insensitive).
    Replace {
        find: String,
        with: String,
        regex: bool,
        ignore_case: bool,
    },
    Prefix(String),
    /// Appended before the extension.
    Suffix(String),
    /// `{n}` in the pattern becomes the counter, zero-padded to `digits`;
    /// `{name}` is the original base name.
    Number {
        pattern: String,
        start: u32,
        step: u32,
        digits: usize,
    },
    Case(CaseMode),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Unchanged,
    Invalid(String),
    /// Two items would get the same name.
    Duplicate,
    /// An unrelated file already has this name.
    Exists,
}

#[derive(Clone, Debug)]
pub struct Planned {
    pub old: String,
    pub new: String,
    pub status: Status,
}

/// Split into (stem, extension-with-dot); ".bashrc" has no extension and
/// "a.tar.gz" keeps ".tar.gz".
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let (stem, ext) = name.split_at(i);
            if let Some(s) = stem.strip_suffix(".tar") {
                return (&name[..s.len()], &name[s.len()..]);
            }
            (stem, ext)
        }
        _ => (name, ""),
    }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if up {
                out.extend(c.to_uppercase())
            } else {
                out.extend(c.to_lowercase())
            }
            up = false;
        } else {
            out.push(c);
            up = c == ' ' || c == '_' || c == '-' || c == '.';
        }
    }
    out
}

fn apply_rule(rule: &Rule, stem: &str, idx: usize, re: Option<&regex::Regex>) -> String {
    match rule {
        Rule::Replace { find, with, regex, ignore_case } => {
            if find.is_empty() {
                return stem.to_string();
            }
            if *regex {
                match re {
                    Some(r) => r.replace_all(stem, with.as_str()).into_owned(),
                    None => stem.to_string(),
                }
            } else if *ignore_case {
                match regex::RegexBuilder::new(&regex::escape(find)).case_insensitive(true).build() {
                    Ok(r) => r.replace_all(stem, regex::NoExpand(with)).into_owned(),
                    Err(_) => stem.to_string(),
                }
            } else {
                stem.replace(find.as_str(), with)
            }
        }
        Rule::Prefix(p) => format!("{p}{stem}"),
        Rule::Suffix(s) => format!("{stem}{s}"),
        Rule::Number { pattern, start, step, digits } => {
            let n = start.saturating_add(step.saturating_mul(idx as u32));
            let num = format!("{n:0width$}", width = *digits);
            let pat = if pattern.contains("{n}") { pattern.clone() } else { format!("{pattern}{{n}}") };
            pat.replace("{n}", &num).replace("{name}", stem)
        }
        Rule::Case(CaseMode::Lower) => stem.to_lowercase(),
        Rule::Case(CaseMode::Upper) => stem.to_uppercase(),
        Rule::Case(CaseMode::Title) => title_case(stem),
    }
}

/// Plan new names. `existing` = names currently in the folder (including the
/// ones being renamed). `with_ext` applies the rule to the whole name.
pub fn plan(names: &[String], rule: &Rule, with_ext: bool, existing: &HashSet<String>) -> Vec<Planned> {
    let re = match rule {
        Rule::Replace { find, regex: true, ignore_case, .. } if !find.is_empty() => {
            regex::RegexBuilder::new(find).case_insensitive(*ignore_case).build().ok()
        }
        _ => None,
    };
    let renaming: HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut out: Vec<Planned> = names
        .iter()
        .enumerate()
        .map(|(i, old)| {
            let (stem, ext) = if with_ext { (old.as_str(), "") } else { split_ext(old) };
            let new = format!("{}{}", apply_rule(rule, stem, i, re.as_ref()), ext);
            let status = if new == *old {
                Status::Unchanged
            } else if new.is_empty() || new == "." || new == ".." || new.contains('/') || new.contains('\0') {
                Status::Invalid("not a valid file name".into())
            } else if new.len() > 255 {
                Status::Invalid("name too long".into())
            } else {
                Status::Ok
            };
            Planned { old: old.clone(), new, status }
        })
        .collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for p in out.iter().filter(|p| p.status == Status::Ok || p.status == Status::Unchanged) {
        *seen.entry(p.new.clone()).or_default() += 1;
    }
    for p in out.iter_mut() {
        if p.status != Status::Ok {
            continue;
        }
        if seen[&p.new] > 1 {
            p.status = Status::Duplicate;
        } else if existing.contains(&p.new) && !renaming.contains(p.new.as_str()) {
            p.status = Status::Exists;
        }
    }
    out
}

pub fn has_problems(plan: &[Planned]) -> bool {
    plan.iter().any(|p| matches!(p.status, Status::Invalid(_) | Status::Duplicate | Status::Exists))
}

/// Rename every `(from, to)` pair or none. Targets must not exist unless the
/// target is itself one of the sources being renamed away.
pub fn rename_many(pairs: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let pairs: Vec<_> = pairs.iter().filter(|(a, b)| a != b).collect();
    let sources: HashSet<&Path> = pairs.iter().map(|(a, _)| a.as_path()).collect();
    let mut targets = HashSet::new();
    for (from, to) in &pairs {
        if !targets.insert(to.as_path()) {
            return Err(format!("two items would be named {}", to.display()));
        }
        if fs::symlink_metadata(from).is_err() {
            return Err(format!("{} no longer exists", from.display()));
        }
        if fs::symlink_metadata(to).is_ok() && !sources.contains(to.as_path()) {
            return Err(format!("{} already exists", to.display()));
        }
    }
    // Phase 1: move everything to unique temporary names.
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new(); // (orig, tmp, final)
    let undo = |staged: &[(PathBuf, PathBuf, PathBuf)]| {
        for (orig, tmp, _) in staged.iter().rev() {
            let _ = fs::rename(tmp, orig);
        }
    };
    for (i, (from, to)) in pairs.iter().enumerate() {
        let dir = from.parent().unwrap_or_else(|| Path::new("."));
        let tmp = dir.join(format!(".fm-rename-{}-{i}", std::process::id()));
        if let Err(e) = fs::rename(from, &tmp) {
            undo(&staged);
            return Err(format!("{}: {e}", from.display()));
        }
        staged.push((from.clone(), tmp, to.clone()));
    }
    // Phase 2: temporary → final. Never overwrite; roll everything back on failure.
    let mut finished: Vec<(PathBuf, PathBuf)> = Vec::new(); // (final, tmp)
    for (_, tmp, to) in &staged {
        let result: io::Result<()> = if fs::symlink_metadata(to).is_ok() {
            Err(io::Error::new(io::ErrorKind::AlreadyExists, "already exists"))
        } else {
            fs::rename(tmp, to)
        };
        if let Err(e) = result {
            for (fin, t) in finished.iter().rev() {
                let _ = fs::rename(fin, t);
            }
            undo(&staged);
            return Err(format!("{}: {e}", to.display()));
        }
        finished.push((to.clone(), tmp.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ext_splitting() {
        assert_eq!(split_ext("a.txt"), ("a", ".txt"));
        assert_eq!(split_ext("a.tar.gz"), ("a", ".tar.gz"));
        assert_eq!(split_ext(".bashrc"), (".bashrc", ""));
        assert_eq!(split_ext("noext"), ("noext", ""));
    }

    #[test]
    fn rules() {
        let n = names(&["IMG_1.JPG", "IMG_2.JPG"]);
        let all = set(&["IMG_1.JPG", "IMG_2.JPG"]);
        let p = plan(
            &n,
            &Rule::Replace { find: "img".into(), with: "Photo".into(), regex: false, ignore_case: true },
            false,
            &all,
        );
        assert_eq!(p[0].new, "Photo_1.JPG");
        let p = plan(&n, &Rule::Number { pattern: "Trip {n}".into(), start: 8, step: 1, digits: 3 }, false, &all);
        assert_eq!((p[0].new.as_str(), p[1].new.as_str()), ("Trip 008.JPG", "Trip 009.JPG"));
        let p = plan(&n, &Rule::Case(CaseMode::Lower), true, &all);
        assert_eq!(p[0].new, "img_1.jpg");
        let p = plan(&names(&["my cool file.txt"]), &Rule::Case(CaseMode::Title), false, &set(&[]));
        assert_eq!(p[0].new, "My Cool File.txt");
        let p = plan(
            &names(&["a-1.txt"]),
            &Rule::Replace { find: r"(\w)-(\d)".into(), with: "$2$1".into(), regex: true, ignore_case: false },
            false,
            &set(&[]),
        );
        assert_eq!(p[0].new, "1a.txt");
    }

    #[test]
    fn conflicts_are_flagged() {
        let n = names(&["a.txt", "b.txt"]);
        let existing = set(&["a.txt", "b.txt", "x.txt"]);
        let same = plan(&n, &Rule::Number { pattern: "x".into(), start: 1, step: 0, digits: 0 }, true, &existing);
        assert!(same.iter().all(|p| p.status == Status::Duplicate));
        let clash = plan(
            &names(&["a.txt"]),
            &Rule::Replace { find: "a".into(), with: "x".into(), regex: false, ignore_case: false },
            false,
            &existing,
        );
        assert_eq!(clash[0].status, Status::Exists);
        let bad = plan(
            &names(&["a.txt"]),
            &Rule::Replace { find: "a".into(), with: "x/y".into(), regex: false, ignore_case: false },
            false,
            &existing,
        );
        assert!(matches!(bad[0].status, Status::Invalid(_)));
        assert!(has_problems(&bad));
    }

    fn tmp(name: &str) -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn swap_works_and_overwrite_is_refused() {
        let d = tmp("rename_many");
        fs::write(d.join("a"), "A").unwrap();
        fs::write(d.join("b"), "B").unwrap();
        fs::write(d.join("keep"), "K").unwrap();
        rename_many(&[(d.join("a"), d.join("b")), (d.join("b"), d.join("a"))]).unwrap();
        assert_eq!(fs::read_to_string(d.join("a")).unwrap(), "B");
        assert_eq!(fs::read_to_string(d.join("b")).unwrap(), "A");
        // Renaming onto an unrelated existing file must fail and change nothing.
        let err = rename_many(&[(d.join("a"), d.join("keep"))]).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(fs::read_to_string(d.join("a")).unwrap(), "B");
        assert_eq!(fs::read_to_string(d.join("keep")).unwrap(), "K");
    }
}
