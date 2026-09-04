use std::fs;
use std::path::{Path, PathBuf};

use matrix_sdk::authentication::matrix::MatrixSession;
use tracing::{info, warn};

/// File name of the stored session, created next to the state store.
const SESSION_FILE_NAME: &str = "session.json";

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

        match fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|contents| {
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
