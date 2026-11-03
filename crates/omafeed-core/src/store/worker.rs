//! The database worker thread, which serializes all SQLite access.
use super::Store;
use anyhow::Result;
use std::path::Path;

type Job = Box<dyn FnOnce(&Store) + Send>;

/// Handle to the database worker thread, which serializes all SQLite access.
#[derive(Clone)]
pub struct Db {
    sender: std::sync::mpsc::Sender<Job>,
}

impl Db {
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
        Ok(Self { sender })
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
