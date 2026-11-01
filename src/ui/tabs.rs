use adw::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use crate::ui::{pane::Pane, split_view::SplitView};

/// One tab: a split container with one or two panes.
pub struct Tab {
    pub page: adw::TabPage,
    pub split: SplitView,
    pub panes: RefCell<Vec<Rc<Pane>>>,
    pub active: Cell<usize>,
}

impl Tab {
    pub fn active_pane(&self) -> Rc<Pane> {
        let panes = self.panes.borrow();
        panes[self.active.get().min(panes.len() - 1)].clone()
    }

    pub fn index_of(&self, pane: &Rc<Pane>) -> Option<usize> {
        self.panes.borrow().iter().position(|p| Rc::ptr_eq(p, pane))
    }

    pub fn relayout_styles(&self) {
        let n = self.panes.borrow().len();
        for (i, p) in self.panes.borrow().iter().enumerate() {
            p.set_active_style(n > 1 && i == self.active.get());
        }
    }

    pub fn relayout(&self) {
        let widgets: Vec<gtk::Widget> = self.panes.borrow().iter().map(|p| p.root.clone().upcast()).collect();
        self.split.set_panes(&widgets);
        self.relayout_styles();
    }
}
