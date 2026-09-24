//! SWAI — Secure keyring storage for API keys.
//!
//! Wraps the `keyring` crate to store, retrieve, and delete secrets
//! in the OS credential store (Secret Service / KWallet on Linux).
//! All in-memory secret buffers are zeroized on drop.

use thiserror::Error;
use zeroize::Zeroizing;

/// Service name used for all SWAI keyring entries.
const SERVICE: &str = "swai";

/// Errors returned by keyring operations.
#[derive(Debug, Error)]
pub enum KeyringError {
    /// The requested key was not found in the keyring.
    #[error("key not found: {0}")]
    NotFound(String),

    /// The keyring backend reported an error.
    #[error("keyring error: {0}")]
    Backend(String),
}

impl From<keyring::Error> for KeyringError {
    fn from(err: keyring::Error) -> Self {
        match err {
            keyring::Error::NoEntry => KeyringError::NotFound("no entry".into()),
            other => KeyringError::Backend(other.to_string()),
        }
    }
}

/// Save a secret into the OS keyring under the given key reference.
///
/// The `key_ref` is typically a path like `"swai/auditor/claude"`.
/// The secret is written immediately and the in-memory copy is zeroized.
pub fn save_key(key_ref: &str, secret: &str) -> Result<(), KeyringError> {
    let entry = keyring::Entry::new(SERVICE, key_ref)?;
    let buf = Zeroizing::new(secret.to_string());
    entry.set_password(&buf)?;
    Ok(())
}

/// Load a secret from the OS keyring. Returns a `Zeroizing<String>`
/// that automatically clears the secret from memory when dropped.
pub fn load_key(key_ref: &str) -> Result<Zeroizing<String>, KeyringError> {
    let entry = keyring::Entry::new(SERVICE, key_ref)?;
    let password = entry.get_password()?;
    Ok(Zeroizing::new(password))
}

/// Delete a secret from the OS keyring.
pub fn delete_key(key_ref: &str) -> Result<(), KeyringError> {
    let entry = keyring::Entry::new(SERVICE, key_ref)?;
    entry.delete_credential()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyring_error_display() {
        let err = KeyringError::NotFound("test-key".into());
        assert_eq!(err.to_string(), "key not found: test-key");
        let err = KeyringError::Backend("dbus timeout".into());
        assert_eq!(err.to_string(), "keyring error: dbus timeout");
    }

    #[test]
    fn test_keyring_error_from_no_entry() {
        let err: KeyringError = keyring::Error::NoEntry.into();
        assert!(matches!(err, KeyringError::NotFound(_)));
    }

    #[test]
    fn test_zeroizing_drop() {
        let secret = Zeroizing::new("super-secret-key".to_string());
        assert_eq!(secret.as_str(), "super-secret-key");
        drop(secret);
        // After drop, the memory is zeroized — we can't inspect it,
        // but we verify the type enforces zeroization at compile time.
    }

    /// Full round-trip test against the real OS keyring backend.
    /// Ignored by default because it requires a running Secret Service daemon
    /// (e.g., KWallet or gnome-keyring). Run with: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn test_keyring_round_trip() {
        let key_ref = "swai/test/round-trip";
        let secret = "test-api-key-12345";

        // Clean up any leftover from a previous failed run
        let _ = delete_key(key_ref);

        // Save
        save_key(key_ref, secret).expect("save_key failed");

        // Load
        let loaded = load_key(key_ref).expect("load_key failed");
        assert_eq!(loaded.as_str(), secret);

        // Delete
        delete_key(key_ref).expect("delete_key failed");

        // Verify deleted
        let result = load_key(key_ref);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), KeyringError::NotFound(_)));
    }
}
