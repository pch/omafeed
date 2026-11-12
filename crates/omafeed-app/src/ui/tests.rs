//! The desktop smoke test: the real window, driven through its widgets against a temporary
//! library and local servers. Built only with the `test-support` feature.
use super::*;
use std::time::{Duration, Instant};
use webkit6::prelude::*;
#[track_caller]
fn pump_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let context = glib::MainContext::default();
    while !condition() {
        assert!(Instant::now() < deadline, "GTK operation timed out");
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    while context.pending() {
        context.iteration(false);
    }
}
fn widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut all = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(c) = child {
        all.extend(widgets(&c));
        child = c.next_sibling();
    }
    all
}
fn button(root: &gtk::Widget, text: &str) -> gtk::Button {
    widgets(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.label().as_deref() == Some(text))
        .unwrap_or_else(|| panic!("Missing button {text}"))
}
fn all_widgets() -> Vec<gtk::Widget> {
    gtk::Window::list_toplevels()
        .iter()
        .flat_map(widgets)
        .collect()
}
fn form_button(text: &str) -> gtk::Button {
    all_widgets()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.label().as_deref() == Some(text))
        .unwrap_or_else(|| panic!("Missing form button {text}"))
}
/// The text field of the open form whose label starts with `title`.
fn entry_row(title: &str) -> Option<gtk::Entry> {
    all_widgets()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .find(|l| l.has_css_class("form-label") && l.text().starts_with(title))
        .and_then(|l| l.next_sibling())
        .and_then(|w| w.downcast::<gtk::Entry>().ok())
}
/// Whether a form is open.
fn open_form() -> bool {
    all_widgets().iter().any(|w| w.has_css_class("form"))
}
/// The current time, as a refresh would record it.
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub(crate) fn desktop_smoke() {
    // The Miniflux account below keeps its API key here: CI has no keyring.
    // SAFETY: the first thing the only test in its binary does, before GTK, WebKit or tokio
    // start threads that could read the environment.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var(crate::account::TOKEN_VAR, "smoke-test-key");
    }
    adw::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        data: dir.path().join("data"),
        config: dir.path().join("config"),
        cache: dir.path().join("cache"),
    };
    for p in [&paths.data, &paths.config, &paths.cache] {
        std::fs::create_dir_all(p).unwrap();
    }
    let db_path = paths.data.join("test.db");
    let db = Db::open(&db_path).unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let id = runtime
        .block_on(db.call(|s| {
            let folder = s.add_folder("Blogs", None)?;
            let f = s.add_feed("Fixture feed", "https://example.invalid/feed", Some(folder))?;
            let revision = s.library()?.feeds[0].revision;
            s.commit_download(
                f,
                revision,
                omafeed_core::model::Download::Updated(omafeed_core::model::Update {
                    entries: vec![omafeed_core::model::Entry {
                        identity: "one".into(),
                        title: "Fixture article".into(),
                        url: "https://example.invalid/post".into(),
                        author: "Author".into(),
                        published: Some(1_700_000_000),
                        html: "<p>Readable fixture content.</p>".into(),
                        text: "Readable fixture content.".into(),
                    }],
                    etag: None,
                    modified: None,
                    site_url: Some("https://example.invalid/".into()),
                }),
                now(),
                now() + 86400,
            )?;
            Ok(s.articles(&Query {
                scope: Scope::All,
                ..Default::default()
            })?[0]
                .id)
        }))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.pch.Omafeed.Smoke")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let u = Ui::build(
        &app,
        db.clone(),
        paths,
        Settings {
            remote_images: false,
            ..Default::default()
        },
        runtime.clone(),
        Refresher::new().unwrap(),
    );
    pump_until(|| !u.view.borrow().articles().is_empty());
    // The search box forgives typos. Changing the text first clears the box, which GTK reports
    // at once, so wait until the new text has been applied, then until the database has
    // answered, before looking at the list.
    let search = |text: &str| {
        u.header.search.set_text(text);
        pump_until(|| u.view.borrow().query.search == text);
        runtime.block_on(db.call(|_| Ok(()))).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        pump_until(|| true);
    };
    search("zzzzzzzz");
    assert!(u.view.borrow().articles().is_empty());
    search("fixtre contnt");
    assert!(
        u.view.borrow().articles().iter().any(|a| a.id == id),
        "a misspelled search should still find the article"
    );
    search("");
    assert!(u.view.borrow().articles().iter().any(|a| a.id == id));
    assert!(!u.list.previous.is_sensitive());
    assert!(!u.list.next.is_sensitive());
    // Content filters apply at once and refuse a pattern that cannot work.
    crate::dialogs::filters(u.clone());
    let dialog = Rc::new(RefCell::new(None::<adw::Dialog>));
    pump_until(|| {
        *dialog.borrow_mut() = all_widgets()
            .into_iter()
            .filter_map(|w| w.downcast::<adw::Dialog>().ok())
            .find(|d| d.title() == "Content filters");
        dialog.borrow().is_some()
    });
    let root = dialog.borrow().clone().unwrap().upcast::<gtk::Widget>();
    let entry = widgets(&root)
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Entry>().ok())
        .unwrap();
    entry.set_text("/(/");
    button(&root, "Add").emit_clicked();
    assert!(u.settings.borrow().filters.is_empty());
    assert_eq!(entry.text(), "/(/");
    entry.set_text("fixture");
    button(&root, "Add").emit_clicked();
    assert_eq!(u.settings.borrow().filters, ["fixture"]);
    assert_eq!(entry.text(), "");
    pump_until(|| u.view.borrow().articles().iter().all(|a| a.id != id));
    widgets(&root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.tooltip_text().as_deref() == Some("Remove"))
        .unwrap()
        .emit_clicked();
    assert!(u.settings.borrow().filters.is_empty());
    pump_until(|| u.view.borrow().articles().iter().any(|a| a.id == id));
    button(&root, "Done").emit_clicked();
    pump_until(|| !open_form());
    let expander = widgets(&u.side.list.clone().upcast())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.tooltip_text().as_deref() == Some("Collapse folder"))
        .unwrap();
    expander.emit_clicked();
    assert!(
        !u.sidebar_rows
            .borrow()
            .iter()
            .any(|r| matches!(r.scope, Scope::Feed(_)))
    );
    let expander = widgets(&u.side.list.clone().upcast())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.tooltip_text().as_deref() == Some("Expand folder"))
        .unwrap();
    expander.emit_clicked();
    assert!(
        u.sidebar_rows
            .borrow()
            .iter()
            .any(|r| matches!(r.scope, Scope::Feed(_)))
    );
    // Changes show at once; wait until the database has them too.
    let settled = || {
        pump_until(|| {
            let view = u.view.borrow();
            view.unfinished(id, changes::Field::Read).is_none()
                && view.unfinished(id, changes::Field::Starred).is_none()
        });
    };
    u.select(id);
    pump_until(|| u.view.borrow().open().is_some_and(|a| a.read));
    settled();
    u.toggle_star();
    assert!(u.view.borrow().open().is_some_and(|a| a.starred));
    settled();
    u.toggle_read();
    assert!(u.view.borrow().open().is_some_and(|a| !a.read));
    settled();
    // Each quick toggle flips what the one before it showed, before any write has finished,
    // and the last one is what gets saved.
    u.toggle_star();
    u.toggle_star();
    assert!(u.view.borrow().open().is_some_and(|a| a.starred));
    settled();
    assert!(u.view.borrow().open().is_some_and(|a| a.starred));
    assert!(
        runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .starred
    );
    // Marking the view read and undoing it updates the open article both times.
    u.mark_all();
    pump_until(|| u.view.borrow().open().is_some_and(|a| a.read));
    assert!(
        runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .read
    );
    u.undo();
    pump_until(|| {
        u.view.borrow().open().is_some_and(|a| !a.read)
            && u.view
                .borrow()
                .articles()
                .iter()
                .any(|a| a.id == id && !a.read)
    });
    assert!(
        !runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .read
    );
    u.render();
    pump_until(|| !u.reader.web.is_loading());
    let done = Rc::new(RefCell::new(None));
    let out = done.clone();
    u.reader.web.evaluate_javascript(
        "document.body.textContent.includes('Readable fixture content')",
        Some("omafeed"),
        None,
        None::<&gio::Cancellable>,
        move |r| {
            *out.borrow_mut() = Some(r.map(|v| v.to_boolean()));
        },
    );
    pump_until(|| done.borrow().is_some());
    assert!(done.borrow_mut().take().unwrap().unwrap());
    assert!(
        !runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .read,
        "Theme/reader reload must not mark an intentionally unread article read"
    );
    // Another program (the command line) changing the library shows up without any action
    // in the window. A second connection to the same file stands in for that program.
    assert!(
        u.view
            .borrow()
            .articles()
            .iter()
            .any(|a| a.id == id && !a.read)
    );
    let outsider = Db::open(&db_path).unwrap();
    runtime
        .block_on(outsider.call(move |s| s.set_read(id, true)))
        .unwrap();
    pump_until(|| {
        u.view
            .borrow()
            .articles()
            .iter()
            .all(|a| a.id != id || a.read)
            && u.view.borrow().open().is_some_and(|a| a.read)
    });
    assert!(
        widgets(&u.reader.root.clone().upcast())
            .iter()
            .any(|w| w.tooltip_text().as_deref() == Some("Mark unread (M)"))
    );
    runtime
        .block_on(outsider.call(move |s| s.set_read(id, false)))
        .unwrap();
    pump_until(|| {
        u.view
            .borrow()
            .articles()
            .iter()
            .any(|a| a.id == id && !a.read)
    });
    pump_until(|| u.view.borrow().open().is_some_and(|a| !a.read));
    runtime
        .block_on(outsider.call(move |s| s.set_starred(id, false)))
        .unwrap();
    pump_until(|| u.view.borrow().open().is_some_and(|a| !a.starred));
    assert!(
        widgets(&u.reader.root.clone().upcast())
            .iter()
            .any(|w| w.tooltip_text().as_deref() == Some("Star (S)"))
    );
    u.toggle_star();
    assert!(u.view.borrow().open().is_some_and(|a| a.starred));
    settled();
    assert!(
        runtime
            .block_on(outsider.call(move |s| s.article(id)))
            .unwrap()
            .starred
    );
    // Choosing a different view closes the article instead of keeping it open.
    let starred = u
        .sidebar_rows
        .borrow()
        .iter()
        .position(|r| r.scope == Scope::Starred)
        .unwrap();
    u.side
        .list
        .select_row(u.side.list.row_at_index(starred as i32).as_ref());
    pump_until(|| u.list.heading.text() == "Starred");
    assert!(u.view.borrow().open().is_none());
    assert!(u.settings.borrow().selected_article.is_none());
    // New folders, new feeds and edits go through forms that stay open until they succeed.
    WidgetExt::activate_action(&u.window, "win.new-folder", None).unwrap();
    pump_until(|| entry_row("Name").is_some());
    entry_row("Name").unwrap().set_text("Created through UI");
    form_button("Create").emit_clicked();
    pump_until(|| {
        runtime
            .block_on(db.call(|s| {
                Ok(s.library()?
                    .folders
                    .iter()
                    .any(|f| f.name == "Created through UI"))
            }))
            .unwrap()
    });
    pump_until(|| !open_form());
    let (fixture_feed, created) = runtime
        .block_on(db.call(|s| {
            let lib = s.library()?;
            let created = lib
                .folders
                .iter()
                .find(|f| f.name == "Created through UI")
                .unwrap()
                .id;
            Ok((lib.feeds[0].id, created))
        }))
        .unwrap();
    pump_until(|| {
        u.sidebar_rows
            .borrow()
            .iter()
            .any(|r| r.scope == Scope::Folder(created))
    });
    // A name already taken beside it is refused in the form, which stays open.
    WidgetExt::activate_action(&u.window, "win.new-folder", None).unwrap();
    pump_until(|| entry_row("Name").is_some());
    entry_row("Name").unwrap().set_text("Created through UI");
    form_button("Create").emit_clicked();
    pump_until(|| {
        all_widgets()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .any(|l| l.is_visible() && l.text() == "A folder with this name is already there")
    });
    assert!(open_form());
    form_button("Cancel").emit_clicked();
    pump_until(|| !open_form());
    // Dropping a feed on a folder moves it there.
    u.move_entry(Scope::Feed(fixture_feed), Some(created));
    pump_until(|| {
        runtime
            .block_on(db.call(move |s| Ok(s.library()?.feed(fixture_feed).unwrap().folder)))
            .unwrap()
            == Some(created)
    });
    u.move_entry(Scope::Feed(fixture_feed), None);
    pump_until(|| {
        runtime
            .block_on(db.call(move |s| Ok(s.library()?.feed(fixture_feed).unwrap().folder)))
            .unwrap()
            .is_none()
    });
    // Editing renames and moves it.
    crate::dialogs::edit(u.clone(), Scope::Feed(fixture_feed));
    pump_until(|| entry_row("Name").is_some());
    entry_row("Name").unwrap().set_text("Renamed through UI");
    let picker = all_widgets()
        .into_iter()
        .filter(|w| w.ancestor(adw::Dialog::static_type()).is_some())
        .find_map(|w| w.downcast::<gtk::DropDown>().ok())
        .unwrap();
    picker.set_selected(2);
    form_button("Save").emit_clicked();
    pump_until(|| {
        runtime
            .block_on(db.call(|s| {
                let lib = s.library()?;
                let f = &lib.feeds[0];
                Ok(f.title == "Renamed through UI"
                    && lib.folders.iter().any(|folder| {
                        Some(folder.id) == f.folder && folder.name == "Created through UI"
                    }))
            }))
            .unwrap()
    });
    pump_until(|| !open_form());
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let website = format!("http://{}/", listener.local_addr().unwrap());
    let server = runtime.spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let n = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..n]);
            let body = if request.starts_with("GET /rss/ ") {
                r#"<rss version="2.0"><channel><title>Discovered feed</title><description>Fixture</description></channel></rss>"#
            } else {
                r#"<html><head><link rel="alternate" type="application/rss+xml" href="/rss/"></head></html>"#
            };
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    // Wide enough for the layout that shows the sidebar, which Xvfb's window is not.
    u.window.set_size_request(1300, 850);
    pump_until(|| u.side.root.is_visible());
    WidgetExt::activate_action(&u.window, "win.add-feed", None).unwrap();
    pump_until(|| entry_row("Address").is_some());
    assert!(entry_row("Name").unwrap().text().is_empty());
    entry_row("Address").unwrap().set_text(&website);
    form_button("Add").emit_clicked();
    pump_until(|| {
        runtime
            .block_on(db.call(|s| {
                Ok(s.library()?
                    .feeds
                    .iter()
                    .any(|f| f.title == "Discovered feed" && f.url.ends_with("/rss/")))
            }))
            .unwrap()
    });
    pump_until(|| !open_form());
    // The new feed is shown and selected in the sidebar.
    let added = runtime
        .block_on(db.call(|s| {
            Ok(s.library()?
                .feeds
                .iter()
                .find(|f| f.title == "Discovered feed")
                .unwrap()
                .id)
        }))
        .unwrap();
    assert_eq!(u.view.borrow().query.scope, Scope::Feed(added));
    pump_until(|| {
        let selected = u.side.list.selected_row().map(|r| r.index() as usize);
        selected
            .and_then(|i| u.sidebar_rows.borrow().get(i).map(|r| r.scope.clone()))
            .is_some_and(|scope| scope == Scope::Feed(added))
    });
    // Its row takes keyboard focus, and keeps it once the refresh that follows rebuilds
    // the sidebar.
    let focused = || {
        u.sidebar_rows
            .borrow()
            .iter()
            .find(|r| r.scope == Scope::Feed(added))
            .is_some_and(|r| r.row.is_focus())
    };
    pump_until(focused);
    pump_until(|| {
        !u.busy.get()
            && runtime
                .block_on(db.call(move |s| Ok(s.library()?.feed(added).unwrap().last_attempt)))
                .unwrap()
                .is_some()
    });
    // The reload after the refresh has landed.
    runtime.block_on(db.call(|_| Ok(()))).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    pump_until(|| true);
    assert!(focused());
    // Whichever finished first, a rebuild of the sidebar keeps it.
    u.rebuild_sidebar();
    assert!(focused());
    u.window.set_size_request(-1, -1);
    // Back to every article for the checks below.
    let all = u
        .sidebar_rows
        .borrow()
        .iter()
        .position(|r| r.scope == Scope::All)
        .unwrap();
    u.side
        .list
        .select_row(u.side.list.row_at_index(all as i32).as_ref());
    pump_until(|| u.view.borrow().articles().iter().any(|a| a.id == id));
    // Subscribing to it again closes the form without adding anything.
    WidgetExt::activate_action(&u.window, "win.add-feed", None).unwrap();
    pump_until(|| entry_row("Address").is_some());
    entry_row("Address").unwrap().set_text(&website);
    form_button("Add").emit_clicked();
    pump_until(|| !open_form());
    assert_eq!(
        runtime
            .block_on(db.call(|s| Ok(s.library()?.feeds.len())))
            .unwrap(),
        2
    );
    server.abort();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let slow_url = format!("http://{}/", listener.local_addr().unwrap());
    let slow = runtime.spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    WidgetExt::activate_action(&u.window, "win.add-feed", None).unwrap();
    pump_until(|| entry_row("Address").is_some());
    entry_row("Address").unwrap().set_text(&slow_url);
    form_button("Add").emit_clicked();
    pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Spinner>()));
    // Cancelling the search leaves the form open for another try.
    let progress = all_widgets()
        .into_iter()
        .find(|w| w.is::<gtk::Spinner>())
        .and_then(|w| w.ancestor(adw::AlertDialog::static_type()))
        .unwrap();
    button(&progress, "Cancel").emit_clicked();
    pump_until(|| !all_widgets().iter().any(|w| w.is::<gtk::Spinner>()));
    assert!(open_form());
    assert_eq!(
        runtime
            .block_on(db.call(|s| Ok(s.library()?.feeds.len())))
            .unwrap(),
        2
    );
    form_button("Cancel").emit_clicked();
    pump_until(|| !open_form());
    slow.abort();
    // The sidebar's context menu, as a right click or the Menu key opens it, does its work
    // without changing the view shown.
    let (fixture, discovered, folder) = runtime
        .block_on(db.call(move |s| {
            let lib = s.library()?;
            let fixture = s.article(id)?.feed_id;
            let discovered = lib
                .feeds
                .iter()
                .find(|f| f.title == "Discovered feed")
                .unwrap()
                .id;
            Ok((
                fixture,
                discovered,
                lib.feed(fixture).unwrap().folder.unwrap(),
            ))
        }))
        .unwrap();
    pump_until(|| {
        u.sidebar_rows
            .borrow()
            .iter()
            .any(|r| r.scope == Scope::Feed(discovered))
    });
    let open_menu = |scope: Scope| -> Vec<String> {
        u.side.menu.popdown();
        let index = u
            .sidebar_rows
            .borrow()
            .iter()
            .position(|r| r.scope == scope)
            .unwrap();
        let row = u.side.list.row_at_index(index as i32).unwrap();
        u.open_sidebar_menu(&row, None);
        // No main loop turn until the caller has run an action: a reload finishing in between
        // would close the menu and drop its actions. (Whether the popover is mapped yet is up
        // to GTK; its actions work either way.)
        let model = u.side.menu.menu_model().unwrap();
        let mut labels = Vec::new();
        for i in 0..model.n_items() {
            let section = model.item_link(i, gio::MENU_LINK_SECTION).unwrap();
            for j in 0..section.n_items() {
                let label = section
                    .item_attribute_value(j, gio::MENU_ATTRIBUTE_LABEL, None)
                    .and_then(|v| v.get::<String>())
                    .unwrap();
                labels.push(label);
            }
        }
        labels
    };
    // Opens the menu each time, since anything that reads the library meanwhile closes it.
    let run = |scope: Scope, command: sidebar::Command| {
        open_menu(scope);
        u.side
            .menu
            .activate_action(&format!("sidebar.{}", command.name()), None)
            .unwrap();
    };
    let clipboard = || {
        let text = Rc::new(RefCell::new(None));
        let out = text.clone();
        let clipboard = u.window.clipboard();
        glib::spawn_future_local(async move {
            let read = clipboard.read_text_future().await;
            *out.borrow_mut() = Some(read.ok().flatten().map(|t| t.to_string()));
        });
        pump_until(|| text.borrow().is_some());
        text.borrow_mut().take().flatten()
    };
    assert_eq!(
        open_menu(Scope::Feed(fixture)),
        [
            "Open home page",
            "Copy feed URL",
            "Copy home page URL",
            "Refresh",
            "Mark all as read",
            "Edit…",
            "Remove…"
        ]
    );
    run(Scope::Feed(fixture), sidebar::Command::CopyFeedUrl);
    assert_eq!(clipboard().as_deref(), Some("https://example.invalid/feed"));
    run(Scope::Feed(fixture), sidebar::Command::CopyHomePageUrl);
    let website = u.library.borrow().feed(fixture).unwrap().site_url.clone();
    assert!(!website.is_empty());
    assert_eq!(clipboard(), Some(website));
    // Refreshing one feed downloads only it; its address does not resolve, so it fails.
    run(Scope::Feed(fixture), sidebar::Command::Refresh);
    pump_until(|| {
        u.header
            .refresh
            .tooltip_text()
            .is_some_and(|t| t.starts_with("Refreshed 1 feed at") && t.contains("1 unavailable"))
    });
    assert!(!u.busy.get());
    run(Scope::Feed(fixture), sidebar::Command::MarkRead);
    pump_until(|| {
        runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .read
    });
    u.undo();
    pump_until(|| {
        !runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .read
    });
    assert_eq!(
        open_menu(Scope::Folder(folder)),
        [
            "Refresh",
            "Mark all as read",
            "Add feed here…",
            "New folder inside…",
            "Edit…",
            "Remove…"
        ]
    );
    assert_eq!(open_menu(Scope::Starred), ["Mark all as read"]);
    // An article's own menu changes it without opening it, and can go to its feed.
    let article_menu = |id: omafeed_core::model::ArticleId| -> Vec<String> {
        let index = u
            .view
            .borrow()
            .articles()
            .iter()
            .position(|a| a.id == id)
            .unwrap();
        let row = u.list.list.row_at_index(index as i32).unwrap();
        u.open_article_menu(&row, None);
        let model = u.list.menu.menu_model().unwrap();
        let mut labels = Vec::new();
        for i in 0..model.n_items() {
            let section = model.item_link(i, gio::MENU_LINK_SECTION).unwrap();
            for j in 0..section.n_items() {
                labels.push(
                    section
                        .item_attribute_value(j, gio::MENU_ATTRIBUTE_LABEL, None)
                        .and_then(|v| v.get::<String>())
                        .unwrap(),
                );
            }
        }
        labels
    };
    // Once the list, too, shows the undo.
    pump_until(|| {
        u.view
            .borrow()
            .articles()
            .iter()
            .any(|a| a.id == id && !a.read)
    });
    assert_eq!(
        article_menu(id),
        [
            "Mark as read",
            "Unstar",
            "Mark above as read",
            "Mark below as read",
            "Open in browser",
            "Copy link",
            "Go to feed"
        ]
    );
    u.list
        .menu
        .activate_action(
            &format!("article.{}", articles::ArticleCommand::ToggleRead.name()),
            None,
        )
        .unwrap();
    pump_until(|| {
        runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .read
    });
    assert!(u.view.borrow().open().is_none());
    article_menu(id);
    u.list
        .menu
        .activate_action(
            &format!("article.{}", articles::ArticleCommand::ToggleRead.name()),
            None,
        )
        .unwrap();
    pump_until(|| {
        !runtime
            .block_on(db.call(move |s| s.article(id)))
            .unwrap()
            .read
    });
    article_menu(id);
    u.list
        .menu
        .activate_action(
            &format!("article.{}", articles::ArticleCommand::GoToFeed.name()),
            None,
        )
        .unwrap();
    pump_until(|| u.view.borrow().query.scope == Scope::Feed(fixture));
    pump_until(|| u.list.heading.text() == u.library.borrow().feed(fixture).unwrap().title);
    // Back to Starred, which the rest expects.
    let starred = u
        .sidebar_rows
        .borrow()
        .iter()
        .position(|r| r.scope == Scope::Starred)
        .unwrap();
    u.side
        .list
        .select_row(u.side.list.row_at_index(starred as i32).as_ref());
    pump_until(|| u.list.heading.text() == "Starred");
    // Reading the library again closes the menu and drops its actions: the entry it was
    // opened on may be gone, its ID taken by another.
    open_menu(Scope::Feed(fixture));
    u.reload();
    pump_until(|| {
        u.side
            .menu
            .activate_action("sidebar.copy-feed-url", None)
            .is_err()
    });
    assert!(!u.side.menu.is_visible());
    run(Scope::Feed(discovered), sidebar::Command::Edit);
    pump_until(|| entry_row("Name").is_some());
    let name = entry_row("Name").unwrap();
    assert_eq!(name.text(), "Discovered feed");
    name.set_text("Renamed from the menu");
    form_button("Save").emit_clicked();
    pump_until(|| {
        runtime
            .block_on(db.call(move |s| {
                Ok(s.library()?
                    .feed(discovered)
                    .is_some_and(|f| f.title == "Renamed from the menu"))
            }))
            .unwrap()
    });
    pump_until(|| !open_form());
    run(Scope::Feed(discovered), sidebar::Command::Remove);
    pump_until(|| {
        all_widgets()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .any(|b| b.label().as_deref() == Some("Remove"))
    });
    form_button("Remove").emit_clicked();
    pump_until(|| {
        runtime
            .block_on(db.call(move |s| Ok(s.library()?.feed(discovered).is_none())))
            .unwrap()
    });
    pump_until(|| {
        !u.sidebar_rows
            .borrow()
            .iter()
            .any(|r| r.scope == Scope::Feed(discovered))
    });
    assert_eq!(u.view.borrow().query.scope, Scope::Starred);
    assert_eq!(u.list.heading.text(), "Starred");
    // Persisted state and actual WebKit rendering were verified above.
    u.select(id);
    pump_until(|| u.view.borrow().open().is_some_and(|a| a.id == id));
    let feed = u.view.borrow().open().unwrap().feed_id;
    runtime
        .block_on(outsider.call(move |s| s.delete_feed(feed)))
        .unwrap();
    pump_until(|| u.view.borrow().open().is_none());
    assert!(u.settings.borrow().selected_article.is_none());
    assert!(
        widgets(&u.reader.root.clone().upcast())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Stack>().ok())
            .any(|s| s.visible_child_name().as_deref() == Some("empty"))
    );
    // Connecting a Miniflux account from the window turns the library into a mirror of it:
    // feeds are changed on the server first, and there is no importing OPML.
    let miniflux = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let server = format!("http://{}/", miniflux.local_addr().unwrap());
    let removed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = removed.clone();
    let mock = runtime.spawn(async move {
        loop {
            let (socket, _) = miniflux.accept().await.unwrap();
            tokio::spawn(answer_as_miniflux(socket, log.clone()));
        }
    });
    let enabled = |name: &str| u.window.lookup_action(name).unwrap().is_enabled();
    assert!(enabled("add-feed") && enabled("import"));
    WidgetExt::activate_action(&u.window, "win.account", None).unwrap();
    pump_until(|| entry_row("Server address").is_some());
    entry_row("Server address").unwrap().set_text(&server);
    all_widgets()
        .into_iter()
        .find_map(|w| w.downcast::<gtk::PasswordEntry>().ok())
        .unwrap()
        .set_text("smoke-test-key");
    form_button("Connect").emit_clicked();
    pump_until(|| !open_form() && u.library.borrow().account.is_some());
    assert!(enabled("add-feed") && !enabled("import"));
    let remote = u
        .library
        .borrow()
        .feeds
        .iter()
        .find(|f| f.title == "Remote feed")
        .unwrap()
        .id;
    pump_until(|| {
        u.sidebar_rows
            .borrow()
            .iter()
            .any(|r| r.scope == Scope::Feed(remote))
    });
    assert_eq!(
        open_menu(Scope::Feed(remote)),
        [
            "Open home page",
            "Copy feed URL",
            "Copy home page URL",
            "Mark all as read",
            "Edit…",
            "Remove…"
        ]
    );
    // Removing a feed unsubscribes the server first.
    run(Scope::Feed(remote), sidebar::Command::Remove);
    pump_until(|| {
        all_widgets()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .any(|b| b.label().as_deref() == Some("Remove"))
    });
    form_button("Remove").emit_clicked();
    pump_until(|| u.library.borrow().feed(remote).is_none());
    assert!(
        removed
            .lock()
            .unwrap()
            .iter()
            .any(|r| r == "DELETE /v1/feeds/7"),
        "the server was not told"
    );
    // Disconnecting, after confirming, makes the feeds local again.
    let feeds = u.library.borrow().feeds.len();
    u.side.menu.popdown();
    WidgetExt::activate_action(&u.window, "win.account", None).unwrap();
    pump_until(open_form);
    form_button("Disconnect…").emit_clicked();
    pump_until(|| {
        all_widgets()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .any(|b| b.label().as_deref() == Some("Disconnect"))
    });
    form_button("Disconnect").emit_clicked();
    pump_until(|| u.library.borrow().account.is_none() && !open_form());
    assert!(enabled("add-feed") && enabled("import"));
    assert_eq!(u.library.borrow().feeds.len(), feeds);
    mock.abort();
    u.window.close();
    pump_until(|| !u.window.is_visible());
}

