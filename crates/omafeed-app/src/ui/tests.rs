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
#[test]
#[ignore = "requires a desktop session; uses an isolated temporary library"]
fn desktop_smoke() {
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
    let db = Db::open(paths.data.join("test.db")).unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let id = runtime
        .block_on(db.call(|s| {
            let folder = s.add_folder("Blogs", None)?;
            let f = s.add_feed("Fixture feed", "https://example.invalid/feed", Some(folder))?;
            s.commit_download(
                f,
                omafeed_core::fetch::Download::Updated(omafeed_core::fetch::Update {
                    entries: vec![omafeed_core::fetch::Entry {
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
                1440,
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
    pump_until(|| !u.articles.borrow().is_empty());
    assert!(!u.list.previous.is_sensitive());
    assert!(!u.list.next.is_sensitive());
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
    u.select(id);
    pump_until(|| u.selected.borrow().as_ref().is_some_and(|a| a.read));
    u.toggle_star();
    pump_until(|| u.selected.borrow().as_ref().is_some_and(|a| a.starred));
    u.toggle_read();
    pump_until(|| u.selected.borrow().as_ref().is_some_and(|a| !a.read));
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
    assert!(u.selected.borrow().is_none());
    assert!(u.settings.borrow().selected_article.is_none());
    crate::dialogs::library(u.clone());
    let window = Rc::new(RefCell::new(None::<gtk::Window>));
    pump_until(|| {
        *window.borrow_mut() = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Manage library"));
        window.borrow().is_some()
    });
    let library = window.borrow().clone().unwrap();
    let root = library.clone().upcast::<gtk::Widget>();
    button(&root, "New folder").emit_clicked();
    pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
    let name = all_widgets()
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Entry>().ok())
        .unwrap();
    name.set_text("Created through UI");
    form_button("Save").emit_clicked();
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
    pump_until(|| !all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
    pump_until(|| {
        widgets(&root)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .any(|l| l.text() == "Created through UI")
    });
    let list = widgets(&root)
        .into_iter()
        .find_map(|w| w.downcast::<gtk::ListBox>().ok())
        .unwrap();
    let feed_row = widgets(&list.clone().upcast())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::ListBoxRow>().ok())
        .find(|r| {
            widgets(&r.clone().upcast())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .any(|l| l.text().contains("Fixture feed"))
        })
        .unwrap();
    list.select_row(Some(&feed_row));
    button(&root, "Edit / Move").emit_clicked();
    pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
    let fields = all_widgets()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Entry>().ok())
        .collect::<Vec<_>>();
    fields[0].set_text("Renamed through UI");
    let picker = all_widgets()
        .into_iter()
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
    pump_until(|| !all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
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
    button(&root, "Add feed").emit_clicked();
    pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
    let fields = all_widgets()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Entry>().ok())
        .collect::<Vec<_>>();
    assert!(fields[0].text().is_empty());
    fields[1].set_text(&website);
    form_button("Save").emit_clicked();
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
    server.abort();
    pump_until(|| {
        widgets(&root)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .any(|l| l.text().contains("Discovered feed"))
    });
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let slow_url = format!("http://{}/", listener.local_addr().unwrap());
    let slow = runtime.spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    button(&root, "Add feed").emit_clicked();
    pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
    let fields = all_widgets()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Entry>().ok())
        .collect::<Vec<_>>();
    fields[1].set_text(&slow_url);
    form_button("Save").emit_clicked();
    pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Spinner>()));
    form_button("Cancel").emit_clicked();
    pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
    assert_eq!(
        runtime
            .block_on(db.call(|s| Ok(s.library()?.feeds.len())))
            .unwrap(),
        2
    );
    form_button("Cancel").emit_clicked();
    slow.abort();
    library.close();
    pump_until(|| !library.is_visible());
    // Persisted state and actual WebKit rendering were verified above.
    u.window.close();
    pump_until(|| !u.window.is_visible());
}
