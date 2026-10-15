use thiserror::Error;

/// Service namespace for every credential stored by this app.
pub const SECRET_SERVICE: &str = "oxidify";

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("invalid account name")]
    InvalidAccount,
    /// Backend failure while storing. The backend message is kept as the
    /// source only, so credential material never appears in displays/logs.
    #[error("failed to store credential")]
    Store(#[source] keyring::Error),
    #[error("failed to load credential")]
    Load(#[source] keyring::Error),
    #[error("failed to delete credential")]
    Delete(#[source] keyring::Error),
}

fn validated_account(account: &str) -> Result<&str, SecretError> {
    if account.trim().is_empty() {
        return Err(SecretError::InvalidAccount);
    }
    Ok(account)
}

/// Store (create or overwrite) the secret for `account` under the shared
/// service namespace. Values are never logged.
pub fn store_secret(account: &str, secret: &str) -> Result<(), SecretError> {
    let account = validated_account(account)?;
    let entry = keyring::Entry::new(SECRET_SERVICE, account).map_err(SecretError::Store)?;
    entry.set_password(secret).map_err(SecretError::Store)
}

/// Load the secret for `account`. `Ok(None)` means no entry exists.
pub fn load_secret(account: &str) -> Result<Option<String>, SecretError> {
    let account = validated_account(account)?;
    let entry = keyring::Entry::new(SECRET_SERVICE, account).map_err(SecretError::Load)?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretError::Load(e)),
    }
}

/// Delete the secret for `account`. Missing entries are treated as success
/// so cleanup stays idempotent.
pub fn delete_secret(account: &str) -> Result<(), SecretError> {
    let account = validated_account(account)?;
    let entry = keyring::Entry::new(SECRET_SERVICE, account).map_err(SecretError::Delete)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(SecretError::Delete(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_accounts_rejected_before_backend() {
        assert!(matches!(
            store_secret("", "s3cr3t-value"),
            Err(SecretError::InvalidAccount)
        ));
        assert!(matches!(load_secret("   "), Err(SecretError::InvalidAccount)));
        assert!(matches!(
            delete_secret(""),
            Err(SecretError::InvalidAccount)
        ));
    }

    #[test]
    fn error_text_never_carries_secret_values() {
        let sentinel = "s3cr3t-value-9z";
        let store = format!("{}", SecretError::Store(keyring::Error::NoEntry));
        let load = format!("{}", SecretError::Load(keyring::Error::NoEntry));
        let delete = format!(
            "{:?}",
            SecretError::Delete(keyring::Error::NoEntry)
        );
        for text in [&store, &load, &delete] {
            assert!(!text.contains(sentinel), "leak in: {text}");
        }
        assert!(store.contains("store"));
        assert!(load.contains("load"));
        assert!(delete.contains("Delete"));
    }

    /// Live OS-store roundtrip. Ignored by default: it needs a working
    /// credential backend and must never run in headless CI.
    #[test]
    #[ignore]
    fn live_roundtrip_against_os_store() {
        struct Cleanup<'a> {
            account: &'a str,
        }
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = delete_secret(self.account);
            }
        }
        let account = format!("oxidify-test-{}", std::process::id());
        let _guard = Cleanup { account: &account };
        let _ = delete_secret(&account);
        store_secret(&account, "roundtrip-value").unwrap();
        assert_eq!(load_secret(&account).unwrap().as_deref(), Some("roundtrip-value"));
        delete_secret(&account).unwrap();
        assert_eq!(load_secret(&account).unwrap(), None);
    }
}
