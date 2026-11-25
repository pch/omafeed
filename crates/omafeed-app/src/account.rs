//! The sync account's secret (Miniflux's API key, or FreshRSS's API password), kept in the
//! desktop's keyring (the Secret Service, such as GNOME Keyring or KWallet) under the service
//! and the server's address, the client the window and the command line sync with, and
//! connecting and disconnecting the account with its secret. Must run on the tokio runtime.
use anyhow::{Context, Result, anyhow, bail};
use omafeed_core::{
    Db, full_article,
    model::{ArticleId, Service},
    sync::{self, Backend, Report, backend::User, freshrss, miniflux},
};
use std::{collections::HashMap, time::Duration};

/// Used instead of the keyring when set, by every command and the window; nothing is saved to
/// or removed from the keyring then. For scripts, and systems without a keyring.
pub const TOKEN_VAR: &str = "OMAFEED_SYNC_TOKEN";

/// The older name of [`TOKEN_VAR`], still read when it is not set.
pub const MINIFLUX_TOKEN_VAR: &str = "OMAFEED_MINIFLUX_TOKEN";

/// How long to wait for the keyring, which may first ask for the password that unlocks it.
const KEYRING_TIMEOUT: Duration = Duration::from_secs(60);

/// What the service calls the secret omafeed signs in with.
pub fn secret_name(service: Service) -> &'static str {
    match service {
        Service::Miniflux => "API key",
        Service::FreshRss => "API password",
    }
}

fn attributes(service: Service, server: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", "omafeed"),
        ("service", service.key()),
        ("server", server),
    ])
}

/// The secret from [`TOKEN_VAR`] (or [`MINIFLUX_TOKEN_VAR`]), if one is set and not empty.
pub fn from_environment() -> Option<String> {
    [TOKEN_VAR, MINIFLUX_TOKEN_VAR]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|token| !token.trim().is_empty())
}

/// Run `work` against the keyring, failing clearly if there is none or it does not answer.
async fn with_keyring<T>(
    work: impl AsyncFnOnce(oo7::Keyring) -> Result<T, oo7::Error>,
) -> Result<T> {
    let run = async {
        let keyring = oo7::Keyring::new().await?;
        work(keyring).await
    };
    tokio::time::timeout(KEYRING_TIMEOUT, run)
        .await
        .map_err(|_| anyhow!("The keyring did not answer"))?
        .with_context(|| {
            format!(
                "Could not use the keyring; is a Secret Service such as GNOME Keyring running? \
                 Set {TOKEN_VAR} to sign in without one"
            )
        })
}

/// The secret for `service` on `server`.
pub async fn token(service: Service, server: &str) -> Result<String> {
    if let Some(token) = from_environment() {
        return Ok(token);
    }
    let server = server.to_owned();
    let secret = with_keyring(async |keyring| {
        let items = keyring.search_items(&attributes(service, &server)).await?;
        let Some(item) = items.into_iter().next() else {
            return Ok(None);
        };
        if item.is_locked().await? {
            item.unlock().await?;
        }
        Ok(Some(item.secret().await?.to_vec()))
    })
    .await?
    .ok_or_else(|| {
        anyhow!(
            "The keyring has no {service} {} for {server}; connect again, or set {TOKEN_VAR}",
            secret_name(service)
        )
    })?;
    String::from_utf8(secret).with_context(|| {
        format!(
            "The {service} {} in the keyring is not text",
            secret_name(service)
        )
    })
}

/// Keep `token` as the secret for `service` on `server`, replacing any older one.
pub async fn save_token(service: Service, server: &str, token: &str) -> Result<()> {
    if from_environment().is_some() {
        return Ok(());
    }
    let label = format!("omafeed: {service} {} for {server}", secret_name(service));
    let (server, token) = (server.to_owned(), token.to_owned());
    with_keyring(async |keyring| {
        keyring
            .create_item(&label, &attributes(service, &server), token, true)
            .await
    })
    .await
}

