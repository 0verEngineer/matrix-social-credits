use std::fs;
use std::path::{Path, PathBuf};

use matrix_sdk::SqliteCryptoStore;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::ruma::OwnedDeviceId;
use matrix_sdk_crypto::store::CryptoStore;
use tracing::{info, warn};

/// File name of the stored session, created next to the state store.
const SESSION_FILE_NAME: &str = "session.json";

/// File name matrix-sdk gives the crypto store inside the store directory.
const CRYPTO_STORE_FILE_NAME: &str = "matrix-sdk-crypto.sqlite3";

/// Passphrase for the sqlite stores. There is none: the directory is only readable by the
/// bot's own user, and a passphrase in an environment variable next to the password would
/// not add anything. Must be the same wherever the store is opened.
pub const STORE_PASSPHRASE: Option<&str> = None;

/// Where the persistent client state lives.
///
/// Keeping this on disk is what stops the bot from logging in again on every start. A fresh
/// login creates a new device each time (so the bot account collects hundreds of them), and
/// `/login` is one of the endpoints Synapse rate limits hardest -- which is exactly the 429
/// storm seen when the Matrix stack is restarted.
///
/// The state store additionally keeps the sync token, so a restart resumes where the previous
/// run left off instead of asking the homeserver for an initial sync and replaying the whole
/// timeline.
#[derive(Debug, Clone)]
pub struct SessionStore {
    directory: PathBuf,
}

impl SessionStore {
    pub fn new(directory: impl AsRef<Path>) -> std::io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        Ok(Self { directory })
    }

    /// Directory handed to `ClientBuilder::sqlite_store`.
    pub fn state_store_path(&self) -> &Path {
        &self.directory
    }

    /// The device the crypto store belongs to, if there is a crypto store at all.
    ///
    /// The crypto store is tied to exactly one device: it holds that device's Olm account
    /// and every room key it was ever given. Logging in as any other device while this store
    /// is on disk fails inside the SDK ("the account in the store doesn't match the account
    /// in the constructor"), and it fails *after* the homeserver has already handed out the
    /// new device -- so every attempt leaves another dead device on the account and the
    /// client is left half initialised. Seen in production when `session.json` was gone but
    /// the sqlite files were not: a crash loop that created a device per restart.
    ///
    /// A fresh login therefore has to ask the homeserver for this very device id. That is
    /// also what keeps the room keys: the same device just gets a new access token.
    ///
    /// Read before the client is built, so the store is not open twice at the same time.
    pub async fn stored_device_id(&self) -> Option<OwnedDeviceId> {
        let path = self.directory.join(CRYPTO_STORE_FILE_NAME);
        if !path.exists() {
            return None;
        }

        // Opening the store also creates the file, which is why the existence check comes
        // first: a store that is not there yet is not an error, it is the first start.
        let store = match SqliteCryptoStore::open(&self.directory, STORE_PASSPHRASE).await {
            Ok(store) => store,
            Err(error) => {
                warn!(path = %path.display(), %error, "Unable to open the crypto store");
                return None;
            }
        };

        match store.load_account().await {
            Ok(Some(account)) => {
                let device_id = account.device_id().to_owned();
                info!(%device_id, "The crypto store belongs to a known device");
                Some(device_id)
            }
            Ok(None) => None,
            Err(error) => {
                warn!(path = %path.display(), %error, "Unable to read the crypto store");
                None
            }
        }
    }

    fn session_path(&self) -> PathBuf {
        self.directory.join(SESSION_FILE_NAME)
    }

    /// Load a previously saved session, if there is one that can still be read.
    ///
    /// A broken or outdated file is not fatal: the caller falls back to a normal login.
    pub fn load(&self) -> Option<MatrixSession> {
        let path = self.session_path();
        if !path.exists() {
            return None;
        }

        match fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|contents| {
                serde_json::from_str::<MatrixSession>(&contents).map_err(|e| e.to_string())
            }) {
            Ok(session) => {
                info!(path = %path.display(), "Restoring the saved session");
                Some(session)
            }
            Err(error) => {
                warn!(path = %path.display(), %error, "Unable to read the saved session, logging in again");
                None
            }
        }
    }

    /// Persist a session so the next start can skip the login.
    ///
    /// The file holds an access token, so it is written with owner-only permissions.
    pub fn save(&self, session: &MatrixSession) -> std::io::Result<()> {
        let path = self.session_path();
        let contents = serde_json::to_string(session)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        fs::write(&path, contents)?;
        restrict_permissions(&path)?;

        info!(path = %path.display(), "Saved the session");
        Ok(())
    }

    /// Remove a session the homeserver no longer accepts.
    pub fn clear(&self) {
        let path = self.session_path();
        if path.exists()
            && let Err(error) = fs::remove_file(&path)
        {
            warn!(path = %path.display(), %error, "Unable to remove the stale session file");
        }
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{device_id, user_id};
    use matrix_sdk_crypto::olm::Account;
    use matrix_sdk_crypto::store::types::PendingChanges;

    use super::*;

    #[tokio::test]
    async fn no_crypto_store_means_no_device() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path()).unwrap();

        assert_eq!(store.stored_device_id().await, None);
        // Asking must not have created one either.
        assert!(!dir.path().join(CRYPTO_STORE_FILE_NAME).exists());
    }

    #[tokio::test]
    async fn an_empty_crypto_store_has_no_device_yet() {
        let dir = tempfile::tempdir().unwrap();
        SqliteCryptoStore::open(dir.path(), STORE_PASSPHRASE)
            .await
            .unwrap();
        let store = SessionStore::new(dir.path()).unwrap();

        assert_eq!(store.stored_device_id().await, None);
    }

    /// The situation from production: the sqlite files survived, `session.json` did not.
    #[tokio::test]
    async fn the_device_of_an_existing_account_is_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let crypto_store = SqliteCryptoStore::open(dir.path(), STORE_PASSPHRASE)
            .await
            .unwrap();
        let account =
            Account::with_device_id(user_id!("@bot:example.org"), device_id!("THTSUUHDAQ"));
        crypto_store
            .save_pending_changes(PendingChanges {
                account: Some(account),
            })
            .await
            .unwrap();
        drop(crypto_store);

        let store = SessionStore::new(dir.path()).unwrap();
        assert_eq!(
            store.stored_device_id().await.as_deref(),
            Some(device_id!("THTSUUHDAQ"))
        );
    }
}
