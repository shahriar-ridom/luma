//! Global menu over `com.canonical.dbusmenu`.
//!
//! The menubar model from `menubar.rs` is served as a dbusmenu tree on the
//! session bus and announced to `com.canonical.AppMenu.Registrar` (what Plasma,
//! and custom shells like Quickshell bars, implement). Item state is read from
//! the active window's actions, and clicks activate those actions.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::glib::{self, Variant, VariantTy};
use gtk::{gio, prelude::*};

use crate::application::state::AppState;
use crate::ui::menubar;

const REG_NAME: &str = "com.canonical.AppMenu.Registrar";
const REG_PATH: &str = "/com/canonical/AppMenu/Registrar";
const IFACE: &str = "com.canonical.dbusmenu";
const PATH: &str = "/MenuBar";

const XML: &str = r#"<node><interface name="com.canonical.dbusmenu">
  <method name="GetLayout">
    <arg type="i" name="parentId" direction="in"/><arg type="i" name="recursionDepth" direction="in"/>
    <arg type="as" name="propertyNames" direction="in"/>
    <arg type="u" name="revision" direction="out"/><arg type="(ia{sv}av)" name="layout" direction="out"/>
  </method>
  <method name="GetGroupProperties">
    <arg type="ai" name="ids" direction="in"/><arg type="as" name="propertyNames" direction="in"/>
    <arg type="a(ia{sv})" name="properties" direction="out"/>
  </method>
  <method name="GetProperty">
    <arg type="i" name="id" direction="in"/><arg type="s" name="name" direction="in"/>
    <arg type="v" name="value" direction="out"/>
  </method>
  <method name="Event">
    <arg type="i" name="id" direction="in"/><arg type="s" name="eventId" direction="in"/>
    <arg type="v" name="data" direction="in"/><arg type="u" name="timestamp" direction="in"/>
  </method>
  <method name="EventGroup">
    <arg type="a(isvu)" name="events" direction="in"/><arg type="ai" name="idsNotFound" direction="out"/>
  </method>
  <method name="AboutToShow">
    <arg type="i" name="id" direction="in"/><arg type="b" name="needUpdate" direction="out"/>
  </method>
  <method name="AboutToShowGroup">
    <arg type="ai" name="ids" direction="in"/>
    <arg type="ai" name="updatesNeeded" direction="out"/><arg type="ai" name="idsNotFound" direction="out"/>
  </method>
  <signal name="ItemsPropertiesUpdated"><arg type="a(ia{sv})"/><arg type="a(ias)"/></signal>
  <signal name="LayoutUpdated"><arg type="u" name="revision"/><arg type="i" name="parent"/></signal>
  <signal name="ItemActivationRequested"><arg type="i" name="id"/><arg type="u" name="timestamp"/></signal>
  <property name="Version" type="u" access="read"/>
  <property name="TextDirection" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconThemePath" type="as" access="read"/>
</interface></node>"#;

struct Node {
    id: i32,
    props: HashMap<String, Variant>,
    children: Vec<Node>,
    /// `(action, target)` for activatable leaves.
    action: Option<(String, Option<Variant>)>,
}

impl Node {
    fn variant(&self, depth: i32) -> Variant {
        let kids: Vec<Variant> = if depth == 0 {
            Vec::new()
        } else {
            self.children.iter().map(|c| Variant::from_variant(&c.variant(depth - 1))).collect()
        };
        Variant::tuple_from_iter([
            self.id.to_variant(),
            self.props.to_variant(),
            Variant::array_from_iter_with_type(VariantTy::VARIANT, kids),
        ])
    }

    fn find(&self, id: i32) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }
}

struct Ctx {
    app: gtk::Application,
    state: Rc<AppState>,
    conn: gio::DBusConnection,
    revision: Cell<u32>,
    refresh_queued: Cell<bool>,
}

fn props<const N: usize>(pairs: [(&str, Variant); N]) -> HashMap<String, Variant> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// GTK accelerator string -> dbusmenu shortcut (`[["Control", "n"]]`).
fn shortcut(accel: &str) -> Option<Variant> {
    let (key, mods) = gtk::accelerator_parse(accel)?;
    let mut keys: Vec<String> = Vec::new();
    for (m, name) in [
        (gtk::gdk::ModifierType::CONTROL_MASK, "Control"),
        (gtk::gdk::ModifierType::SHIFT_MASK, "Shift"),
        (gtk::gdk::ModifierType::ALT_MASK, "Alt"),
        (gtk::gdk::ModifierType::SUPER_MASK, "Super"),
    ] {
        if mods.contains(m) {
            keys.push(name.to_string());
        }
    }
    keys.push(key.name()?.to_string());
    Some(vec![keys].to_variant())
}

