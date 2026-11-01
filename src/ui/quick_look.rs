//! macOS-style Quick Look: a small floating window previewing the selection.
//! Space or Esc dismisses it; arrow keys move the selection underneath while
//! it stays open; Return opens the file.

use adw::prelude::*;
use gtk::gdk;
use std::{cell::RefCell, rc::Rc};

use crate::ui::preview::{Preview, Target};

type StepFn = Rc<dyn Fn(i32)>;
type OpenFn = Rc<dyn Fn()>;

pub struct QuickLook {
    window: adw::Window,
    preview: Rc<Preview>,
    title: adw::WindowTitle,
    on_step: RefCell<Option<StepFn>>,
    on_open: RefCell<Option<OpenFn>>,
}

impl QuickLook {
    pub fn new(parent: &gtk::Window) -> Rc<Self> {
        let preview = Preview::new(true);
        preview.hide_title();
        let title = adw::WindowTitle::new("Quick Look", "");
        let open = gtk::Button::with_label("Open");
        open.add_css_class("suggested-action");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        header.pack_start(&open);
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&preview.widget));

        let window = adw::Window::builder()
            .transient_for(parent)
            .destroy_with_parent(true)
            .hide_on_close(true)
            .default_width(720)
            .default_height(620)
            .content(&tv)
            .build();
        window.add_css_class("fm-quick-look");

        let this = Rc::new(Self { window, preview, title, on_step: RefCell::default(), on_open: RefCell::default() });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(q) = w.upgrade() else { return gtk::glib::Propagation::Proceed };
            match key {
                gdk::Key::space | gdk::Key::Escape => q.hide(),
                gdk::Key::Left | gdk::Key::Up => q.step(-1),
                gdk::Key::Right | gdk::Key::Down => q.step(1),
                gdk::Key::Return | gdk::Key::KP_Enter => q.open(),
                _ => return gtk::glib::Propagation::Proceed,
            }
            gtk::glib::Propagation::Stop
        });
        this.window.add_controller(keys);
        let w = Rc::downgrade(&this);
        open.connect_clicked(move |_| {
            if let Some(q) = w.upgrade() {
                q.open();
            }
        });
        // Release media decoders as soon as the window goes away, however it
        // was dismissed (Space, Esc, close button, window manager).
        let w = Rc::downgrade(&this);
        this.window.connect_hide(move |_| {
            if let Some(q) = w.upgrade() {
                q.preview.show(None);
            }
        });
        let w = Rc::downgrade(&this);
        this.window.connect_unmap(move |_| {
            if let Some(q) = w.upgrade() {
                q.preview.show(None);
            }
        });
        this
    }

    pub fn set_callbacks(&self, on_step: impl Fn(i32) + 'static, on_open: impl Fn() + 'static) {
        *self.on_step.borrow_mut() = Some(Rc::new(on_step));
        *self.on_open.borrow_mut() = Some(Rc::new(on_open));
    }

    fn step(&self, d: i32) {
        let cb = self.on_step.borrow().clone();
        if let Some(cb) = cb {
            cb(d);
        }
    }

    fn open(&self) {
        let cb = self.on_open.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    /// Update the content; present the window if `present` is set.
    pub fn show(&self, target: Option<Target>, present: bool) {
        match &target {
            Some((_, info)) => {
                self.title.set_title(&info.display_name());
                self.title.set_subtitle("");
            }
            None => self.title.set_title("Quick Look"),
        }
        self.preview.show(target);
        if present {
            self.window.present();
        }
    }
}
