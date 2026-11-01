//! Shared tag visuals: a stable colour per tag name.

use gtk::prelude::*;

const COLORS: u32 = 7;

pub fn class_for(tag: &str) -> String {
    let h = tag.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    format!("fm-tag-{}", h % COLORS)
}

pub fn dot(tag: &str) -> gtk::Label {
    let l = gtk::Label::new(Some("●"));
    l.add_css_class(&class_for(tag));
    l.set_tooltip_text(Some(tag));
    l
}

/// Replace the contents of `row` with a Git state badge and coloured tag dots.
pub fn fill_badges(row: &gtk::Box, tags: &[String], git: Option<crate::integrations::git::FileState>) {
    while let Some(c) = row.first_child() {
        row.remove(&c);
    }
    // Ignored files are common (build output, caches); a badge on each is just noise.
    let git = git.filter(|s| *s != crate::integrations::git::FileState::Ignored);
    if let Some(s) = git {
        let l = gtk::Label::new(Some(s.letter()));
        l.add_css_class("fm-git");
        l.add_css_class(s.css());
        l.set_tooltip_text(Some(s.label()));
        row.append(&l);
    }
    for t in tags.iter().take(5) {
        row.append(&dot(t));
    }
    if tags.len() > 5 {
        let more = gtk::Label::new(Some(&format!("+{}", tags.len() - 5)));
        more.add_css_class("dim-label");
        row.append(&more);
    }
    row.set_visible(!tags.is_empty() || git.is_some());
}
