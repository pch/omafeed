//! The sync account (Miniflux or FreshRSS): connecting the library to one, or, while
//! connected, how its syncs are going and disconnecting it.
use super::{Form, confirm_as};
use crate::{
    plural,
    ui::{Ui, widgets::label},
};
use adw::prelude::*;
use gtk::glib;
use omafeed_core::model::{Account, Service};
use std::rc::Rc;

/// Open the account dialog: a form to connect, or the connected account.
pub fn account(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let account = u.library.borrow().account.clone();
        match account {
            Some(account) => connected(&u, &account).await,
            None => connect(&u).await,
        }
    });
}

/// What the connect form says for each service: a placeholder for the address, and where
/// to find the secret.
fn hints(service: Service) -> (&'static str, &'static str) {
    match service {
        Service::Miniflux => (
            "https://miniflux.example.org",
            "Make one in Miniflux under Settings → API Keys. omafeed keeps it in your keyring.",
        ),
        Service::FreshRss => (
            "https://freshrss.example.org",
            "Set one in FreshRSS under Settings → Profile → API management, after allowing \
             API access under Settings → Authentication. omafeed keeps it in your keyring.",
        ),
    }
}

async fn connect(u: &Rc<Ui>) {
    let form = Form::new(&u.window, "Connect a sync account", "Connect");
    form.note(
        "While connected, your Miniflux or FreshRSS server fetches the feeds and decides which \
         feeds and folders omafeed shows, and read and starred articles stay the same on both. \
         Your feeds the server does not have yet are added to it.",
    );
    let names: Vec<&str> = Service::ALL.iter().map(|s| s.name()).collect();
    let choice = form.choice("Service", &names, 0);
    let server = form.entry("Server address", "");
    let username = gtk::Entry::builder().activates_default(true).build();
    let (user_field, _) = form.labeled("User name", &username);
    let key = gtk::PasswordEntry::builder()
        .show_peek_icon(true)
        .activates_default(true)
        .build();
    let (_, key_label) = form.labeled("API key", &key);
    let hint = label("", "form-note");
    hint.set_wrap(true);
    form.append(&hint);
    let selected = {
        let choice = choice.clone();
        move || {
            Service::ALL
                .get(choice.selected() as usize)
                .copied()
                .unwrap_or_default()
        }
    };
    let show = {
        let (server, user_field, key_label, hint) = (
            server.clone(),
            user_field.clone(),
            key_label.clone(),
            hint.clone(),
        );
        let selected = selected.clone();
        move || {
            let service = selected();
            let (placeholder, note) = hints(service);
            server.set_placeholder_text(Some(placeholder));
            user_field.set_visible(service == Service::FreshRss);
            key_label.set_text(crate::account::secret_name(service));
            hint.set_text(note);
        }
    };
    show();
    choice.connect_selected_notify(move |_| show());
    form.focus(&server);
    while form.run().await {
        let service = selected();
        let secret = crate::account::secret_name(service);
        let address = server.text().trim().to_owned();
        let user = username.text().trim().to_owned();
        let token = key.text().to_string();
        let needs_user = service == Service::FreshRss;
        if address.is_empty() || token.trim().is_empty() || (needs_user && user.is_empty()) {
            let who = if needs_user { ", your user name" } else { "" };
            form.show_error(&format!(
                "Enter your {service} server's address{who} and an {secret}"
            ));
            continue;
        }
        let token = match service {
            Service::Miniflux => token.trim().to_owned(),
            Service::FreshRss => token,
        };
        match u.connect_account(service, address, user, token).await {
            Ok(done) => {
                let r = &done.report;
                let mut text = format!(
                    "Connected to {service} as {} · {}",
                    done.user.username,
                    plural(r.feeds, "feed")
                );
                if r.subscribed > 0 {
                    text.push_str(&format!(" · {} added to {service}", r.subscribed));
                }
                u.toast(&text);
                if !r.refused.is_empty() {
                    refused(u, service, &r.refused);
                }
                return;
            }
            Err(e) => {
                let connected = u.db.call(|s| s.account()).await.ok().flatten().is_some();
                if connected {
                    // The secret worked, and only the first sync failed: the next one finishes.
                    u.error(format!(
                        "Connected to {service}, but the first sync failed: {e:#}. omafeed \
                         tries again when you refresh."
                    ));
                    return;
                }
                form.show_error(&format!("{e:#}"));
            }
        }
    }
}

