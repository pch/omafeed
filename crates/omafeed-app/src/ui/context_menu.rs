//! Context menus of list rows: opened by a right click, or by the Menu key or Shift+F10 on
//! the focused row, and filled afresh each time from sections of commands.
use gtk::{gdk, gio, glib, prelude::*};

/// One command in a context menu.
pub(super) struct Item {
    pub label: String,
    /// The action's name, under the menu's prefix.
    pub name: &'static str,
    pub enabled: bool,
    pub run: Box<dyn Fn()>,
}

/// Call `open` with the row to show a menu for, and where in `list` the click was (`None`
/// for a key, which puts the menu below the row).
pub(super) fn connect(
    list: &gtk::ListBox,
    open: impl Fn(&gtk::ListBoxRow, Option<(f64, f64)>) + 'static,
) {
    let open = std::rc::Rc::new(open);
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    click.connect_pressed(glib::clone!(
        #[weak]
        list,
        #[strong]
        open,
        move |gesture, _, x, y| {
            #[expect(clippy::cast_possible_truncation, reason = "pixel coordinates")]
            if let Some(row) = list.row_at_y(y as i32) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                open(&row, Some((x, y)));
            }
        }
    ));
    list.add_controller(click);
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        list,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, mods| {
            let menu_key = key == gdk::Key::Menu
                || (key == gdk::Key::F10 && mods.contains(gdk::ModifierType::SHIFT_MASK));
            let row = list
                .focus_child()
                .and_downcast::<gtk::ListBoxRow>()
                .or_else(|| list.selected_row());
            match row.filter(|_| menu_key) {
                Some(row) => {
                    open(&row, None);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        }
    ));
    list.add_controller(keys);
}

/// Fill `menu` with `sections` as actions under `prefix`, and show it at `at` in `list`'s
/// coordinates or, without it, below `row`. `menu` is parented to `root`, an ancestor of
/// `list`.
pub(super) fn show(
    menu: &gtk::PopoverMenu,
    prefix: &str,
    sections: Vec<Vec<Item>>,
    root: &gtk::Widget,
    list: &gtk::ListBox,
    row: &gtk::ListBoxRow,
    at: Option<(f64, f64)>,
) {
    let model = gio::Menu::new();
    let group = gio::SimpleActionGroup::new();
    for section in sections {
        let part = gio::Menu::new();
        for item in section {
            part.append(Some(&item.label), Some(&format!("{prefix}.{}", item.name)));
            let action = gio::SimpleAction::new(item.name, None);
            action.set_enabled(item.enabled);
            let run = item.run;
            action.connect_activate(move |_, _| run());
            group.add_action(&action);
        }
        model.append_section(None, &part);
    }
    menu.set_menu_model(Some(&model));
    menu.insert_action_group(prefix, Some(&group));
    #[expect(clippy::cast_possible_truncation, reason = "pixel coordinates")]
    let target = match at {
        Some((x, y)) => list
            .compute_point(root, &gtk::graphene::Point::new(x as f32, y as f32))
            .map(|p| gtk::graphene::Rect::new(p.x(), p.y(), 1.0, 1.0)),
        None => row.compute_bounds(root),
    };
    if let Some(r) = target {
        #[expect(clippy::cast_possible_truncation, reason = "pixel coordinates")]
        menu.set_pointing_to(Some(&gdk::Rectangle::new(
            r.x() as i32,
            r.y() as i32,
            (r.width() as i32).max(1),
            (r.height() as i32).max(1),
        )));
    }
    menu.popup();
}

/// Close `menu` and drop its actions, so none outlives what it was opened on.
pub(super) fn close(menu: &gtk::PopoverMenu, prefix: &str) {
    menu.popdown();
    menu.insert_action_group(prefix, None::<&gio::ActionGroup>);
}

/// A popover for context menus, parented to `root` rather than to a list box, which would
/// take it for a row.
pub(super) fn popover(root: &impl IsA<gtk::Widget>) -> gtk::PopoverMenu {
    let menu = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
    menu.set_has_arrow(false);
    menu.set_halign(gtk::Align::Start);
    menu.set_parent(root);
    menu
}