impl Ctx {
    /// The group an action name like `win.copy` belongs to.
    fn group_for(&self, full: &str) -> Option<(gio::ActionGroup, String)> {
        if let Some(name) = full.strip_prefix("app.") {
            return Some((self.app.clone().upcast(), name.to_string()));
        }
        let name = full.strip_prefix("win.")?;
        let win = self.app.active_window()?;
        Some((win.dynamic_cast::<gio::ActionGroup>().ok()?, name.to_string()))
    }

    fn leaf(&self, model: &gio::MenuModel, i: i32, next: &mut i32) -> Node {
        let id = *next;
        *next += 1;
        let label = model
            .item_attribute_value(i, "label", Some(VariantTy::STRING))
            .and_then(|v| v.get::<String>())
            .unwrap_or_default();
        let action = model.item_attribute_value(i, "action", Some(VariantTy::STRING)).and_then(|v| v.get::<String>());
        let target = model.item_attribute_value(i, "target", None);
        let mut p = props([("label", label.to_variant())]);
        if let Some(sc) = model
            .item_attribute_value(i, "accel", Some(VariantTy::STRING))
            .and_then(|v| v.get::<String>())
            .and_then(|a| shortcut(&a))
        {
            p.insert("shortcut".into(), sc);
        }
        let mut enabled = false;
        if let Some((group, name)) = action.as_deref().and_then(|a| self.group_for(a)) {
            enabled = group.is_action_enabled(&name);
            if let Some(state) = group.action_state(&name) {
                if let Some(b) = state.get::<bool>() {
                    p.insert("toggle-type".into(), "checkmark".to_variant());
                    p.insert("toggle-state".into(), i32::from(b).to_variant());
                } else if let Some(t) = &target {
                    p.insert("toggle-type".into(), "radio".to_variant());
                    p.insert("toggle-state".into(), i32::from(&state == t).to_variant());
                }
            }
        }
        p.insert("enabled".into(), enabled.to_variant());
        Node { id, props: p, children: Vec::new(), action: action.map(|a| (a, target)) }
    }

    fn fill(&self, model: &gio::MenuModel, next: &mut i32, out: &mut Vec<Node>) {
        for i in 0..model.n_items() {
            if let Some(section) = model.item_link(i, "section") {
                let mut inner = Vec::new();
                self.fill(&section, next, &mut inner);
                if !inner.is_empty() {
                    if !out.is_empty() {
                        let id = *next;
                        *next += 1;
                        out.push(Node {
                            id,
                            props: props([("type", "separator".to_variant())]),
                            children: Vec::new(),
                            action: None,
                        });
                    }
                    out.append(&mut inner);
                }
            } else if let Some(sub) = model.item_link(i, "submenu") {
                let mut n = self.leaf(model, i, next);
                n.action = None;
                n.props.insert("enabled".into(), true.to_variant());
                n.props.insert("children-display".into(), "submenu".to_variant());
                self.fill(&sub, next, &mut n.children);
                out.push(n);
            } else {
                out.push(self.leaf(model, i, next));
            }
        }
    }

    /// Whole tree, rebuilt from the current model and action states.
    fn tree(&self) -> Node {
        let model = menubar::build(&self.state);
        let mut next = 1;
        let mut root = Node {
            id: 0,
            props: props([("children-display", "submenu".to_variant())]),
            children: Vec::new(),
            action: None,
        };
        self.fill(model.upcast_ref(), &mut next, &mut root.children);
        root
    }

    fn signal(&self, name: &str, params: &Variant) {
        if let Err(e) = self.conn.emit_signal(None, PATH, IFACE, name, Some(params)) {
            tracing::debug!("dbusmenu {name}: {e}");
        }
    }

    /// Tell the host to re-read the menu (coalesced; action changes come in bursts).
    fn queue_refresh(self: &Rc<Self>) {
        if self.refresh_queued.replace(true) {
            return;
        }
        let me = self.clone();
        glib::idle_add_local_once(move || {
            me.refresh_queued.set(false);
            let rev = me.revision.get() + 1;
            me.revision.set(rev);
            me.signal("LayoutUpdated", &(rev, 0i32).to_variant());
        });
    }

    fn activate(&self, id: i32) {
        let tree = self.tree();
        let Some((full, target)) = tree.find(id).and_then(|n| n.action.clone()) else { return };
        if let Some((group, name)) = self.group_for(&full)
            && group.is_action_enabled(&name)
        {
            group.activate_action(&name, target.as_ref());
        }
    }

