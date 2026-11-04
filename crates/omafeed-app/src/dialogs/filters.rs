//! "Content filters" dialog: hide articles that match a word or a `/regex/`.
use super::Form;
use crate::ui::{Ui, widgets::label};
use adw::prelude::*;
use gtk::glib;
use omafeed_core::filters::validate;
use std::rc::Rc;

struct Panel {
    u: Rc<Ui>,
    list: gtk::Box,
    empty: gtk::Label,
    entry: gtk::Entry,
    error: gtk::Label,
}

impl Panel {
    fn render(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let filters = self.u.settings.borrow().filters.clone();
        for pattern in &filters {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("filter");
            let text = label(pattern, "filter-pattern");
            if pattern.len() > 1 && pattern.starts_with('/') && pattern.ends_with('/') {
                text.add_css_class("regex");
            }
            text.set_hexpand(true);
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            row.append(&text);
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            remove.set_tooltip_text(Some("Remove"));
            remove.add_css_class("flat");
            remove.set_valign(gtk::Align::Center);
            // Weak: the panel owns the list, and so this button.
            let panel = Rc::downgrade(self);
            let pattern = pattern.clone();
            remove.connect_clicked(move |_| {
                if let Some(panel) = panel.upgrade() {
                    panel.remove(&pattern);
                }
            });
            row.append(&remove);
            self.list.append(&row);
        }
        self.empty.set_visible(filters.is_empty());
    }

    fn apply(self: &Rc<Self>, filters: Vec<String>) {
        self.u.apply_filters(filters);
        self.render();
    }

    fn add(self: &Rc<Self>) {
        let text = self.entry.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let mut filters = self.u.settings.borrow().filters.clone();
        let problem = if filters.contains(&text) {
            Some("This filter is already in the list".to_string())
        } else {
            validate(&text).err().map(|e| format!("{e:#}"))
        };
        self.error.set_text(problem.as_deref().unwrap_or_default());
        self.error.set_visible(problem.is_some());
        if problem.is_none() {
            filters.push(text);
            self.entry.set_text("");
            self.apply(filters);
        }
    }

    fn remove(self: &Rc<Self>, pattern: &str) {
        let mut filters = self.u.settings.borrow().filters.clone();
        filters.retain(|f| f != pattern);
        self.apply(filters);
    }
}

/// Edit the content filters; each change applies at once.
pub fn filters(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let form = Form::new(&u.window, "Content filters", "Done").without_cancel();
        form.note(
            "Hide articles whose title or preview contains a word, or matches a /regex/. \
             Case is ignored unless a regex turns it back on with (?-i). Starred articles \
             stay visible.",
        );
        let input = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        input.add_css_class("form-input-row");
        let entry = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text("A word, or /regex/")
            .build();
        input.append(&entry);
        let add = gtk::Button::with_label("Add");
        input.append(&add);
        form.field("New filter", &input);
        let error = label("", "form-error");
        error.add_css_class("inline");
        error.set_wrap(true);
        error.set_visible(false);
        form.append(&error);
        form.section("FILTERS");
        let empty = label("No filters yet", "form-note");
        form.append(&empty);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        form.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(280)
                .child(&list)
                .build(),
        );
        let panel = Rc::new(Panel {
            u,
            list,
            empty,
            entry: entry.clone(),
            error,
        });
        panel.render();
        // The panel holds the dialog's widgets, so their handlers hold it weakly; this task
        // keeps it until the dialog closes.
        add.connect_clicked(glib::clone!(
            #[weak]
            panel,
            move |_| panel.add()
        ));
        entry.connect_activate(glib::clone!(
            #[weak]
            panel,
            move |_| panel.add()
        ));
        entry.grab_focus();
        form.run().await;
        drop(panel);
    });
}
