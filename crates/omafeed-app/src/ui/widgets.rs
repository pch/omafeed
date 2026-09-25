//! Small widget constructors shared by the main window and dialogs.
use gtk::prelude::*;

pub fn label(text: &str, css: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    if !css.is_empty() {
        l.add_css_class(css);
    }
    l
}

pub fn icon(name: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(name);
    b.set_tooltip_text(Some(tip));
    b
}

pub fn scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

pub fn margins(w: &impl IsA<gtk::Widget>, n: i32) {
    w.set_margin_start(n);
    w.set_margin_end(n);
    w.set_margin_top(n);
    w.set_margin_bottom(n);
}
