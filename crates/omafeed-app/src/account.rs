//! The Miniflux API key, kept in the desktop's keyring (the Secret Service, such as GNOME
//! Keyring or KWallet) under the server's address, the client the window and the command
//! line sync with, and connecting and disconnecting the account with its key. Must run on
//! the tokio runtime.
use anyhow::{Context, Result, anyhow, bail};
use omafeed_core::{
    Db,
    sync::{
        self, Report,
        miniflux::{Client, User},
    },
};
use std::{collections::HashMap, time::Duration};

/// Used instead of the keyring when set, by every command and the window; nothing is saved to
/// or removed from the keyring then. For scripts, and systems without a keyring.
pub const TOKEN_VAR: &str = "OMAFEED_MINIFLUX_TOKEN";

/// How long to wait for the keyring, which may first ask for the password that unlocks it.
const KEYRING_TIMEOUT: Duration = Duration::from_secs(60);

fn attributes(server: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", "omafeed"),
        ("service", "miniflux"),
        ("server", server),
    ])
}

/// The key from [`TOKEN_VAR`], if it is set and not empty.
fn from_environment() -> Option<String> {
    std::env::var(TOKEN_VAR)
        .ok()
        .filter(|token| !token.trim().is_empty())
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
                 Set {TOKEN_VAR} to use the API key without one"
            )
        })
}

/// The API key for `server`.
pub async fn token(server: &str) -> Result<String> {
    if let Some(token) = from_environment() {
        return Ok(token);
    }
    let server = server.to_owned();
    let secret = with_keyring(async |keyring| {
        let items = keyring.search_items(&attributes(&server)).await?;
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
            "The keyring has no Miniflux API key for {server}; connect again, or set {TOKEN_VAR}"
        )
    })?;
    String::from_utf8(secret).context("The Miniflux API key in the keyring is not text")
}

/// Keep `token` as the API key for `server`, replacing any older one.
pub async fn save_token(server: &str, token: &str) -> Result<()> {
    if from_environment().is_some() {
        return Ok(());
    }
    let label = format!("omafeed: Miniflux API key for {server}");
    let (server, token) = (server.to_owned(), token.to_owned());
    with_keyring(async |keyring| {
        keyring
            .create_item(&label, &attributes(&server), token, true)
            .await
    })
    .await
}

/// Remove the API key for `server`.
pub async fn forget_token(server: &str) -> Result<()> {
    if from_environment().is_some() {
        return Ok(());
    }
    let server = server.to_owned();
    with_keyring(async |keyring| keyring.delete(&attributes(&server)).await).await
}

/// A client for the account the library is connected to, or `None` if it is local.
pub async fn client(db: &Db) -> Result<Option<Client>> {
    let Some(account) = db.call(|s| s.account()).await? else {
        return Ok(None);
    };
    let token = token(&account.server).await?;
    Ok(Some(Client::new(&account.server, &token)?))
}

/// What [`connect`] did.
pub struct Connected {
    /// The server's address, as the library keeps it.
    pub server: String,
    /// Who the key signs in as.
    pub user: User,
    /// What the first sync did.
    pub report: Report,
}

/// Connect the library to the account `token` signs in to on `server`, keep the key, and run
/// the first sync. If the server refuses the key, nothing is kept; if only the first sync
/// fails, the account stays connected with its key, to sync again.
pub async fn connect(db: &Db, server: &str, token: &str) -> Result<Connected> {
    let client = Client::new(server, token)?;
    let user = client.me().await?;
    save_token(client.server(), token).await?;
    match sync::connect(db, &client).await {
        Ok(report) => Ok(Connected {
            server: client.server().to_owned(),
            user,
            report,
        }),
        Err(e) => {
            // A first sync that failed leaves the account connected, and needs the key.
            if db.call(|s| s.account()).await?.is_none() {
                let _ = forget_token(client.server()).await;
            }
            Err(e)
        }
    }
}

/// What [`disconnect`] did.
pub struct Disconnected {
    /// The server the library mirrored.
    pub server: String,
    /// Read and star changes that could not be sent first, and were dropped.
    pub dropped: i64,
    /// Why the API key could not be removed from the keyring, if it could not.
    pub keyring_error: Option<anyhow::Error>,
}

/// Stop mirroring the account, sending unsent changes first if its key and server allow, and
/// remove its key.
pub async fn disconnect(db: &Db) -> Result<Disconnected> {
    let Some(connected) = db.call(|s| s.account()).await? else {
        bail!("No Miniflux account is connected");
    };
    // Without the key, or with the server gone, unsent changes are dropped.
    let client = client(db).await.ok().flatten();
    let dropped = sync::disconnect(db, client.as_ref()).await?;
    Ok(Disconnected {
        keyring_error: forget_token(&connected.server).await.err(),
        server: connected.server,
        dropped,
    })
}
