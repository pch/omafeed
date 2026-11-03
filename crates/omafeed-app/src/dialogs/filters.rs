//! "Content filters" window: hide articles that match a word or a `/regex/`.
use crate::ui::{Ui, widgets::margins};
use adw::prelude::*;
use gtk::glib;
use omafeed_core::filters::validate;
use std::rc::Rc;

struct Panel {
    u: Rc<Ui>,
    list: gtk::ListBox,
    entry: gtk::Entry,
    error: gtk::Label,
}

impl Panel {
    fn render(self: &Rc<Self>) {
        self.list.remove_all();
        for pattern in self.u.settings.borrow().filters.iter() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            margins(&row, 6);
            let label = gtk::Label::new(Some(pattern));
            label.set_hexpand(true);
            label.set_xalign(0.0);
            label.set_wrap(true);
            row.append(&label);
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            remove.set_tooltip_text(Some("Remove"));
            remove.add_css_class("flat");
            let panel = self.clone();
            let pattern = pattern.clone();
            remove.connect_clicked(move |_| panel.remove(&pattern));
            row.append(&remove);
            self.list.append(&row);
        }
        self.list.set_visible(self.list.first_child().is_some());
    }

    fn apply(self: &Rc<Self>, filters: Vec<String>) {
        self.u.apply_filters(filters);
        self.render();
    }

    fn add(self: &Rc<Self>) {
        let text = self.entry.text().trim().to_string();
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

pub fn filters(u: Rc<Ui>) {
    let window = gtk::Window::builder()
        .title("Content filters")
        .transient_for(&u.window)
        .modal(true)
        .default_width(480)
        .default_height(480)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    margins(&root, 16);
    let description = gtk::Label::new(Some(
        "Hide articles whose title or preview contains a word, or matches a /regex/. \
         Case is ignored. Starred articles stay visible.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    root.append(&description);
    let input = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text("word or /regex/")
        .build();
    input.append(&entry);
    let add = gtk::Button::with_label("Add");
    input.append(&add);
    root.append(&input);
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    error.set_xalign(0.0);
    error.add_css_class("error");
    error.set_visible(false);
    root.append(&error);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(gtk::Align::Start);
    root.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&list)
            .build(),
    );
    let close = gtk::Button::with_label("Done");
    close.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.close()
    ));
    root.append(&close);
    window.set_child(Some(&root));
    let panel = Rc::new(Panel {
        u,
        list,
        entry: entry.clone(),
        error,
    });
    panel.render();
    add.connect_clicked(glib::clone!(
        #[strong]
        panel,
        move |_| panel.add()
    ));
    entry.connect_activate(move |_| panel.add());
    window.present();
}
