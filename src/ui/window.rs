use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use crate::application::{
    actions::{ACTIONS, EXTRA_ACTIONS},
    state::AppState,
};
use crate::settings::persistence;
use crate::ui::{
    dialogs,
    file_list::SortKey,
    operations::OpsUi,
    pane::{Pane, PaneEnv, PaneEvent},
    quick_look::QuickLook,
    sidebar::Sidebar,
    split_view::SplitView,
    tabs::Tab,
};

glib::wrapper! {
    pub struct FmWindow(ObjectSubclass<imp::FmWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
        gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct FmWindow {
        pub ctl: RefCell<Option<Rc<WinCtl>>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for FmWindow {
        const NAME: &'static str = "FmWindow";
        type Type = super::FmWindow;
        type ParentType = adw::ApplicationWindow;
    }
    impl ObjectImpl for FmWindow {}
    impl WidgetImpl for FmWindow {}
    impl WindowImpl for FmWindow {}
    impl ApplicationWindowImpl for FmWindow {}
    impl AdwApplicationWindowImpl for FmWindow {}
}

const VIEWS: [&str; 4] = ["grid", "list", "column", "gallery"];
const MAX_CLOSED: usize = 20;

pub struct WinCtl {
    win: glib::WeakRef<FmWindow>,
    state: Rc<AppState>,
    env: Rc<PaneEnv>,
    tabs: adw::TabView,
    tab_list: RefCell<Vec<Rc<Tab>>>,
    closed: RefCell<Vec<PathBuf>>,
    menu_page: RefCell<Option<adw::TabPage>>,
    sidebar: Rc<Sidebar>,
    split: adw::OverlaySplitView,
    quick_look: Rc<QuickLook>,
    view_btns: Vec<gtk::ToggleButton>,
    sort_dd: gtk::DropDown,
    sort_dir: gtk::ToggleButton,
    search: gtk::SearchEntry,
    hidden_btn: gtk::ToggleButton,
    recent_btn: gtk::MenuButton,
    title: adw::WindowTitle,
    syncing: Cell<bool>,
    restoring: Cell<bool>,
    /// Set once the user agreed to close while operations were running.
    close_confirmed: Cell<bool>,
    primary: bool,
    actions: HashMap<&'static str, gio::SimpleAction>,
    shortcut_ctrl: RefCell<Vec<gtk::ShortcutController>>,
    /// Storage facts for the folder the island describes.
    place: RefCell<Option<Place>>,
    /// Stops measuring a folder we've left.
    measuring: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    /// Size of the selected folder, measured when the island's card opens.
    sel_folder: RefCell<Option<(PathBuf, crate::ui::island::FolderSize)>>,
    sel_measuring: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
}

impl FmWindow {
    pub fn new(app: &adw::Application, state: Rc<AppState>, primary: bool, paths: Vec<PathBuf>, active: usize) -> Self {
        let win: Self = glib::Object::builder().property("application", app).build();
        let w = state.db.window_state("width").and_then(|v| v.parse().ok()).unwrap_or(1100);
        let h = state.db.window_state("height").and_then(|v| v.parse().ok()).unwrap_or(700);
        win.set_default_size(w, h);
        if primary && state.db.window_state("maximized").as_deref() == Some("1") {
            win.maximize();
        }
        let ctl = WinCtl::build(&win, state, primary);
        *win.imp().ctl.borrow_mut() = Some(ctl.clone());
        ctl.restore(paths, active);
        win
    }

    /// Open folders in new tabs; files open their folder with the file selected.
    pub fn open_locations(&self, paths: Vec<PathBuf>, properties: bool) {
        if let Some(c) = self.imp().ctl.borrow().clone() {
            c.open_locations(paths, properties);
        }
        self.present();
    }

    /// Re-read shortcut bindings (after the editor changed them).
    pub fn apply_shortcuts(&self) {
        if let Some(c) = self.imp().ctl.borrow().clone() {
            c.apply_shortcuts();
        }
    }
}

/// Storage facts gathered for one folder.
#[derive(Clone)]
struct Place {
    dir: PathBuf,
    /// Drive (size, free).
    drive: Option<(u64, u64)>,
    scope: Scope,
}

#[derive(Clone)]
enum Scope {
    /// Not known yet.
    Pending,
    /// The folder is the top of a drive (Home, `/`, a mount point): show the drive.
    Drive,
    /// A folder inside a drive, with its measured size.
    Folder(Option<u64>),
}

/// Home, `/` and mount points describe a whole drive; other folders describe
/// themselves. Runs off the main thread (stat can block on network mounts).
fn is_drive_top(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    if dir == glib::home_dir() {
        return true;
    }
    let Some(parent) = dir.parent() else { return true };
    match (std::fs::metadata(dir), std::fs::metadata(parent)) {
        (Ok(a), Ok(b)) => a.dev() != b.dev(),
        _ => false,
    }
}

pub fn apply_color_scheme(name: &str) {
    adw::StyleManager::default().set_color_scheme(match name {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    });
}

fn dir_title(p: &Path) -> String {
    if crate::core::operations::trash::is_trash(p) {
        return "Trash".into();
    }
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}

impl WinCtl {
    fn build(win: &FmWindow, state: Rc<AppState>, primary: bool) -> Rc<Self> {
        // Applied here (after libadwaita is initialised), not in the app's startup handler.
        apply_color_scheme(&state.prefs.color_scheme.borrow());
        let ops = OpsUi::new(win.upcast_ref(), state.clone());
        let env = Rc::new(PaneEnv {
            ops: ops.clone(),
            window: win.upcast_ref::<gtk::Window>().downgrade(),
            state: state.clone(),
            notify: RefCell::default(),
        });

        // ---- actions ---------------------------------------------------------
        let mut actions = HashMap::new();
        let ids = ACTIONS.iter().map(|a| a.id).chain(EXTRA_ACTIONS.iter().copied());
        for id in ids {
            let a = gio::SimpleAction::new(id, None);
            win.add_action(&a);
            actions.insert(id, a);
        }

        // ---- header ----------------------------------------------------------
        let mk_btn = |icon: &str, tip: &str, action: &str| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b.set_action_name(Some(action));
            b
        };
        let nav_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        nav_box.add_css_class("fm-seg");
        nav_box.append(&mk_btn("luma-back-symbolic", "Back", "win.back"));
        nav_box.append(&mk_btn("luma-forward-symbolic", "Forward", "win.forward"));
        nav_box.append(&mk_btn("luma-up-symbolic", "Parent folder", "win.parent"));

        let mk_view = |icon: &str, tip: &str| {
            let b = gtk::ToggleButton::new();
            b.set_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b
        };
        let view_btns = vec![
            mk_view("luma-view-grid-symbolic", "Icons"),
            mk_view("luma-view-list-symbolic", "List"),
            mk_view("luma-view-columns-symbolic", "Columns"),
            mk_view("luma-view-gallery-symbolic", "Gallery"),
        ];
        for b in &view_btns[1..] {
            b.set_group(Some(&view_btns[0]));
        }
        view_btns[1].set_active(true);
        let view_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        view_box.add_css_class("fm-seg");
        for b in &view_btns {
            view_box.append(b);
        }

        let labels: Vec<&str> = SortKey::ALL.iter().map(|k| k.label()).collect();
        let sort_dd = gtk::DropDown::from_strings(&labels);
        sort_dd.set_tooltip_text(Some("Sort by"));
        let sort_dir = gtk::ToggleButton::new();
        sort_dir.set_icon_name("luma-sort-asc-symbolic");
        sort_dir.set_tooltip_text(Some("Reverse sort order"));
        let sort_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        sort_dd.set_hexpand(true);
        sort_box.append(&sort_dd);
        sort_box.append(&sort_dir);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Filter"));
        search.set_width_chars(14);

        let hidden_btn = gtk::ToggleButton::with_label("Show hidden files");
        hidden_btn.add_css_class("flat");
        if let Some(l) = hidden_btn.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }

        // One tidy "View options" popover instead of a row of toolbar controls.
        let options_btn = gtk::MenuButton::new();
        options_btn.set_icon_name("luma-options-symbolic");
        options_btn.set_tooltip_text(Some("View options"));
        let sort_title = gtk::Label::new(Some("Sort by"));
        sort_title.set_xalign(0.0);
        sort_title.add_css_class("fm-section");
        let opts = gtk::Box::new(gtk::Orientation::Vertical, 8);
        opts.set_margin_top(12);
        opts.set_margin_bottom(8);
        opts.set_margin_start(12);
        opts.set_margin_end(12);
        opts.set_width_request(230);
        opts.append(&sort_title);
        opts.append(&sort_box);
        opts.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        opts.append(&hidden_btn);
        let opts_pop = gtk::Popover::new();
        opts_pop.set_child(Some(&opts));
        options_btn.set_popover(Some(&opts_pop));

        let recent_btn = gtk::MenuButton::new();
        recent_btn.set_icon_name("luma-history-symbolic");
        recent_btn.set_tooltip_text(Some("Recent folders and files"));

        let sidebar_btn = gtk::ToggleButton::new();
        sidebar_btn.set_icon_name("luma-sidebar-symbolic");
        sidebar_btn.set_tooltip_text(Some("Toggle sidebar"));
        let preview_btn = gtk::Button::from_icon_name("luma-eye-symbolic");
        preview_btn.set_tooltip_text(Some("Quick Look (Space)"));
        preview_btn.set_action_name(Some("win.quick-preview"));

        let main_menu = gio::Menu::new();
        let s1 = gio::Menu::new();
        s1.append(Some("New Window"), Some("win.new-window"));
        s1.append(Some("New Tab"), Some("win.new-tab"));
        s1.append(Some("Split View"), Some("win.split-view"));
        main_menu.append_section(None, &s1);
        let s_edit = gio::Menu::new();
        s_edit.append(Some("Undo"), Some("win.undo"));
        s_edit.append(Some("Find Duplicate Files…"), Some("win.find-duplicates"));
        s_edit.append(Some("Show Git Status"), Some("win.git-enabled"));
        s_edit.append(Some("Custom Actions & Plugins…"), Some("win.extensions"));
        main_menu.append_section(None, &s_edit);
        let theme = gio::Menu::new();
        for (label, val) in [("System", "default"), ("Light", "light"), ("Dark", "dark")] {
            let it = gio::MenuItem::new(Some(label), None);
            it.set_action_and_target_value(Some("win.color-scheme"), Some(&val.to_variant()));
            theme.append_item(&it);
        }
        let s_theme = gio::Menu::new();
        s_theme.append_submenu(Some("Appearance"), &theme);
        s_theme.append(Some("Toggle Sidebar"), Some("win.toggle-sidebar"));
        main_menu.append_section(None, &s_theme);
        let s2 = gio::Menu::new();
        s2.append(Some("Connect to Server…"), Some("win.connect-server"));
        s2.append(Some("Storage & Disk Usage…"), Some("win.storage"));
        s2.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        s2.append(Some("About"), Some("win.about"));
        main_menu.append_section(None, &s2);
        let menu_btn = gtk::MenuButton::new();
        menu_btn.set_icon_name("luma-more-symbolic");
        menu_btn.set_menu_model(Some(&main_menu));

        let title = adw::WindowTitle::new("", "");
        let header = adw::HeaderBar::new();
        header.add_css_class("fm-header");
        // The location lives in the pane's breadcrumb bar, so the title stays empty.
        header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
        header.pack_start(&sidebar_btn);
        header.pack_start(&nav_box);
        header.pack_start(&view_box);
        header.pack_end(&menu_btn);
        header.pack_end(&recent_btn);
        header.pack_end(&preview_btn);
        header.pack_end(&options_btn);
        header.pack_end(&search);

        // ---- tabs ------------------------------------------------------------
        let tabs = adw::TabView::new();
        let tab_menu = gio::Menu::new();
        tab_menu.append(Some("Duplicate Tab"), Some("win.tab-duplicate"));
        tab_menu.append(Some("Close Tab"), Some("win.tab-close"));
        tab_menu.append(Some("Close Other Tabs"), Some("win.tab-close-others"));
        tabs.set_menu_model(Some(&tab_menu));
        let tab_bar = adw::TabBar::new();
        tab_bar.set_view(Some(&tabs));
        tab_bar.set_autohide(true);

        // ---- sidebar (callbacks are bound to the controller afterwards) ----------
        let sb_nav: Rc<RefCell<Option<std::rc::Weak<WinCtl>>>> = Rc::default();
        let sb_drop: Rc<RefCell<Option<std::rc::Weak<WinCtl>>>> = Rc::default();
        let n = sb_nav.clone();
        let d = sb_drop.clone();
        let rv = sb_nav.clone();
        let sidebar = Sidebar::new(
            state.clone(),
            win.upcast_ref(),
            ops.clone(),
            move |p| {
                if let Some(c) = n.borrow().as_ref().and_then(|w| w.upgrade()) {
                    if let Some(pane) = c.active_pane() {
                        pane.navigate(p);
                    }
                    if c.split.is_collapsed() {
                        c.split.set_show_sidebar(false);
                    }
                }
            },
            Rc::new(move |files, dest, copy| {
                // Sidebar drops go through the active pane's transfer logic.
                if let Some(c) = d.borrow().as_ref().and_then(|w| w.upgrade())
                    && let Some(pane) = c.active_pane()
                {
                    pane.drop_files(files, dest, copy);
                }
            }),
            Rc::new(move |path| {
                if let Some(pane) = rv.borrow().as_ref().and_then(|w| w.upgrade()).and_then(|c| c.active_pane()) {
                    pane.reveal(path);
                }
            }),
        );
        sidebar.widget.set_width_request(210);
        sidebar.widget.add_css_class("fm-sidebar");

        let split = adw::OverlaySplitView::new();
        split.add_css_class("fm-split");
        split.set_sidebar(Some(&sidebar.widget));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // The browsing area is a rounded sheet inset into the window frame.
        let sheet = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sheet.add_css_class("fm-sheet");
        sheet.set_overflow(gtk::Overflow::Hidden);
        sheet.append(&tabs);
        tab_bar.add_css_class("fm-tabbar");
        content.append(&tab_bar);
        content.append(&sheet);
        content.set_hexpand(true);
        tabs.set_vexpand(true);
        sheet.set_vexpand(true);
        let intro = sheet.clone();
        win.connect_map(move |_| {
            intro.add_css_class("fm-intro");
            let i = intro.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || i.remove_css_class("fm-intro"));
        });

        let quick_look = QuickLook::new(win.upcast_ref());
        split.set_content(Some(&content));
        split.set_sidebar_width_fraction(0.2);
        split.set_min_sidebar_width(180.0);
        sidebar_btn.set_active(true);
        sidebar_btn.bind_property("active", &split, "show-sidebar").bidirectional().sync_create().build();

        // Responsive layout: collapse the sidebar into an overlay, then drop
        // secondary header controls as the window narrows.
        win.set_width_request(360);
        let bp = |cond: &str| adw::Breakpoint::new(adw::BreakpointCondition::parse(cond).expect("breakpoint"));
        let narrow = bp("max-width: 760sp");
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&recent_btn, "visible", Some(&false.to_value()));
        win.add_breakpoint(narrow);
        let medium = bp("max-width: 980sp");
        medium.add_setter(&search, "visible", Some(&false.to_value()));
        win.add_breakpoint(medium);
        let tiny = bp("max-width: 560sp");
        tiny.add_setter(&view_box, "visible", Some(&false.to_value()));
        tiny.add_setter(&preview_btn, "visible", Some(&false.to_value()));
        // Too narrow for the island's text: keep just the ring.
        tiny.add_setter(ops.island().text(), "visible", Some(&false.to_value()));
        win.add_breakpoint(tiny);

        win.add_css_class("fm-window");
        let tv = adw::ToolbarView::new();
        tv.set_top_bar_style(adw::ToolbarStyle::Flat);
        tv.add_top_bar(&header);
        tv.set_content(Some(&split));
        // The activity island floats over the middle of the header bar; the
        // header's controls tighten up to make room for it.
        let island_layer = crate::ui::island_layout::install(
            &tv,
            ops.island(),
            &header,
            vec![sidebar_btn.clone().upcast(), nav_box.clone().upcast(), view_box.clone().upcast()],
            vec![
                search.clone().upcast(),
                options_btn.clone().upcast(),
                preview_btn.clone().upcast(),
                recent_btn.clone().upcast(),
                menu_btn.clone().upcast(),
            ],
            vec![(search.clone().upcast(), 2), (view_box.clone().upcast(), 3)],
        );
        win.set_content(Some(&island_layer));

        let ctl = Rc::new(WinCtl {
            win: win.downgrade(),
            state,
            env,
            tabs,
            tab_list: RefCell::default(),
            closed: RefCell::default(),
            menu_page: RefCell::default(),
            sidebar,
            split,
            quick_look,
            view_btns,
            sort_dd,
            sort_dir,
            search,
            hidden_btn,
            recent_btn,
            title,
            syncing: Cell::new(false),
            restoring: Cell::new(false),
            close_confirmed: Cell::new(false),
            primary,
            actions,
            shortcut_ctrl: RefCell::default(),
            place: RefCell::default(),
            measuring: RefCell::default(),
            sel_folder: RefCell::default(),
            sel_measuring: RefCell::default(),
        });
        *sb_nav.borrow_mut() = Some(Rc::downgrade(&ctl));
        *sb_drop.borrow_mut() = Some(Rc::downgrade(&ctl));
        ctl.wire();
        ctl.apply_shortcuts();
        ctl
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);

        // Appearance (stateful action, applied app-wide).
        if let Some(win) = self.win.upgrade() {
            let cur = match self.state.prefs.color_scheme.borrow().as_str() {
                "" => "default".to_string(),
                s => s.to_string(),
            };
            let cs = gio::SimpleAction::new_stateful("color-scheme", Some(glib::VariantTy::STRING), &cur.to_variant());
            let st = self.state.clone();
            cs.connect_activate(move |a, p| {
                let v = p.and_then(|v| v.get::<String>()).unwrap_or_else(|| "default".into());
                a.set_state(&v.to_variant());
                apply_color_scheme(&v);
                persistence::save_color_scheme(&st.db, &st.prefs, &v);
            });
            win.add_action(&cs);
        }
        let (w1, w2) = (weak.clone(), weak.clone());
        // Free space and folder sizes change when operations finish.
        let w = weak.clone();
        self.env.ops.set_on_settled(move || {
            if let Some(c) = w.upgrade() {
                c.sel_folder.borrow_mut().take();
                // Trashing, restoring or emptying changes what the Trash holds.
                for t in c.tab_list.borrow().iter() {
                    for p in t.panes.borrow().iter() {
                        p.reload_trash();
                    }
                }
                c.refresh_island(true);
            }
        });
        // Opening the island's card on a selected folder measures it.
        let w = weak.clone();
        self.env.ops.island().set_on_open(move || {
            if let Some(c) = w.upgrade() {
                c.refresh_island(false);
            }
        });
        self.quick_look.set_callbacks(
            move |d| {
                if let Some(p) = w1.upgrade().and_then(|c| c.active_pane()) {
                    p.step_selection(d);
                }
            },
            move || {
                if let Some(c) = w2.upgrade() {
                    c.quick_look.hide();
                    c.run_action("open");
                }
            },
        );

        // Extension actions carry their key as a string parameter.
        if let Some(win) = self.win.upgrade() {
            let ea = gio::SimpleAction::new("ext-action", Some(glib::VariantTy::STRING));
            let w = weak.clone();
            ea.connect_activate(move |_, p| {
                if let (Some(c), Some(key)) = (w.upgrade(), p.and_then(|v| v.get::<String>()))
                    && let Some(pane) = c.active_pane()
                {
                    pane.run_ext(&key);
                }
            });
            win.add_action(&ea);

            // Git status can be switched off globally.
            let ga = gio::SimpleAction::new_stateful("git-enabled", None, &self.state.prefs.git_enabled().to_variant());
            let w = weak.clone();
            ga.connect_activate(move |a, _| {
                let Some(c) = w.upgrade() else { return };
                let on = !c.state.prefs.git_enabled();
                a.set_state(&on.to_variant());
                persistence::save_git_enabled(&c.state.db, &c.state.prefs, on);
                for t in c.tab_list.borrow().iter() {
                    for p in t.panes.borrow().iter() {
                        p.refresh_git();
                    }
                }
            });
            win.add_action(&ga);
        }

        // Pane events.
        let w = weak.clone();
        *self.env.notify.borrow_mut() = Some(Rc::new(move |pane, ev| {
            if let Some(c) = w.upgrade() {
                c.on_pane_event(pane, ev);
            }
        }));

        // Actions.
        for (id, a) in &self.actions {
            let (w, id) = (weak.clone(), *id);
            a.connect_activate(move |_, _| {
                if let Some(c) = w.upgrade() {
                    c.run_action(id);
                }
            });
        }

        // Header controls (ignored while we mirror pane state into them).
        for (i, b) in self.view_btns.iter().enumerate() {
            let w = weak.clone();
            b.connect_toggled(move |b| {
                let Some(c) = w.upgrade() else { return };
                if b.is_active() && !c.syncing.get() {
                    c.set_view(VIEWS[i]);
                }
            });
        }
        let w = weak.clone();
        let apply_sort = move || {
            let Some(c) = w.upgrade() else { return };
            if c.syncing.get() {
                return;
            }
            if let Some(p) = c.active_pane() {
                p.set_sort(SortKey::ALL[c.sort_dd.selected() as usize], !c.sort_dir.is_active());
            }
            c.sort_dir.set_icon_name(if c.sort_dir.is_active() {
                "luma-sort-desc-symbolic"
            } else {
                "luma-sort-asc-symbolic"
            });
        };
        let a = apply_sort.clone();
        self.sort_dd.connect_selected_notify(move |_| a());
        self.sort_dir.connect_toggled(move |_| apply_sort());
        let w = weak.clone();
        self.search.connect_search_changed(move |e| {
            let Some(c) = w.upgrade() else { return };
            if !c.syncing.get()
                && let Some(p) = c.active_pane()
            {
                p.set_query(&e.text());
            }
        });
        let w = weak.clone();
        self.hidden_btn.connect_toggled(move |b| {
            let Some(c) = w.upgrade() else { return };
            if !c.syncing.get() {
                c.set_hidden(b.is_active());
            }
        });

        // Recent popover is built on demand so it is always current.
        let w = weak.clone();
        self.recent_btn.set_create_popup_func(move |btn| {
            if let Some(c) = w.upgrade() {
                btn.set_popover(Some(&c.build_recent_popover()));
            }
        });

        // Tab lifecycle.
        let w = weak.clone();
        self.tabs.connect_selected_page_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.on_tab_selected();
            }
        });
        let w = weak.clone();
        self.tabs.connect_close_page(move |_, page| {
            if let Some(c) = w.upgrade()
                && !c.restoring.get()
                && let Some(tab) = c.tab_for(page)
                && let Some(d) = tab.panes.borrow().first().and_then(|p| p.cur_dir())
            {
                let mut closed = c.closed.borrow_mut();
                closed.push(d);
                if closed.len() > MAX_CLOSED {
                    closed.remove(0);
                }
            }
            glib::Propagation::Proceed
        });
        let w = weak.clone();
        self.tabs.connect_page_detached(move |tv, page, _| {
            let Some(c) = w.upgrade() else { return };
            c.tab_list.borrow_mut().retain(|t| &t.page != page);
            if tv.n_pages() == 0 {
                if let Some(win) = c.win.upgrade() {
                    win.close();
                }
            } else {
                c.save_session();
                c.update_actions();
            }
        });
        let w = weak.clone();
        self.tabs.connect_setup_menu(move |_, page| {
            if let Some(c) = w.upgrade() {
                *c.menu_page.borrow_mut() = page.cloned();
            }
        });

        // Mouse side buttons: 8 = back, 9 = forward (as in browsers and other file managers).
        if let Some(win) = self.win.upgrade() {
            let g = gtk::GestureClick::new();
            g.set_button(0);
            g.set_propagation_phase(gtk::PropagationPhase::Capture);
            let w = weak.clone();
            g.connect_pressed(move |g, _, _, _| {
                let Some(c) = w.upgrade() else { return };
                match g.current_button() {
                    8 => c.run_action("back"),
                    9 => c.run_action("forward"),
                    _ => return,
                }
                g.set_state(gtk::EventSequenceState::Claimed);
            });
            win.add_controller(g);
        }

        // Window state.
        if let Some(win) = self.win.upgrade() {
            let w = weak.clone();
            win.connect_close_request(move |win| {
                if let Some(c) = w.upgrade()
                    && !c.close_confirmed.get()
                    && c.env.ops.active_count() > 0
                {
                    // Closing would kill running copies mid-file: ask first.
                    let (c, win) = (c.clone(), win.clone());
                    glib::spawn_future_local(async move {
                        let n = c.env.ops.active_count();
                        let body = if n == 1 {
                            "An operation is still running. Closing now cancels it and may leave a partly copied file behind.".to_string()
                        } else {
                            format!("{n} operations are still running. Closing now cancels them and may leave partly copied files behind.")
                        };
                        if !dialogs::confirm(win.upcast_ref(), "Cancel running operations?", &body, "Cancel and Close", true).await {
                            return;
                        }
                        c.env.ops.cancel_all();
                        // Give the workers a moment to stop and clean up.
                        for _ in 0..30 {
                            if c.env.ops.active_count() == 0 {
                                break;
                            }
                            glib::timeout_future(std::time::Duration::from_millis(100)).await;
                        }
                        c.close_confirmed.set(true);
                        win.close();
                    });
                    return glib::Propagation::Stop;
                }
                if let Some(c) = w.upgrade()
                    && c.primary
                {
                    let (width, height) = win.default_size();
                    let db = &c.state.db;
                    if !win.is_maximized() {
                        let _ = db.set_window_state("width", &width.to_string());
                        let _ = db.set_window_state("height", &height.to_string());
                    }
                    let _ = db.set_window_state("maximized", if win.is_maximized() { "1" } else { "0" });
                }
                glib::Propagation::Proceed
            });
        }
    }

    // ---- tabs --------------------------------------------------------------

    /// Folders that no longer exist are dropped, but only after the window is
    /// up: checking them here could freeze startup on a stale network mount.
    fn drop_missing_tabs(self: &Rc<Self>, saved: Vec<(PathBuf, Rc<Tab>)>) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let paths: Vec<PathBuf> = saved.iter().map(|(p, _)| p.clone()).collect();
            let Ok(found) = gio::spawn_blocking(move || paths.iter().map(|p| p.is_dir()).collect::<Vec<_>>()).await
            else {
                return;
            };
            let gone: Vec<(PathBuf, Rc<Tab>)> =
                saved.into_iter().zip(found).filter(|(_, ok)| !ok).map(|(s, _)| s).collect();
            // Skip tabs the user already closed or navigated somewhere else.
            let gone: Vec<Rc<Tab>> = gone
                .into_iter()
                .filter(|(p, t)| {
                    this.tab_list.borrow().iter().any(|x| Rc::ptr_eq(x, t))
                        && t.panes.borrow().first().and_then(|pane| pane.cur_dir()).as_deref() == Some(p.as_path())
                })
                .map(|(_, t)| t)
                .collect();
            if gone.is_empty() {
                return;
            }
            this.restoring.set(true);
            if gone.len() == this.tabs.n_pages() as usize {
                this.add_tab(glib::home_dir(), true);
            }
            for t in gone {
                this.tabs.close_page(&t.page);
            }
            this.restoring.set(false);
            this.save_session();
        });
    }

    fn restore(self: &Rc<Self>, mut paths: Vec<PathBuf>, active: usize) {
        if paths.is_empty() {
            paths.push(glib::home_dir());
        }
        self.restoring.set(true);
        let tabs: Vec<_> = paths.iter().cloned().map(|p| self.add_tab(p, false)).collect();
        self.restoring.set(false);
        self.drop_missing_tabs(paths.into_iter().zip(tabs.iter().cloned()).collect());
        if let Some(t) = tabs.get(active).or(tabs.first()) {
            self.tabs.set_selected_page(&t.page);
        }
        self.on_tab_selected();
        self.save_session();
        #[cfg(debug_assertions)]
        if let Ok(dir) = std::env::var("FM_ISLAND_DEMO") {
            self.island_demo(PathBuf::from(dir));
        }
        #[cfg(debug_assertions)]
        if std::env::var("FM_QUICKLOOK").is_ok() {
            let w = Rc::downgrade(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                if let Some(c) = w.upgrade() {
                    c.run_action("quick-preview");
                }
            });
        }
    }

    /// Debug: drive the activity island through a fake operation and save
    /// window snapshots of each stage into `dir`, then quit.
    #[cfg(debug_assertions)]
    fn island_demo(self: &Rc<Self>, dir: PathBuf) {
        use std::time::Duration;
        let isl = self.env.ops.island().clone();
        let win = self.win.clone();
        let snap = move |name: &str| {
            let Some(w) = win.upgrade() else { return };
            let p = gtk::WidgetPaintable::new(Some(&w));
            let s = gtk::Snapshot::new();
            p.snapshot(&s, w.width() as f64, w.height() as f64);
            if let (Some(node), Some(r)) = (s.to_node(), w.native().and_then(|n| n.renderer())) {
                let _ = r.render_texture(node, None).save_to_png(dir.join(format!("{name}.png")));
            }
        };
        // Same path as a real click on the pill.
        let toggle = |i: &Rc<crate::ui::island::Island>| {
            let Some(top) = i.widget.first_child() else { return };
            let ctrls = top.observe_controllers();
            for c in ctrls.into_iter().flatten() {
                if let Ok(g) = c.downcast::<gtk::GestureClick>() {
                    g.emit_by_name::<()>("released", &[&1i32, &0f64, &0f64]);
                }
            }
        };
        use crate::ui::island::Outcome;
        let id = Rc::new(Cell::new(0u64));
        let mut steps: Vec<(u64, Box<dyn Fn()>)> = Vec::new();
        let mut at = |ms: u64, f: Box<dyn Fn()>| steps.push((ms, f));
        let shot = |name: &'static str| {
            let s = snap.clone();
            Box::new(move || s(name)) as Box<dyn Fn()>
        };
        if std::env::var("FM_LIGHT").is_ok() {
            at(300, Box::new(|| adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight)));
        }
        at(1200, shot("0-idle"));
        let i = isl.clone();
        at(1300, Box::new(move || toggle(&i)));
        // A burst through the opening animation, to check it moves smoothly.
        for (ms, name) in [(1330, "f1"), (1380, "f2"), (1430, "f3"), (1480, "f4"), (1560, "f5")] {
            at(ms, shot(name));
        }
        at(1900, shot("1-idle-card"));
        let i = isl.clone();
        at(2000, Box::new(move || toggle(&i)));
        let (i, d) = (isl.clone(), id.clone());
        at(2600, Box::new(move || d.set(i.add("Copying 24 items to “Photos”", Rc::new(|| {}), Some(Rc::new(|_| {}))))));
        let (i, d) = (isl.clone(), id.clone());
        at(3000, Box::new(move || i.progress(d.get(), Some(0.42), "1.4 GB of 3.4 GB · IMG_2041.jpg", None)));
        at(3500, shot("2-live"));
        let i = isl.clone();
        at(3600, Box::new(move || toggle(&i)));
        at(4200, shot("3-expanded"));
        let i = isl.clone();
        at(4300, Box::new(move || toggle(&i)));
        let (i, d) = (isl.clone(), id.clone());
        at(5000, Box::new(move || i.finish(d.get(), Outcome::Done("Copied 24 items".into()))));
        at(5600, shot("4-done"));
        at(8000, shot("5-idle-again"));
        // Inside a folder: its own size, not the drive's.
        let w = Rc::downgrade(self);
        at(
            8100,
            Box::new(move || {
                if let Some(p) = w.upgrade().and_then(|c| c.active_pane()) {
                    p.navigate(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
                }
            }),
        );
        at(9500, shot("6-folder"));
        let i = isl.clone();
        at(9600, Box::new(move || toggle(&i)));
        at(10300, shot("7-folder-card"));
        let i = isl.clone();
        at(10400, Box::new(move || toggle(&i)));
        let w = Rc::downgrade(self);
        at(
            10900,
            Box::new(move || {
                if let Some(p) = w.upgrade().and_then(|c| c.active_pane()) {
                    p.select_visible("main.rs");
                }
            }),
        );
        at(11500, shot("8-selected"));
        let i = isl.clone();
        at(11600, Box::new(move || toggle(&i)));
        at(12300, shot("9-selected-card"));
        let i = isl.clone();
        at(12400, Box::new(move || toggle(&i)));
        // Split view with the list: each half must keep names readable.
        let w = Rc::downgrade(self);
        at(
            12600,
            Box::new(move || {
                if let Some(c) = w.upgrade() {
                    c.run_action("view-list");
                    c.run_action("split-view");
                }
            }),
        );
        at(13600, shot("10-split"));
        // A selected folder: its size appears when the card opens.
        let w = Rc::downgrade(self);
        at(
            13700,
            Box::new(move || {
                if let Some(p) = w.upgrade().and_then(|c| c.active_pane()) {
                    p.select_visible("ui");
                }
            }),
        );
        at(14200, shot("11-folder-selected"));
        let i = isl.clone();
        at(14300, Box::new(move || toggle(&i)));
        at(15300, shot("12-folder-card"));
        let i = isl.clone();
        at(15400, Box::new(move || toggle(&i)));
        // The Trash (view only: the demo never deletes anything).
        let w = Rc::downgrade(self);
        at(
            15600,
            Box::new(move || {
                if let Some(c) = w.upgrade() {
                    c.run_action("split-view");
                    if let Some(p) = c.active_pane() {
                        p.navigate(PathBuf::from(crate::core::operations::trash::TRASH_URI));
                    }
                }
            }),
        );
        at(17000, shot("13-trash"));
        let i = isl.clone();
        at(17100, Box::new(move || toggle(&i)));
        at(17800, shot("14-trash-card"));
        at(
            17900,
            Box::new(move || {
                std::process::exit(0);
            }),
        );
        for (ms, f) in steps {
            glib::timeout_add_local_once(Duration::from_millis(ms), f);
        }
    }

    fn add_tab(self: &Rc<Self>, path: PathBuf, select: bool) -> Rc<Tab> {
        let pane = Pane::new(self.env.clone());
        pane.set_view(&self.state.prefs.view());
        pane.set_hidden(*self.state.prefs.show_hidden.borrow());
        let split = SplitView::new();
        let page = self.tabs.append(&split.widget);
        page.set_title(&dir_title(&path));
        page.set_icon(Some(&gio::ThemedIcon::new("luma-folder")));
        let tab =
            Rc::new(Tab { page: page.clone(), split, panes: RefCell::new(vec![pane.clone()]), active: Cell::new(0) });
        tab.relayout();
        self.tab_list.borrow_mut().push(tab.clone());
        if select {
            self.tabs.set_selected_page(&page);
        }
        pane.navigate(path);
        tab
    }

    fn tab_for(&self, page: &adw::TabPage) -> Option<Rc<Tab>> {
        self.tab_list.borrow().iter().find(|t| &t.page == page).cloned()
    }

    fn selected_tab(&self) -> Option<Rc<Tab>> {
        self.tabs.selected_page().and_then(|p| self.tab_for(&p))
    }

    fn active_pane(&self) -> Option<Rc<Pane>> {
        self.selected_tab().map(|t| t.active_pane())
    }

    fn tab_of_pane(&self, pane: &Rc<Pane>) -> Option<Rc<Tab>> {
        self.tab_list.borrow().iter().find(|t| t.index_of(pane).is_some()).cloned()
    }

    fn on_tab_selected(self: &Rc<Self>) {
        if let Some(p) = self.active_pane()
            && let Some(d) = p.cur_dir()
        {
            self.sidebar.select_path(&d);
        }
        self.sync_header();
        self.update_actions();
        self.update_preview();
        self.refresh_island(true);
    }

    /// Tell the island where we are: folder, its storage, the selection.
    /// `requery` re-reads the drive and re-measures the folder (after
    /// navigating, or when files may have changed).
    fn refresh_island(self: &Rc<Self>, requery: bool) {
        use crate::ui::island::{Idle, Selection, Storage};
        let Some(p) = self.active_pane() else { return };
        if p.in_trash() {
            // The Trash describes itself: what's in it and how much space it takes.
            let (n, bytes) = p.trash_summary();
            self.env.ops.island().set_idle(Idle {
                name: "Trash".into(),
                path: "Deleted items from all drives".into(),
                items: n as u32,
                storage: Storage::Trash { bytes },
                selection: None,
            });
            return;
        }
        let Some(dir) = p.cur_dir() else { return };
        let place = self.place.borrow().as_ref().filter(|pl| pl.dir == dir).cloned();
        let drive_size = place.as_ref().and_then(|pl| pl.drive).map(|(s, _)| s);
        let storage = match place.as_ref().map(|pl| (&pl.scope, pl.drive)) {
            Some((Scope::Drive, Some((size, free)))) => Storage::Drive { size, free },
            Some((Scope::Folder(b), _)) => Storage::Folder { bytes: *b, drive_size },
            _ => Storage::Unknown,
        };
        let infos = p.selection_infos();
        let selection = match infos.as_slice() {
            [] => None,
            [one] => {
                let is_dir = crate::core::filesystem::is_dir(one);
                let path = dir.join(one.name());
                let folder = if is_dir {
                    self.sel_folder
                        .borrow()
                        .as_ref()
                        .filter(|(p, _)| *p == path)
                        .map(|(_, f)| f.clone())
                        .unwrap_or_default()
                } else {
                    Default::default()
                };
                Some(Selection {
                    count: 1,
                    name: one.display_name().to_string(),
                    kind: if is_dir {
                        "Folder".into()
                    } else {
                        gio::content_type_get_description(&one.content_type().unwrap_or_default()).to_string()
                    },
                    bytes: one.size(),
                    is_dir,
                    modified: one
                        .modification_date_time()
                        .and_then(|d| d.to_local().ok())
                        .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    folder,
                })
            }
            many => Some(Selection {
                count: many.len() as u32,
                bytes: many.iter().filter(|i| !crate::core::filesystem::is_dir(i)).map(|i| i.size()).sum(),
                ..Default::default()
            }),
        };
        // Selection moved off the folder being measured: stop walking it.
        let selected_dir = match infos.as_slice() {
            [one] if crate::core::filesystem::is_dir(one) => Some(dir.join(one.name())),
            _ => None,
        };
        let stale = matches!(&*self.sel_folder.borrow(),
            Some((p, crate::ui::island::FolderSize::Measuring)) if Some(p) != selected_dir.as_ref());
        if stale {
            self.sel_folder.borrow_mut().take();
            if let Some(c) = self.sel_measuring.borrow_mut().take() {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        // The card is open on a folder we haven't measured: measure it now.
        let want_measure = self.env.ops.island().is_expanded()
            && matches!(&selection, Some(s) if s.is_dir && s.folder == crate::ui::island::FolderSize::NotMeasured);
        self.env.ops.island().set_idle(Idle {
            name: dir_title(&dir),
            path: dir.to_string_lossy().into_owned(),
            items: p.visible_count(),
            storage,
            selection,
        });
        if want_measure && let [one] = infos.as_slice() {
            self.measure_selected_folder(dir.join(one.name()));
        }
        if !requery && place.is_some() {
            return;
        }

        // Keep what we know about this folder while re-measuring (no flicker);
        // a new folder starts out measuring.
        if place.is_none() {
            *self.place.borrow_mut() = Some(Place { dir: dir.clone(), drive: None, scope: Scope::Pending });
        }
        let still_here = |c: &Rc<Self>, dir: &Path| c.active_pane().and_then(|p| p.cur_dir()).as_deref() == Some(dir);
        let update = |c: &Rc<Self>, dir: &Path, f: &dyn Fn(&mut Place)| {
            if let Some(pl) = c.place.borrow_mut().as_mut().filter(|pl| pl.dir == dir) {
                f(pl);
            }
        };

        // Drive size and free space.
        let (w, d) = (Rc::downgrade(self), dir.clone());
        glib::spawn_future_local(async move {
            let info = crate::services::storage::fs_info(&gio::File::for_path(&d)).await;
            let Some(c) = w.upgrade() else { return };
            if let Some(i) = info.filter(|i| i.size > 0) {
                update(&c, &d, &|pl| pl.drive = Some((i.size, i.free)));
                if still_here(&c, &d) {
                    c.refresh_island(false);
                }
            }
        });

        // The folder itself: the whole drive at its top, otherwise measured
        // off the main thread (cancelled if we move on).
        if let Some(old) = self.measuring.borrow_mut().take() {
            old.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        *self.measuring.borrow_mut() = Some(cancel.clone());
        let (w, d) = (Rc::downgrade(self), dir);
        glib::spawn_future_local(async move {
            let path = d.clone();
            let c2 = cancel.clone();
            let scope = gio::spawn_blocking(move || {
                if is_drive_top(&path) {
                    return Some(Scope::Drive);
                }
                let st = crate::core::metadata::dir_stats(&path, &c2);
                (!c2.load(std::sync::atomic::Ordering::Relaxed)).then_some(Scope::Folder(Some(st.bytes)))
            })
            .await;
            let (Some(c), Ok(Some(scope))) = (w.upgrade(), scope) else { return };
            update(&c, &d, &|pl| pl.scope = scope.clone());
            if still_here(&c, &d) {
                c.refresh_island(false);
            }
        });
    }

    /// Measure a selected folder off the main thread; the island's card
    /// shows "Measuring…" meanwhile. A newer request cancels this one.
    fn measure_selected_folder(self: &Rc<Self>, path: PathBuf) {
        use crate::ui::island::FolderSize;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        if let Some(old) = self.sel_measuring.borrow_mut().take() {
            old.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *self.sel_measuring.borrow_mut() = Some(cancel.clone());
        *self.sel_folder.borrow_mut() = Some((path.clone(), FolderSize::Measuring));
        self.refresh_island(false);
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let (p, c) = (path.clone(), cancel.clone());
            let st = gio::spawn_blocking(move || {
                let st = crate::core::metadata::dir_stats(&p, &c);
                (!c.load(Ordering::Relaxed)).then_some(st)
            })
            .await;
            let Some(win) = w.upgrade() else { return };
            let mut cache = win.sel_folder.borrow_mut();
            if cache.as_ref().is_none_or(|(p, _)| *p != path) {
                return;
            }
            *cache = match st {
                Ok(Some(st)) => Some((path, FolderSize::Done { bytes: st.bytes, files: st.files })),
                // Cancelled or failed: measure again next time the card opens.
                _ => None,
            };
            drop(cache);
            win.refresh_island(false);
        });
    }

    fn update_preview(&self) {
        if self.quick_look.is_visible() {
            self.quick_look.show(self.active_pane().and_then(|p| p.preview_target()), false);
        }
    }

    fn on_pane_event(self: &Rc<Self>, pane: &Rc<Pane>, ev: PaneEvent) {
        let Some(tab) = self.tab_of_pane(pane) else { return };
        let idx = tab.index_of(pane).unwrap_or(0);
        let is_selected_tab = self.selected_tab().is_some_and(|t| Rc::ptr_eq(&t, &tab));
        match ev {
            PaneEvent::Focused => {
                if tab.active.get() != idx {
                    tab.active.set(idx);
                    tab.relayout_styles();
                    if is_selected_tab {
                        self.on_tab_selected();
                    }
                }
            }
            PaneEvent::Navigated => {
                if let Some(d) = pane.location()
                    && idx == tab.active.get()
                {
                    tab.page.set_title(&dir_title(&d));
                    tab.page.set_tooltip(&d.to_string_lossy());
                    if is_selected_tab {
                        self.sidebar.select_path(&d);
                    }
                }
                if is_selected_tab && idx == tab.active.get() {
                    self.sync_header();
                    self.update_actions();
                    self.update_preview();
                    self.refresh_island(true);
                }
                self.save_session();
            }
            PaneEvent::Tags => self.sidebar.rebuild(),
            PaneEvent::Selection | PaneEvent::Status => {
                if is_selected_tab && idx == tab.active.get() {
                    self.update_actions();
                    self.update_preview();
                    // A finished load may follow file operations: re-read the drive.
                    self.refresh_island(ev == PaneEvent::Status);
                }
            }
        }
    }

    fn save_session(&self) {
        if !self.primary || self.restoring.get() {
            return;
        }
        // Debug demos navigate on their own; don't overwrite the real session.
        #[cfg(debug_assertions)]
        if std::env::var("FM_ISLAND_DEMO").is_ok() {
            return;
        }
        let mut paths = Vec::new();
        let mut active = 0;
        for i in 0..self.tabs.n_pages() {
            let page = self.tabs.nth_page(i);
            if let Some(d) = self.tab_for(&page).and_then(|t| t.panes.borrow().first().and_then(|p| p.cur_dir())) {
                if self.tabs.selected_page().as_ref() == Some(&page) {
                    active = paths.len();
                }
                paths.push(d);
            }
        }
        if !paths.is_empty()
            && let Err(e) = self.state.db.save_session(&paths, active)
        {
            tracing::warn!("saving session: {e:#}");
        }
    }

    fn toggle_split(self: &Rc<Self>) {
        let Some(tab) = self.selected_tab() else { return };
        if tab.panes.borrow().len() == 1 {
            let cur = tab.active_pane();
            let pane = Pane::new(self.env.clone());
            pane.set_view(&cur.view());
            pane.set_hidden(cur.hidden());
            tab.panes.borrow_mut().push(pane.clone());
            tab.active.set(1);
            tab.relayout();
            pane.navigate(cur.cur_dir().unwrap_or_else(glib::home_dir));
        } else {
            let a = tab.active.get();
            tab.panes.borrow_mut().remove(a);
            tab.active.set(0);
            tab.relayout();
        }
        self.on_tab_selected();
    }

    // ---- actions -------------------------------------------------------------

    fn set_view(self: &Rc<Self>, name: &str) {
        if let Some(p) = self.active_pane() {
            p.set_view(name);
        }
        persistence::save_view(&self.state.db, &self.state.prefs, name);
        self.sync_header();
    }

    fn set_hidden(self: &Rc<Self>, v: bool) {
        if let Some(p) = self.active_pane() {
            p.set_hidden(v);
        }
        persistence::save_hidden(&self.state.db, &self.state.prefs, v);
        self.sync_header();
    }

    fn run_action(self: &Rc<Self>, id: &str) {
        let pane = self.active_pane();
        let cur = pane.as_ref().and_then(|p| p.cur_dir());
        match id {
            "new-window" => {
                if let Some(app) = self.win.upgrade().and_then(|w| w.application()).and_downcast::<adw::Application>() {
                    FmWindow::new(&app, self.state.clone(), false, vec![cur.unwrap_or_else(glib::home_dir)], 0)
                        .present();
                }
            }
            "new-tab" | "duplicate-tab" => {
                self.add_tab(cur.unwrap_or_else(glib::home_dir), true);
            }
            "close-tab" => {
                if let Some(p) = self.tabs.selected_page() {
                    self.tabs.close_page(&p);
                }
            }
            "reopen-tab" => {
                let p = self.closed.borrow_mut().pop();
                if let Some(p) = p {
                    self.add_tab(p, true);
                }
            }
            "tab-duplicate" => {
                let page = self.menu_page.borrow().clone();
                if let Some(d) = page.and_then(|p| self.tab_for(&p)).and_then(|t| t.active_pane().cur_dir()) {
                    self.add_tab(d, true);
                }
            }
            "tab-close" => {
                let page = self.menu_page.borrow().clone();
                if let Some(p) = page {
                    self.tabs.close_page(&p);
                }
            }
            "tab-close-others" => {
                let page = self.menu_page.borrow().clone();
                if let Some(p) = page {
                    self.tabs.close_other_pages(&p);
                }
            }
            "next-tab" => {
                self.tabs.select_next_page();
            }
            "prev-tab" => {
                self.tabs.select_previous_page();
            }
            "split-view" => self.toggle_split(),
            "view-grid" => self.set_view("grid"),
            "view-list" => self.set_view("list"),
            "view-column" => self.set_view("column"),
            "view-gallery" => self.set_view("gallery"),
            "quick-preview" => {
                if self.quick_look.is_visible() {
                    self.quick_look.hide();
                } else if !self.quick_look.just_dismissed() {
                    self.quick_look.show(pane.and_then(|p| p.preview_target()), true);
                }
            }
            "toggle-sidebar" => self.split.set_show_sidebar(!self.split.shows_sidebar()),
            "toggle-hidden" => {
                if let Some(p) = &pane {
                    self.set_hidden(!p.hidden());
                }
            }
            "connect-server" => self.sidebar.connect_dialog(),
            "undo" => self.undo(),
            "extensions" => {
                if let Some(win) = self.win.upgrade() {
                    crate::ui::dialogs::extensions::show(win.upcast_ref(), self.state.clone(), Rc::new(|| {}));
                }
            }
            "storage" => {
                if let Some(win) = self.win.upgrade() {
                    let w = Rc::downgrade(self);
                    crate::ui::dialogs::storage::show(
                        win.upcast_ref(),
                        cur.unwrap_or_else(glib::home_dir),
                        Rc::new(move |p| {
                            if let Some(pane) = w.upgrade().and_then(|c| c.active_pane()) {
                                pane.navigate(p);
                            }
                        }),
                    );
                }
            }
            "search" => {
                if let (Some(win), Some(p)) = (self.win.upgrade(), pane.clone()) {
                    let w = Rc::downgrade(self);
                    crate::ui::dialogs::search::show(
                        win.upcast_ref(),
                        p.search_root(),
                        self.state.clone(),
                        Rc::new(move |path, reveal| {
                            let Some(pane) = w.upgrade().and_then(|c| c.active_pane()) else { return };
                            if reveal {
                                pane.reveal(path);
                                return;
                            }
                            glib::spawn_future_local(async move {
                                if crate::core::filesystem::is_dir_async(&path).await {
                                    pane.navigate(path);
                                } else {
                                    pane.launch(&path);
                                }
                            });
                        }),
                    );
                }
            }
            "find" => {
                self.search.grab_focus();
            }
            "empty-trash" => {
                if let Some(win) = self.win.upgrade() {
                    crate::ui::trash_view::empty_trash(win.upcast_ref(), &self.env.ops, || {});
                }
            }
            "bookmark-add" => {
                let targets = pane.as_ref().map(|p| p.favorite_targets()).unwrap_or_default();
                let added = targets.iter().filter(|d| self.sidebar.add_bookmark(d)).count();
                let msg = match (added, targets.len()) {
                    (0, _) => "Already in Favorites".to_string(),
                    (1, _) => "Added to Favorites".to_string(),
                    (n, _) => format!("Added {n} folders to Favorites"),
                };
                if !targets.is_empty() {
                    self.env.ops.toast(&msg);
                }
            }
            "favorite-remove" => {
                let targets = pane.as_ref().map(|p| p.favorite_targets()).unwrap_or_default();
                let removed = targets.iter().filter(|d| self.sidebar.remove_favorite(d)).count();
                if removed > 0 {
                    self.env.ops.toast("Removed from Favorites");
                }
            }
            "shortcuts" => {
                if let Some(win) = self.win.upgrade() {
                    let app = win.application();
                    let w = win.clone();
                    dialogs::shortcuts::show(
                        win.upcast_ref(),
                        self.state.clone(),
                        Rc::new(move || {
                            if let Some(app) = app.as_ref() {
                                for win in app.windows() {
                                    if let Some(fw) = win.downcast_ref::<FmWindow>() {
                                        fw.apply_shortcuts();
                                    }
                                }
                            } else {
                                w.apply_shortcuts();
                            }
                        }),
                    );
                }
            }
            "about" => {
                if let Some(win) = self.win.upgrade() {
                    let about = adw::AboutDialog::new();
                    about.set_application_name("Luma");
                    about.set_application_icon(crate::application::app::APP_ID);
                    about.set_version(env!("CARGO_PKG_VERSION"));
                    about.set_comments("Design beautifully. Navigate effortlessly. Manage everything.");
                    about.present(Some(&win));
                }
            }
            _ => {
                let Some(p) = pane else { return };
                match id {
                    "focus-location" => p.focus_location(),
                    "select-all" => p.select_all(),
                    "open" => p.open_selected(),
                    "copy" => p.copy_cut(false),
                    "cut" => p.copy_cut(true),
                    "paste" => p.paste(),
                    "new-folder" => p.create(true),
                    "new-file" => p.create(false),
                    "rename" => p.rename(),
                    "trash" => p.trash(),
                    "delete" => p.delete(),
                    "back" => p.go_back(),
                    "forward" => p.go_forward(),
                    "parent" => p.go_up(),
                    "refresh" => p.refresh(),
                    "properties" => p.properties(),
                    "tags" => p.edit_tags(),
                    "open-with" => p.open_with(),
                    "git-stage" => p.git_stage(),
                    "git-unstage" => p.git_unstage(),
                    "git-discard" => p.git_discard(),
                    "git-commit" => p.git_commit(),
                    "git-pull" => p.git_pull(),
                    "git-push" => p.git_push(),
                    "git-refresh" => p.refresh_git(),
                    "compare" => p.compare(),
                    "find-duplicates" => p.find_duplicates(),
                    "compress" => p.compress(),
                    "extract-here" => p.extract(true),
                    "extract-to" => p.extract(false),
                    "browse-archive" => p.browse_archive(),
                    "open-terminal" => p.open_terminal(),
                    "copy-path" => p.copy_path(),
                    _ => {}
                }
            }
        }
    }

    fn open_locations(self: &Rc<Self>, paths: Vec<PathBuf>, properties: bool) {
        let this = self.clone();
        // Telling folders from files stats them; do that off the main thread.
        glib::spawn_future_local(async move {
            for p in paths {
                if crate::core::filesystem::is_dir_async(&p).await {
                    this.add_tab(p, true);
                } else if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
                    let tab = this.add_tab(parent.to_path_buf(), true);
                    tab.active_pane().select_when_loaded(name.to_string_lossy().into_owned());
                    if properties {
                        let w = Rc::downgrade(&this);
                        glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
                            if let Some(c) = w.upgrade() {
                                c.run_action("properties");
                            }
                        });
                    }
                }
            }
        });
    }

    fn undo(self: &Rc<Self>) {
        let Some(entry) = self.state.undo.pop() else {
            self.env.ops.toast("Nothing to undo");
            return;
        };
        let (label, action, w) = (entry.label.clone(), entry.action.clone(), Rc::downgrade(self));
        let moved_back = match &entry.action {
            crate::core::operations::undo::UndoAction::MoveBack(p) => p.clone(),
            _ => vec![],
        };
        self.env.ops.run_job(
            &format!("Undoing {label}"),
            move |_c| crate::core::operations::undo::perform(&action),
            move |r| {
                let Some(c) = w.upgrade() else { return };
                match r {
                    Ok(_) => {
                        for (now, orig) in &moved_back {
                            let _ = c.state.db.move_tags(now, orig);
                        }
                        c.env.ops.toast(&format!("Undid {label}"));
                    }
                    Err(e) => c.env.ops.toast(&format!("Could not undo {label}: {e}")),
                }
            },
        );
    }

    fn update_actions(&self) {
        let en = |id: &str, v: bool| {
            if let Some(a) = self.actions.get(id) {
                a.set_enabled(v);
            }
        };
        let Some(p) = self.active_pane() else { return };
        let n = p.selected_count();
        for k in ["open", "copy", "cut", "trash", "delete"] {
            en(k, n > 0);
        }
        en("rename", n >= 1);
        en("tags", n > 0);
        en("compare", p.can_compare());
        en("open-with", n > 0);
        let repo = p.in_repo();
        for k in ["git-stage", "git-unstage", "git-discard"] {
            en(k, repo && n > 0);
        }
        for k in ["git-commit", "git-pull", "git-push", "git-refresh"] {
            en(k, repo);
        }
        en("compress", n > 0);
        let arc = p.archive_selection();
        en("extract-here", arc);
        en("extract-to", arc);
        en("browse-archive", arc && n == 1);
        en("back", p.can_back());
        en("forward", p.can_forward());
        en("parent", p.can_up());
        en("reopen-tab", !self.closed.borrow().is_empty());
    }

    /// Mirror the active pane's state into the header controls.
    fn sync_header(&self) {
        let Some(p) = self.active_pane() else { return };
        self.syncing.set(true);
        let view = p.view();
        if let Some(i) = VIEWS.iter().position(|v| *v == view) {
            self.view_btns[i].set_active(true);
        }
        let (key, asc) = p.sort();
        self.sort_dd.set_selected(SortKey::ALL.iter().position(|k| *k == key).unwrap_or(0) as u32);
        self.sort_dir.set_active(!asc);
        self.sort_dir.set_icon_name(if asc { "luma-sort-asc-symbolic" } else { "luma-sort-desc-symbolic" });
        self.hidden_btn.set_active(p.hidden());
        if self.search.text() != p.query() {
            self.search.set_text(&p.query());
        }
        if let Some(d) = p.cur_dir() {
            self.title.set_title(&dir_title(&d));
            self.title.set_subtitle(&d.to_string_lossy());
            if let Some(w) = self.win.upgrade() {
                w.set_title(Some(&dir_title(&d)));
            }
        }
        self.syncing.set(false);
    }

    fn build_recent_popover(self: &Rc<Self>) -> gtk::Popover {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.set_margin_top(6);
        col.set_margin_bottom(6);
        col.set_margin_start(6);
        col.set_margin_end(6);
        let pop = gtk::Popover::new();
        let heading = |t: &str| {
            let l = gtk::Label::new(Some(t));
            l.add_css_class("heading");
            l.set_xalign(0.0);
            l.set_margin_top(6);
            l.set_margin_start(6);
            l
        };
        let entry = |p: &Path| {
            let b = gtk::Button::new();
            b.add_css_class("flat");
            let l = gtk::Label::new(Some(&format!(
                "{}  —  {}",
                dir_title(p),
                p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
            )));
            l.set_xalign(0.0);
            l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            l.set_max_width_chars(48);
            b.set_child(Some(&l));
            b
        };
        // Checking that each entry still exists touches the disk (maybe a stale
        // network mount), so the list fills in once that has run off the main thread.
        let folders = self.state.db.recent_locations(40).unwrap_or_default();
        let files = self.state.db.recent_files(40).unwrap_or_default();
        let loading = gtk::Label::new(Some("Loading…"));
        col.append(&loading);
        let (this, col2, pop3) = (self.clone(), col.clone(), pop.clone());
        glib::spawn_future_local(async move {
            let checked = gio::spawn_blocking(move || {
                let folders: Vec<_> = folders.into_iter().filter(|p| p.is_dir()).take(10).collect();
                let files: Vec<_> = files.into_iter().filter(|p| p.exists()).take(10).collect();
                (folders, files)
            })
            .await;
            let (folders, files) = checked.unwrap_or_default();
            let (col, pop, this) = (col2, pop3, this);
            col.remove(&loading);
            col.append(&heading("Recent Folders"));
            if folders.is_empty() {
                col.append(&gtk::Label::new(Some("Nothing yet")));
            }
            for p in folders {
                let b = entry(&p);
                let (w, pop2) = (Rc::downgrade(&this), pop.clone());
                b.connect_clicked(move |_| {
                    pop2.popdown();
                    if let Some(pane) = w.upgrade().and_then(|c| c.active_pane()) {
                        pane.navigate(p.clone());
                    }
                });
                col.append(&b);
            }
            col.append(&heading("Recent Files"));
            if files.is_empty() {
                col.append(&gtk::Label::new(Some("Nothing yet")));
            }
            for p in files {
                let b = entry(&p);
                let (w, pop2) = (Rc::downgrade(&this), pop.clone());
                b.connect_clicked(move |_| {
                    pop2.popdown();
                    if let Some(pane) = w.upgrade().and_then(|c| c.active_pane()) {
                        pane.launch(&p);
                    }
                });
                col.append(&b);
            }
        });
        let sw = gtk::ScrolledWindow::new();
        sw.set_child(Some(&col));
        sw.set_propagate_natural_height(true);
        sw.set_max_content_height(420);
        sw.set_min_content_width(320);
        pop.set_child(Some(&sw));
        pop
    }

    /// Rebuild window shortcuts from preferences. Bare printable keys (e.g.
    /// Space) are handled in the capture phase so list views don't swallow
    /// them, but never while a text entry has focus.
    pub fn apply_shortcuts(&self) {
        let Some(win) = self.win.upgrade() else { return };
        for old in self.shortcut_ctrl.take() {
            win.remove_controller(&old);
        }
        let normal = gtk::ShortcutController::new();
        normal.set_scope(gtk::ShortcutScope::Global);
        let bare = gtk::ShortcutController::new();
        bare.set_scope(gtk::ShortcutScope::Global);
        bare.set_propagation_phase(gtk::PropagationPhase::Capture);
        for def in ACTIONS {
            let accel = self.state.prefs.accel(def.id);
            let Some(trigger) = (!accel.is_empty()).then(|| gtk::ShortcutTrigger::parse_string(&accel)).flatten()
            else {
                continue;
            };
            let is_bare = gtk::accelerator_parse(&accel).is_some_and(|(k, m)| m.is_empty() && k.to_unicode().is_some());
            if is_bare {
                let (w, id) = (win.downgrade(), def.id);
                let action = gtk::CallbackAction::new(move |_, _| {
                    let Some(win) = w.upgrade() else { return glib::Propagation::Proceed };
                    if gtk::prelude::GtkWindowExt::focus(&win).is_some_and(|f| f.is::<gtk::Editable>()) {
                        return glib::Propagation::Proceed;
                    }
                    gio::prelude::ActionGroupExt::activate_action(&win, id, None);
                    glib::Propagation::Stop
                });
                bare.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
            } else {
                normal.add_shortcut(gtk::Shortcut::new(
                    Some(trigger),
                    Some(gtk::NamedAction::new(&format!("win.{}", def.id))),
                ));
            }
        }
        win.add_controller(normal.clone());
        win.add_controller(bare.clone());
        if let Some(app) = win.application() {
            crate::ui::menubar::install(&app, &self.state);
        }
        *self.shortcut_ctrl.borrow_mut() = vec![normal, bare];
    }
}