/// Remove the secret for `service` on `server`.
pub async fn forget_token(service: Service, server: &str) -> Result<()> {
    if from_environment().is_some() {
        return Ok(());
    }
    let server = server.to_owned();
    with_keyring(async |keyring| keyring.delete(&attributes(service, &server)).await).await
}

/// A client for `service` at `server`, signing in as `username` (FreshRSS) with `secret`.
pub fn backend(
    service: Service,
    server: &str,
    username: &str,
    secret: &str,
) -> Result<Box<dyn Backend>> {
    Ok(match service {
        Service::Miniflux => Box::new(miniflux::Client::new(server, secret)?),
        Service::FreshRss => Box::new(freshrss::Client::new(server, username, secret)?),
    })
}

/// The service the library is connected to, signed in with its secret, or `None` if the
/// library is local.
pub async fn client(db: &Db) -> Result<Option<Box<dyn Backend>>> {
    let Some(account) = db.call(|s| s.account()).await? else {
        return Ok(None);
    };
    let token = token(account.service, &account.server).await?;
    backend(account.service, &account.server, &account.username, &token).map(Some)
}

/// Fetch the article `id` in full ([`full_article::fetch`]), by the connected account's server
/// if it can. If the account's secret cannot be read, the page is fetched directly, and the
/// secret is reported only if that fails too.
pub async fn fetch_full_article(db: &Db, id: ArticleId) -> Result<()> {
    match client(db).await {
        Ok(client) => full_article::fetch(db, client.as_deref(), id).await,
        Err(key) => full_article::fetch(db, None, id)
            .await
            .map_err(|e| anyhow!("{e:#}; the sync server could not be asked either: {key:#}")),
    }
}

/// What [`connect`] did.
pub struct Connected {
    /// The service.
    pub service: Service,
    /// The server's address, as the library keeps it.
    pub server: String,
    /// Who the secret signs in as.
    pub user: User,
    /// What the first sync did.
    pub report: Report,
}

/// Connect the library to the account on `server` of `service`, signing in as `username`
/// (FreshRSS; Miniflux's API key says whose it is) with `secret`, keep the secret, and run the
/// first sync. If the server refuses the secret, nothing is kept; if only the first sync
/// fails, the account stays connected with its secret, to sync again.
pub async fn connect(
    db: &Db,
    service: Service,
    server: &str,
    username: &str,
    secret: &str,
) -> Result<Connected> {
    let client = backend(service, server, username, secret)?;
    let user = client.check().await?;
    save_token(service, client.server(), secret).await?;
    match sync::connect(db, client.as_ref()).await {
        Ok(report) => Ok(Connected {
            service,
            server: client.server().to_owned(),
            user,
            report,
        }),
        Err(e) => {
            // A first sync that failed leaves the account connected, and needs the secret.
            if db.call(|s| s.account()).await?.is_none() {
                let _ = forget_token(service, client.server()).await;
            }
            Err(e)
        }
    }
}

/// What [`disconnect`] did.
pub struct Disconnected {
    /// The service the library mirrored.
    pub service: Service,
    /// The server the library mirrored.
    pub server: String,
    /// Read and star changes that could not be sent first, and were dropped.
    pub dropped: i64,
    /// Why the secret could not be removed from the keyring, if it could not.
    pub keyring_error: Option<anyhow::Error>,
}

/// Stop mirroring the account, sending unsent changes first if its secret and server allow,
/// and remove its secret.
pub async fn disconnect(db: &Db) -> Result<Disconnected> {
    let Some(connected) = db.call(|s| s.account()).await? else {
        bail!("No sync account is connected");
    };
    // Without the secret, or with the server gone, unsent changes are dropped.
    let client = client(db).await.ok().flatten();
    let dropped = sync::disconnect(db, client.as_deref()).await?;
    Ok(Disconnected {
        keyring_error: forget_token(connected.service, &connected.server)
            .await
            .err(),
        service: connected.service,
        server: connected.server,
        dropped,
    })
}
