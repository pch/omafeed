//! The Miniflux account: connecting the library to one, or, while connected, how its syncs
//! are going and disconnecting it.
use super::{Form, confirm_as};
use crate::{
    plural,
    ui::{Ui, widgets::label},
};
use adw::prelude::*;
use gtk::glib;
use omafeed_core::model::Account;
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

async fn connect(u: &Rc<Ui>) {
    let form = Form::new(&u.window, "Connect to Miniflux", "Connect");
    form.note(
        "While connected, your Miniflux server fetches the feeds and decides which feeds and \
         folders omafeed shows, and read and starred articles stay the same on both. Your feeds \
         Miniflux does not have yet are added to it.",
    );
    let server = form.entry("Server address", "");
    server.set_placeholder_text(Some("https://miniflux.example.org"));
    let key = gtk::PasswordEntry::builder()
        .show_peek_icon(true)
        .activates_default(true)
        .build();
    form.field("API key", &key);
    form.note("Make one in Miniflux under Settings → API Keys. omafeed keeps it in your keyring.");
    form.focus(&server);
    while form.run().await {
        let address = server.text().trim().to_owned();
        let token = key.text().trim().to_owned();
        if address.is_empty() || token.is_empty() {
            form.show_error("Enter your Miniflux server's address and an API key");
            continue;
        }
        match u.connect_account(address, token).await {
            Ok(done) => {
                let r = &done.report;
                let mut text = format!(
                    "Connected to Miniflux as {} · {}",
                    done.user.username,
                    plural(r.feeds, "feed")
                );
                if r.subscribed > 0 {
                    text.push_str(&format!(" · {} added to Miniflux", r.subscribed));
                }
                u.toast(&text);
                if !r.refused.is_empty() {
                    refused(u, &r.refused);
                }
                return;
            }
            Err(e) => {
                let connected = u.db.call(|s| s.account()).await.ok().flatten().is_some();
                if connected {
                    // The key worked, and only the first sync failed: the next one finishes.
                    u.error(format!(
                        "Connected to Miniflux, but the first sync failed: {e:#}. omafeed \
                         tries again when you refresh."
                    ));
                    return;
                }
                form.show_error(&format!("{e:#}"));
            }
        }
    }
}

/// List the feeds Miniflux would not subscribe to, which stay in the library with the reason.
fn refused(u: &Rc<Ui>, feeds: &[(String, String)]) {
    let lines: Vec<String> = feeds
        .iter()
        .map(|(title, reason)| format!("{title}: {reason}"))
        .collect();
    let d = adw::AlertDialog::builder()
        .heading("Miniflux would not add some feeds")
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
    let form = Form::new(&u.window, "Miniflux account", "Disconnect…");
    form.cancel.set_label("Close");
    form.save.remove_css_class("suggested-action");
    form.save.add_css_class("destructive-action");
    form.note(
        "Miniflux fetches the feeds and decides which feeds and folders there are; add, edit \
         and remove them there. omafeed syncs with it whenever it would refresh.",
    );
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
    let mut body = "Your feeds, folders and articles stay in omafeed, and omafeed fetches the \
                    feeds itself again. The API key is removed from your keyring."
        .to_owned();
    if account.unsent > 0 {
        let changes = usize::try_from(account.unsent).unwrap_or_default();
        body.push_str(&format!(
            "\n\n{} not yet sent will go to Miniflux first, if it can be reached; \
             otherwise they stay only here.",
            plural(changes, "read or star change")
        ));
    }
    if !confirm_as(
        &form.dialog,
        "Disconnect from Miniflux?",
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
                "Disconnected from Miniflux; omafeed fetches your feeds itself now".to_owned();
            if done.dropped > 0 {
                let dropped = usize::try_from(done.dropped).unwrap_or_default();
                text.push_str(&format!(" · {} not sent", plural(dropped, "change")));
            }
            u.toast(&text);
            if let Some(e) = done.keyring_error {
                u.error(format!(
                    "Could not remove the API key from the keyring: {e:#}"
                ));
            }
        }
        Err(e) => u.error(format!("{e:#}")),
    }
}