/// Answer one request as a Miniflux server with one feed and one unread entry would, taking
/// any feed or category it is sent, and note any deletion in `deleted`.
async fn answer_as_miniflux(
    mut socket: tokio::net::TcpStream,
    deleted: Arc<std::sync::Mutex<Vec<String>>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // Read the whole request, body included, so closing the connection cannot cut it off.
    let mut request = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let Ok(n) = socket.read(&mut buffer).await else {
            return;
        };
        if n == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..n]);
        let text = String::from_utf8_lossy(&request);
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                break;
            }
        }
    }
    let request = String::from_utf8_lossy(&request);
    let line = request.lines().next().unwrap_or_default();
    if line.starts_with("DELETE ") {
        let target = line.split(' ').take(2).collect::<Vec<_>>().join(" ");
        deleted.lock().unwrap().push(target);
    }
    let category = r#"{"id":1,"title":"All"}"#;
    let entry = r#"{"id":5,"feed_id":7,"status":"unread","starred":false,"title":"Remote entry","url":"https://remote.invalid/post","author":"","content":"<p>From Miniflux.</p>","published_at":"2026-01-01T00:00:00Z","enclosures":null}"#;
    let body = if line.starts_with("GET /v1/me ") {
        r#"{"id":1,"username":"smoke"}"#.to_owned()
    } else if line.starts_with("GET /v1/categories ") {
        format!("[{category}]")
    } else if line.starts_with("POST /v1/categories ") {
        r#"{"id":2,"title":"Made"}"#.to_owned()
    } else if line.starts_with("GET /v1/feeds ") {
        format!(
            r#"[{{"id":7,"title":"Remote feed","site_url":"https://remote.invalid/","feed_url":"https://remote.invalid/feed","category":{category}}}]"#
        )
    } else if line.starts_with("POST /v1/feeds ") {
        r#"{"feed_id":99}"#.to_owned()
    } else if line.starts_with("GET /v1/entries") && !line.contains("starred=true") {
        format!(r#"{{"total":1,"entries":[{entry}]}}"#)
    } else if line.starts_with("GET /v1/entries") {
        r#"{"total":0,"entries":[]}"#.to_owned()
    } else {
        String::new()
    };
    let status = if line.starts_with("PUT ") || line.starts_with("DELETE ") {
        "204 No Content"
    } else {
        "200 OK"
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
}
