//! The database worker thread, which serializes all SQLite access.
use super::Store;
use anyhow::Result;
use std::{path::Path, sync::Arc};

type Job = Box<dyn FnOnce(&Store) + Send>;

/// Handle to the database worker thread, which serializes all SQLite access.
#[derive(Clone)]
pub struct Db {
    sender: std::sync::mpsc::Sender<Job>,
    /// Syncs and changes on the server take turns on it: a sync that read the server's feeds
    /// just before an unsubscribe must not bring the feed back.
    sync_turn: Arc<tokio::sync::Mutex<()>>,
}

impl Db {
    /// Open the library as [`Store::open`] does, on this thread, and start its worker.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Store::open(path)?;
        let (sender, receiver) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("omafeed-db".into())
            .spawn(move || {
                for job in receiver {
                    // A panicking job fails its own call without stopping the worker.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&store)));
                }
            })?;
        Ok(Self {
            sender,
            sync_turn: Arc::default(),
        })
    }

    /// Wait for this library's turn to sync or change something on the server.
    pub(crate) async fn sync_turn(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.sync_turn.lock().await
    }

    /// Replace the content filters, as [`Store::set_filters`] does. Jobs run in order, so
    /// calls made after this one see them.
    pub fn set_filters(&self, patterns: Vec<String>) {
        let _ = self.sender.send(Box::new(move |s| {
            if let Err(e) = s.set_filters(&patterns) {
                eprintln!("omafeed: could not apply content filters: {e:#}");
            }
        }));
    }

    /// Run `f` on the worker thread, after everything sent before it, and wait for its result.
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = async_channel::bounded(1);
        self.sender
            .send(Box::new(move |s| {
                let _ = tx.send_blocking(f(s));
            }))
            .map_err(|_| anyhow::anyhow!("Database worker stopped"))?;
        rx.recv()
            .await
            .map_err(|_| anyhow::anyhow!("Database operation failed unexpectedly"))?
    }
}
