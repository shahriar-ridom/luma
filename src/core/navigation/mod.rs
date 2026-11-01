//! Navigation history, independent of any view mode or widget.

use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct History {
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    current: Option<PathBuf>,
}

impl History {
    pub fn current(&self) -> Option<&Path> {
        self.current.as_deref()
    }
    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }
    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Visit a new location, clearing the forward stack.
    pub fn visit(&mut self, path: PathBuf) {
        if self.current.as_ref() == Some(&path) {
            return;
        }
        if let Some(cur) = self.current.replace(path) {
            self.back.push(cur);
        }
        self.forward.clear();
    }

    pub fn go_back(&mut self) -> Option<&Path> {
        let prev = self.back.pop()?;
        if let Some(cur) = self.current.replace(prev) {
            self.forward.push(cur);
        }
        self.current()
    }

    pub fn go_forward(&mut self) -> Option<&Path> {
        let next = self.forward.pop()?;
        if let Some(cur) = self.current.replace(next) {
            self.back.push(cur);
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_forward() {
        let mut h = History::default();
        h.visit("/a".into());
        h.visit("/b".into());
        h.visit("/c".into());
        assert_eq!(h.go_back(), Some(Path::new("/b")));
        assert_eq!(h.go_back(), Some(Path::new("/a")));
        assert_eq!(h.go_back(), None);
        assert_eq!(h.go_forward(), Some(Path::new("/b")));
        h.visit("/d".into());
        assert!(!h.can_forward());
        assert!(h.can_back());
    }
}
