//! Connecting the library to a Miniflux account from the window, and disconnecting it. Both
//! talk to the server, so the window shows them as it shows a refresh, and neither starts
//! while one is under way.
use super::Ui;
use crate::account::{Connected, Disconnected};
use anyhow::{Result, bail};
use gtk::prelude::*;
use omafeed_core::ingest::{Progress, Which};
use std::rc::Rc;

impl Ui {
    /// Run `work` on the tokio runtime while the refresh button shows it under way, as
    /// `tip`. Fails at once if a refresh or sync is under way already.
    async fn talk_to_server<T: Send + 'static>(
        self: &Rc<Self>,
        tip: &str,
        work: impl Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        if self.busy.get() {
            bail!("omafeed is refreshing; try again when it finishes");
        }
        self.on_progress(Progress::Started(1));
        self.header.refresh.set_tooltip_text(Some(tip));
        let result = match self.runtime.spawn(work).await {
            Ok(result) => result,
            Err(e) => Err(e.into()),
        };
        self.refresh_stopped();
        result
    }

    /// Connect the library to the account `token` signs in to on `server`, as
    /// [`crate::account::connect`] does, and show the library it mirrors.
    pub async fn connect_account(
        self: &Rc<Self>,
        server: String,
        token: String,
    ) -> Result<Connected> {
        let db = self.db.clone();
        let result = self
            .talk_to_server("Connecting to Miniflux…", async move {
                crate::account::connect(&db, &server, &token).await
            })
            .await;
        let failed = match &result {
            Ok(done) => done.report.refused.len(),
            Err(_) => 0,
        };
        // A first sync that failed still leaves the account connected, so read the library
        // either way; finishing a refresh of its feeds shows it.
        let feeds = self
            .db
            .call(|s| s.library())
            .await
            .map_or(0, |l| l.feeds.len());
        self.on_progress(Progress::Finished {
            total: feeds,
            failed,
        });
        if feeds == 0 {
            self.reload();
        }
        result
    }

    /// Stop mirroring the account, as [`crate::account::disconnect`] does, and fetch the
    /// feeds, which are local again.
    pub async fn disconnect_account(self: &Rc<Self>) -> Result<Disconnected> {
        let db = self.db.clone();
        let result = self
            .talk_to_server("Disconnecting from Miniflux…", async move {
                crate::account::disconnect(&db).await
            })
            .await;
        self.header
            .refresh
            .set_tooltip_text(Some(super::REFRESH_TIP));
        self.reload();
        if result.is_ok() {
            // Every feed is due once it is local again.
            self.start_refresh(Which::Due);
        }
        result
    }
}