/// List the feeds the server would not subscribe to, which stay in the library with the
/// reason.
pub(super) fn refused(u: &Rc<Ui>, service: Service, feeds: &[(String, String)]) {
    let lines: Vec<String> = feeds
        .iter()
        .map(|(title, reason)| format!("{title}: {reason}"))
        .collect();
    let d = adw::AlertDialog::builder()
        .heading(format!("{service} would not add some feeds"))
        .body(format!(
            "They stay in omafeed, marked with the reason, until you remove or fix them.\n\n{}",
            lines.join("\n")
        ))
        .build();
    d.add_response("ok", "OK");
    d.present(Some(&u.window));
}

/// When `time` (Unix seconds) was, in local time.
fn local_time(time: i64) -> String {
    chrono::DateTime::from_timestamp(time, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// A value shown beside a setting's name.
fn value(text: &str) -> gtk::Label {
    let value = label(text, "dim-label");
    value.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    value.set_selectable(true);
    value
}

async fn connected(u: &Rc<Ui>, account: &Account) {
    let service = account.service;
    let form = Form::new(&u.window, &format!("{service} account"), "Disconnect…");
    form.cancel.set_label("Close");
    form.save.remove_css_class("suggested-action");
    form.save.add_css_class("destructive-action");
    let mut note = format!(
        "{service} fetches the feeds and keeps the list of them. Feeds and categories you add, \
         edit or remove here change in {service} too. omafeed syncs with it whenever it would \
         refresh."
    );
    if service == Service::FreshRss {
        note.push_str(
            " FreshRSS keeps every feed in a category: feeds you put at the top level go into \
             its default one, and a feed's address cannot be changed from here.",
        );
    }
    form.note(&note);
    form.setting("Server", None, &value(&account.server));
    form.setting("Signed in as", None, &value(&account.username));
    let last = account
        .last_sync
        .map_or_else(|| "Never".to_owned(), local_time);
    form.setting("Last sync", None, &value(&last));
    if account.unsent > 0 {
        form.setting(
            "Not sent yet",
            Some("Read and star changes the next sync sends"),
            &value(&account.unsent.to_string()),
        );
    }
    if let Some(error) = &account.error {
        form.show_error(&format!("The last sync failed: {error}"));
    }
    if !form.run().await {
        return;
    }
    let mut body = format!(
        "Your feeds, folders and articles stay in omafeed, and omafeed fetches the feeds itself \
         again. The {} is removed from your keyring.",
        crate::account::secret_name(service)
    );
    if account.unsent > 0 {
        let changes = usize::try_from(account.unsent).unwrap_or_default();
        body.push_str(&format!(
            "\n\n{} not yet sent will go to {service} first, if it can be reached; \
             otherwise they stay only here.",
            plural(changes, "read or star change")
        ));
    }
    if !confirm_as(
        &form.dialog,
        &format!("Disconnect from {service}?"),
        &body,
        "Disconnect",
    )
    .await
    {
        return;
    }
    drop(form);
    match u.disconnect_account().await {
        Ok(done) => {
            let mut text =
                format!("Disconnected from {service}; omafeed fetches your feeds itself now");
            if done.dropped > 0 {
                let dropped = usize::try_from(done.dropped).unwrap_or_default();
                text.push_str(&format!(" · {} not sent", plural(dropped, "change")));
            }
            u.toast(&text);
            if let Some(e) = done.keyring_error {
                u.error(format!(
                    "Could not remove the {} from the keyring: {e:#}",
                    crate::account::secret_name(service)
                ));
            }
        }
        Err(e) => u.error(format!("{e:#}")),
    }
}
