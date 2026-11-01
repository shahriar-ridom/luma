//! Undo journal. Only reversible actions are recorded; anything that
//! replaced or merged into existing data clears the journal instead
//! (see `Summary::undoable`).

use gtk::gio::{self, prelude::*};
use std::{cell::RefCell, path::PathBuf};

use super::{rename::rename_many, trash};

#[derive(Clone, Debug)]
pub enum UndoAction {
    /// Rename each `(current, original)` pair back.
    Rename(Vec<(PathBuf, PathBuf)>),
    /// Items we created (new folder/file, copies): move them to the trash.
    TrashCreated(Vec<PathBuf>),
    /// Move `(now, original)` back.
    MoveBack(Vec<(PathBuf, PathBuf)>),
    /// Restore these originals from the trash.
    Restore(Vec<PathBuf>),
}

#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub label: String,
    pub action: UndoAction,
}

#[derive(Default)]
pub struct UndoStack {
    entries: RefCell<Vec<UndoEntry>>,
}

const MAX_UNDO: usize = 50;

impl UndoStack {
    pub fn push(&self, label: &str, action: UndoAction) {
        let mut e = self.entries.borrow_mut();
        e.push(UndoEntry { label: label.to_string(), action });
        if e.len() > MAX_UNDO {
            e.remove(0);
        }
    }
    pub fn pop(&self) -> Option<UndoEntry> {
        self.entries.borrow_mut().pop()
    }
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
    }
    pub fn peek_label(&self) -> Option<String> {
        self.entries.borrow().last().map(|e| e.label.clone())
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }
}

/// Carry out an undo. Returns how many items were handled; errors are
/// collected so one bad item doesn't block the rest.
pub fn perform(action: &UndoAction) -> Result<usize, String> {
    let mut errors = Vec::new();
    let mut n = 0;
    match action {
        UndoAction::Rename(pairs) => {
            rename_many(pairs)?;
            n = pairs.len();
        }
        UndoAction::TrashCreated(paths) => {
            for p in paths {
                match gio::File::for_path(p).trash(gio::Cancellable::NONE) {
                    Ok(()) => n += 1,
                    Err(e) if e.matches(gio::IOErrorEnum::NotFound) => {}
                    Err(e) => errors.push(format!("{}: {}", p.display(), e.message())),
                }
            }
        }
        UndoAction::MoveBack(pairs) => {
            for (now, orig) in pairs {
                let flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
                match gio::File::for_path(now).move_(&gio::File::for_path(orig), flags, gio::Cancellable::NONE, None) {
                    Ok(()) => n += 1,
                    Err(e) => errors.push(format!("{}: {}", orig.display(), e.message())),
                }
            }
        }
        UndoAction::Restore(origs) => {
            for o in origs {
                match trash::find_anywhere(o) {
                    Some(t) => match trash::restore(&t, o) {
                        Ok(()) => n += 1,
                        Err(e) => errors.push(format!("{}: {e}", o.display())),
                    },
                    None => errors.push(format!("{} was not found in the trash", o.display())),
                }
            }
        }
    }
    if errors.is_empty() { Ok(n) } else { Err(format!("{} undone; {}", n, errors.join("; "))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn stack_limits_and_order() {
        let s = UndoStack::default();
        for i in 0..60 {
            s.push(&format!("op{i}"), UndoAction::TrashCreated(vec![]));
        }
        assert_eq!(s.len(), MAX_UNDO);
        assert_eq!(s.peek_label().as_deref(), Some("op59"));
        s.pop();
        assert_eq!(s.peek_label().as_deref(), Some("op58"));
        s.clear();
        assert!(s.pop().is_none());
    }

    #[test]
    fn rename_and_moveback_refuse_to_overwrite() {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/undo");
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("new"), "n").unwrap();
        fs::write(d.join("old"), "taken").unwrap();
        // The original name is occupied now: undo must fail and change nothing.
        assert!(perform(&UndoAction::Rename(vec![(d.join("new"), d.join("old"))])).is_err());
        assert_eq!(fs::read_to_string(d.join("old")).unwrap(), "taken");
        fs::remove_file(d.join("old")).unwrap();
        assert_eq!(perform(&UndoAction::Rename(vec![(d.join("new"), d.join("old"))])).unwrap(), 1);
        assert!(d.join("old").exists() && !d.join("new").exists());

        fs::write(d.join("sub/f"), "f").unwrap();
        fs::write(d.join("f"), "blocker").unwrap();
        assert!(perform(&UndoAction::MoveBack(vec![(d.join("sub/f"), d.join("f"))])).is_err());
        assert_eq!(fs::read_to_string(d.join("f")).unwrap(), "blocker");
        assert!(d.join("sub/f").exists());
    }
}