    fn call(self: &Rc<Self>, method: &str, params: &Variant) -> Option<Variant> {
        match method {
            "GetLayout" => {
                let (parent, depth, _names): (i32, i32, Vec<String>) = params.get()?;
                let tree = self.tree();
                let node = tree.find(parent)?;
                Some(Variant::tuple_from_iter([self.revision.get().to_variant(), node.variant(depth)]))
            }
            "GetGroupProperties" => {
                let (ids, _names): (Vec<i32>, Vec<String>) = params.get()?;
                let tree = self.tree();
                let items: Vec<Variant> = ids
                    .iter()
                    .filter_map(|id| tree.find(*id))
                    .map(|n| Variant::tuple_from_iter([n.id.to_variant(), n.props.to_variant()]))
                    .collect();
                let arr = Variant::array_from_iter_with_type(VariantTy::new("(ia{sv})").ok()?, items);
                Some(Variant::tuple_from_iter([arr]))
            }
            "GetProperty" => {
                let (id, name): (i32, String) = params.get()?;
                let tree = self.tree();
                let v = tree.find(id)?.props.get(&name)?.clone();
                Some(Variant::tuple_from_iter([Variant::from_variant(&v)]))
            }
            "Event" => {
                let (id, event, _data, _ts): (i32, String, Variant, u32) = params.get()?;
                if event == "clicked" {
                    self.activate(id);
                }
                Some(Variant::tuple_from_iter(Vec::<Variant>::new()))
            }
            "EventGroup" => {
                let events: Vec<(i32, String, Variant, u32)> = params.get()?;
                for (id, event, _, _) in events {
                    if event == "clicked" {
                        self.activate(id);
                    }
                }
                Some((Vec::<i32>::new(),).to_variant())
            }
            // The host asks before opening a submenu: our tree is always fresh on GetLayout.
            "AboutToShow" => Some((true,).to_variant()),
            "AboutToShowGroup" => Some((Vec::<i32>::new(), Vec::<i32>::new()).to_variant()),
            _ => None,
        }
    }
}

fn register_with_host(conn: &gio::DBusConnection) {
    let path = glib::variant::ObjectPath::try_from(PATH).expect("valid object path");
    // The host identifies the app by the sender's pid; the id only has to be unique.
    let params = (std::process::id(), path).to_variant();
    conn.call(
        Some(REG_NAME),
        REG_PATH,
        REG_NAME,
        "RegisterWindow",
        Some(&params),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |res| {
            if let Err(e) = res {
                tracing::debug!("global menu registration failed: {e}");
            }
        },
    );
}

/// Export the menu and keep it registered. A missing registrar is not an error:
/// registration happens whenever one appears (or restarts).
pub fn start(app: &gtk::Application, state: Rc<AppState>) {
    let Some(conn) = app.dbus_connection() else { return };
    let Ok(info) = gio::DBusNodeInfo::for_xml(XML) else { return };
    let Some(iface) = info.lookup_interface(IFACE) else { return };
    let ctx = Rc::new(Ctx {
        app: app.clone(),
        state,
        conn: conn.clone(),
        revision: Cell::new(1),
        refresh_queued: Cell::new(false),
    });

    let c = ctx.clone();
    let reg = conn
        .register_object(PATH, &iface)
        .method_call(move |_conn, _sender, _path, _iface, method, params, invocation| match c.call(method, &params) {
            Some(v) => invocation.return_value(Some(&v)),
            None => invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "bad call"),
        })
        .property(|_conn, _sender, _path, _iface, name| match name {
            "Version" => 3u32.to_variant(),
            "TextDirection" => "ltr".to_variant(),
            "Status" => "normal".to_variant(),
            _ => Vec::<String>::new().to_variant(),
        })
        .build();
    if let Err(e) = reg {
        tracing::warn!("could not export global menu: {e}");
        return;
    }

    // Enabled/checked states follow the active window's actions.
    let hook = {
        let ctx = ctx.clone();
        move |w: &gtk::Window| {
            let Some(g) = w.dynamic_cast_ref::<gio::ActionGroup>() else { return };
            let c = ctx.clone();
            g.connect_action_enabled_changed(None, move |_, _, _| c.queue_refresh());
            let c = ctx.clone();
            g.connect_action_state_changed(None, move |_, _, _| c.queue_refresh());
        }
    };
    for w in app.windows() {
        hook(&w);
    }
    let c = ctx.clone();
    app.connect_window_added(move |_, w| {
        hook(w);
        c.queue_refresh();
    });
    let c = ctx.clone();
    app.connect_active_window_notify(move |_| c.queue_refresh());

    let conn2 = conn.clone();
    gio::bus_watch_name_on_connection(
        &conn,
        REG_NAME,
        gio::BusNameWatcherFlags::NONE,
        move |_, _, _| register_with_host(&conn2),
        |_, _| {},
    );
}
