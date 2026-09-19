//! The bearer token, the socket path, and the one file the CLI shim reads.

use std::path::{Path, PathBuf};

use kea_core::secrets::{CredentialStore, LOCAL_API_TOKEN_REF};

/// 32 bytes of entropy, hex-encoded — 64 characters, fixed length, so the
/// constant-time comparison in [`super::auth`] never has a length to leak.
const TOKEN_BYTES: usize = 32;

/// The listening socket. Under the app data directory, so it inherits that
/// directory's ownership, and named plainly so `lsof` tells the truth about
/// what is listening.
pub fn socket_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("kea-api.sock")
}

/// The `0600` file the CLI shim reads the token from.
///
/// **This file is the weakest link in the whole design, and it is deliberate.**
/// The shim is a different binary from the app, so reading KEA's keychain item
/// from it would raise a macOS keychain ACL prompt on every invocation, which
/// nobody would tolerate. So the token is also written here, at `0600`,
/// rewritten whenever it is regenerated, and deleted the moment the API is
/// turned off. On a single-user Mac anything running as the user can read it —
/// but so can it read the keychain, so the file does not move the boundary.
pub fn token_file_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("api-token")
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        // Infallible against a String; the result is discarded rather than
        // dragging a fmt error up through a function that cannot fail.
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Mint a fresh token from the OS CSPRNG.
pub fn mint() -> Result<String, String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| format!("could not read system entropy: {e}"))?;
    Ok(hex(&bytes))
}

/// The stored token, minting and storing one on first use.
///
/// It lives in the same keychain item family as the provider API keys
/// (`provider_ref = "local-api-token"` against service `ai.kea.desktop`), so it
/// inherits their ACL and needs no new storage code.
pub async fn load_or_mint(credentials: &dyn CredentialStore) -> Result<String, String> {
    if let Some(existing) = credentials
        .get(LOCAL_API_TOKEN_REF)
        .await
        .map_err(|e| e.to_string())?
    {
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let token = mint()?;
    credentials
        .set(LOCAL_API_TOKEN_REF, &token)
        .await
        .map_err(|e| e.to_string())?;
    Ok(token)
}

/// Replace the stored token. Any script holding the old one stops working,
/// which is the entire point of the button that calls this.
pub async fn regenerate(credentials: &dyn CredentialStore) -> Result<String, String> {
    let token = mint()?;
    credentials
        .set(LOCAL_API_TOKEN_REF, &token)
        .await
        .map_err(|e| e.to_string())?;
    Ok(token)
}

/// Write the token file at `0600`.
///
/// The mode is set explicitly rather than left to the umask: a user with a
/// permissive umask would otherwise get a world-readable token, and the whole
/// argument for this file resting on filesystem permissions would be gone.
pub fn write_token_file(app_data_dir: &Path, token: &str) -> Result<PathBuf, String> {
    let path = token_file_path(app_data_dir);
    std::fs::write(&path, token).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    set_owner_only(&path)?;
    Ok(path)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("could not restrict {} to the owner: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<(), String> {
    // Windows has no mode bits to set here, and the API server itself is
    // unix-only (it listens on a unix socket), so this path is unreachable in
    // practice. It exists so the workspace still builds as one feature set.
    Ok(())
}

/// Remove the token file, ignoring an absent one.
pub fn remove_token_file(app_data_dir: &Path) {
    let path = token_file_path(app_data_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "could not delete the API token file")
        }
    }
}

/// Whether `path` may be unlinked as a stale socket.
///
/// A crash leaves the socket file behind and `bind` then fails with
/// `EADDRINUSE`, so the server has to unlink before binding — and an unlink
/// driven by a path is worth a guard. Pure, so the rule is a test rather than
/// a comment.
pub fn safe_to_unlink(path: &Path, app_data_dir: &Path) -> bool {
    path.starts_with(app_data_dir)
        && !path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        && path.file_name() == socket_path(app_data_dir).file_name()
}

/// Clear a stale socket out of the way, returning the path to bind.
pub fn prepare_socket(app_data_dir: &Path) -> Result<PathBuf, String> {
    let path = socket_path(app_data_dir);
    if !path.exists() {
        return Ok(path);
    }
    if !safe_to_unlink(&path, app_data_dir) {
        return Err(format!(
            "refusing to remove {}: it is not inside the app data directory",
            path.display()
        ));
    }
    std::fs::remove_file(&path).map_err(|e| {
        format!(
            "could not clear the stale socket at {}: {e}",
            path.display()
        )
    })?;
    tracing::info!(path = %path.display(), "cleared a stale API socket left by a previous run");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_core::secrets::InMemoryCredentialStore;

    #[test]
    fn a_minted_token_is_64_hex_characters_and_never_the_same_twice() {
        let a = mint().unwrap();
        let b = mint().unwrap();
        assert_eq!(a.len(), TOKEN_BYTES * 2);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "the CSPRNG returned the same 32 bytes twice");
    }

    #[tokio::test]
    async fn load_or_mint_stores_once_and_reads_back() {
        let store = InMemoryCredentialStore::default();
        let first = load_or_mint(&store).await.unwrap();
        let second = load_or_mint(&store).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(
            store.get(LOCAL_API_TOKEN_REF).await.unwrap().as_deref(),
            Some(first.as_str())
        );
    }

    #[tokio::test]
    async fn regenerate_replaces_the_stored_token() {
        let store = InMemoryCredentialStore::default();
        let first = load_or_mint(&store).await.unwrap();
        let next = regenerate(&store).await.unwrap();
        assert_ne!(first, next);
        assert_eq!(load_or_mint(&store).await.unwrap(), next);
    }

    #[tokio::test]
    async fn an_empty_stored_token_is_replaced_rather_than_used() {
        let store = InMemoryCredentialStore::default();
        store.set(LOCAL_API_TOKEN_REF, "").await.unwrap();
        assert!(!load_or_mint(&store).await.unwrap().is_empty());
    }

    #[test]
    fn the_token_file_is_owner_only_and_deletable() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_token_file(dir.path(), "secret").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token file is {:o}", mode & 0o777);
        }
        remove_token_file(dir.path());
        assert!(!path.exists());
        // Deleting an absent file is not an error.
        remove_token_file(dir.path());
    }

    #[test]
    fn only_our_own_socket_inside_the_data_dir_may_be_unlinked() {
        let dir = Path::new("/Users/kea/Library/Application Support/ai.kea.desktop");
        assert!(safe_to_unlink(&socket_path(dir), dir));
        assert!(!safe_to_unlink(Path::new("/tmp/kea-api.sock"), dir));
        assert!(!safe_to_unlink(&dir.join("config.db"), dir));
        assert!(!safe_to_unlink(&dir.join("..").join("kea-api.sock"), dir));
    }

    #[test]
    fn prepare_socket_clears_a_stale_file_and_tolerates_none() {
        let dir = tempfile::tempdir().unwrap();
        // No socket yet: the path comes back untouched.
        let path = prepare_socket(dir.path()).unwrap();
        assert_eq!(path, socket_path(dir.path()));
        assert!(!path.exists());

        // A crash left one behind; a plain file stands in for it, because
        // `remove_file` cannot tell the difference and neither can `bind`.
        std::fs::write(&path, b"stale").unwrap();
        assert_eq!(prepare_socket(dir.path()).unwrap(), path);
        assert!(!path.exists());
    }
}
