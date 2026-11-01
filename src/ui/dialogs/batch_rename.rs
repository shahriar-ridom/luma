//! Batch rename with live preview. Conflicts disable Apply; nothing is
//! overwritten (see `core::operations::rename`).

use adw::prelude::*;
use std::{cell::RefCell, collections::HashSet, rc::Rc};

use crate::core::operations::rename::{CaseMode, Planned, Rule, Status, has_problems, plan};

pub fn show(parent: &gtk::Window, names: Vec<String>, existing: HashSet<String>, on_apply: Rc<dyn Fn(Vec<Planned>)>) {
    let modes = gtk::DropDown::from_strings(&[
        "Find and replace",
        "Add prefix",
        "Add suffix",
        "Number sequentially",
        "Change case",
    ]);

    // Per-mode inputs.
    let find = gtk::Entry::new();
    find.set_placeholder_text(Some("Find"));
    let with = gtk::Entry::new();
    with.set_placeholder_text(Some("Replace with"));
    let regex = gtk::CheckButton::with_label("Regular expression");
    let icase = gtk::CheckButton::with_label("Ignore case");
    let replace_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    for w in [find.upcast_ref::<gtk::Widget>(), with.upcast_ref()] {
        replace_box.append(w);
    }
    let opts = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    opts.append(&regex);
    opts.append(&icase);
    replace_box.append(&opts);

    let text = gtk::Entry::new();
    text.set_placeholder_text(Some("Text"));
    let text_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    text_box.append(&text);

    let pattern = gtk::Entry::new();
    pattern.set_text("File {n}");
    pattern.set_tooltip_text(Some("{n} = number, {name} = original name"));
    let start = gtk::SpinButton::with_range(0.0, 999_999.0, 1.0);
    start.set_value(1.0);
    let step = gtk::SpinButton::with_range(1.0, 1000.0, 1.0);
    let digits = gtk::SpinButton::with_range(0.0, 8.0, 1.0);
    digits.set_value(2.0);
    let num_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    num_box.append(&pattern);
    let nums = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for (l, w) in [("Start", &start), ("Step", &step), ("Digits", &digits)] {
        nums.append(&gtk::Label::new(Some(l)));
        nums.append(w);
    }
    num_box.append(&nums);

    let case = gtk::DropDown::from_strings(&["lowercase", "UPPERCASE", "Title Case"]);
    let case_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    case_box.append(&case);

    let stack = gtk::Stack::new();
    stack.add_named(&replace_box, Some("replace"));
    stack.add_named(&text_box, Some("prefix"));
    stack.add_named(&num_box, Some("number"));
    stack.add_named(&case_box, Some("case"));
    let with_ext = gtk::CheckButton::with_label("Include file extension");

    // Preview.
    let preview = gtk::ListBox::new();
    preview.set_selection_mode(gtk::SelectionMode::None);
    preview.add_css_class("boxed-list");
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&preview));
    sw.set_vexpand(true);
    sw.set_min_content_height(220);
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    let apply = gtk::Button::with_label("Rename");
    apply.add_css_class("suggested-action");
    apply.set_sensitive(false);

    let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
    {
        let m = &form;
        m.set_margin_top(12);
        m.set_margin_bottom(12);
        m.set_margin_start(12);
        m.set_margin_end(12);
    }
    form.append(&modes);
    form.append(&stack);
    form.append(&with_ext);
    form.append(&sw);
    form.append(&status);
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom.set_halign(gtk::Align::End);
    bottom.append(&apply);
    form.append(&bottom);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&form));
    let dialog = adw::Dialog::builder()
        .title(format!("Rename {} Items", names.len()))
        .content_width(560)
        .content_height(640)
        .child(&tv)
        .build();

    let current: Rc<RefCell<Vec<Planned>>> = Rc::default();
    let rule = {
        let (modes, find, with, regex, icase, text, pattern, start, step, digits, case) = (
            modes.clone(),
            find.clone(),
            with.clone(),
            regex.clone(),
            icase.clone(),
            text.clone(),
            pattern.clone(),
            start.clone(),
            step.clone(),
            digits.clone(),
            case.clone(),
        );
        Rc::new(move || -> Rule {
            match modes.selected() {
                0 => Rule::Replace {
                    find: find.text().to_string(),
                    with: with.text().to_string(),
                    regex: regex.is_active(),
                    ignore_case: icase.is_active(),
                },
                1 => Rule::Prefix(text.text().to_string()),
                2 => Rule::Suffix(text.text().to_string()),
                3 => Rule::Number {
                    pattern: pattern.text().to_string(),
                    start: start.value() as u32,
                    step: step.value() as u32,
                    digits: digits.value() as usize,
                },
                _ => Rule::Case([CaseMode::Lower, CaseMode::Upper, CaseMode::Title][case.selected() as usize]),
            }
        })
    };
    let update = {
        let (names, existing, preview, status, apply, current, with_ext, rule, modes, stack) = (
            names.clone(),
            existing,
            preview.clone(),
            status.clone(),
            apply.clone(),
            current.clone(),
            with_ext.clone(),
            rule.clone(),
            modes.clone(),
            stack.clone(),
        );
        Rc::new(move || {
            stack.set_visible_child_name(["replace", "prefix", "prefix", "number", "case"][modes.selected() as usize]);
            let planned = plan(&names, &rule(), with_ext.is_active(), &existing);
            while let Some(c) = preview.first_child() {
                preview.remove(&c);
            }
            for p in &planned {
                let r = adw::ActionRow::new();
                r.set_use_markup(false);
                r.set_title(&p.old);
                r.set_subtitle(&match &p.status {
                    Status::Unchanged => "unchanged".to_string(),
                    Status::Ok => format!("→ {}", p.new),
                    Status::Duplicate => format!("→ {} (another item gets the same name)", p.new),
                    Status::Exists => format!("→ {} (a file with this name already exists)", p.new),
                    Status::Invalid(why) => format!("→ {} ({why})", p.new),
                });
                if matches!(p.status, Status::Duplicate | Status::Exists | Status::Invalid(_)) {
                    r.add_css_class("error");
                }
                preview.append(&r);
            }
            let changes = planned.iter().filter(|p| p.status == Status::Ok).count();
            let problems = has_problems(&planned);
            status.set_text(&if problems {
                "Fix the highlighted names before renaming.".to_string()
            } else if changes == 0 {
                "Nothing would change.".to_string()
            } else {
                format!("{changes} item{} will be renamed.", if changes == 1 { "" } else { "s" })
            });
            apply.set_sensitive(!problems && changes > 0);
            *current.borrow_mut() = planned;
        })
    };
    update();
    for e in [&find, &with, &text, &pattern] {
        let u = update.clone();
        e.connect_changed(move |_| u());
    }
    for c in [&regex, &icase, &with_ext] {
        let u = update.clone();
        c.connect_toggled(move |_| u());
    }
    for s in [&start, &step, &digits] {
        let u = update.clone();
        s.connect_value_changed(move |_| u());
    }
    for d in [&modes, &case] {
        let u = update.clone();
        d.connect_selected_notify(move |_| u());
    }
    let dd = dialog.clone();
    apply.connect_clicked(move |_| {
        let plan: Vec<Planned> = current.borrow().iter().filter(|p| p.status == Status::Ok).cloned().collect();
        dd.close();
        on_apply(plan);
    });
    dialog.present(Some(parent));
}
