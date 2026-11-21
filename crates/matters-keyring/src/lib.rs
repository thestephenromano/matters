//! The platform keyring for a device's sync key seeds (#37): oo7, which
//! uses the Secret Service (GNOME Keyring, KWallet) on the host and the
//! secret portal's encrypted file keyring inside Flatpak.
//!
//! Each device keeps one entry: its two key seeds, found by the name
//! matters-core gives it. Nothing else is stored, and nothing is printed.
//! oo7 is asynchronous; these calls block on it, so call them where a
//! short wait is acceptable (opening sync, not the UI thread's paint path).

use futures_lite::future::block_on;
use matters_core::storage::membership::{DeviceSeeds, KeyStore};
use matters_core::{MattersError, Result};

/// The attribute every Matters entry carries, valued with its name.
const ATTRIBUTE: &str = "matters-sync-device";
const LABEL: &str = "Matters sync device keys";

fn keyring_error(error: oo7::Error) -> MattersError {
    MattersError::Validation(format!("keyring: {error}"))
}

/// A [`KeyStore`] in an oo7 keyring.
pub struct Oo7KeyStore {
    keyring: oo7::Keyring,
}

impl Oo7KeyStore {
    /// The desktop's keyring: the Secret Service on the host, the secret
    /// portal's file keyring when sandboxed.
    pub fn open() -> Result<Self> {
        let keyring = block_on(oo7::Keyring::new()).map_err(keyring_error)?;
        Ok(Self { keyring })
    }

    /// An encrypted file keyring at `path`, unlocked by `secret`: oo7's
    /// sandboxed backend without the portal, for tests and headless use.
    pub fn file(path: &std::path::Path, secret: &[u8]) -> Result<Self> {
        let keyring = block_on(oo7::Keyring::sandboxed_with_path(
            path,
            oo7::Secret::from(secret),
        ))
        .map_err(keyring_error)?;
        Ok(Self { keyring })
    }
}

impl KeyStore for Oo7KeyStore {
    fn load(&self, name: &str) -> Result<Option<DeviceSeeds>> {
        block_on(async {
            let items = self
                .keyring
                .search_items(&[(ATTRIBUTE, name)])
                .await
                .map_err(keyring_error)?;
            let Some(item) = items.first() else {
                return Ok(None);
            };
            if items.len() > 1 {
                return Err(MattersError::Validation(
                    "keyring: more than one entry for this device".into(),
                ));
            }
            if item.is_locked().await.map_err(keyring_error)? {
                item.unlock().await.map_err(keyring_error)?;
            }
            let secret = item.secret().await.map_err(keyring_error)?;
            DeviceSeeds::from_bytes(&secret).map(Some)
        })
    }

    fn store(&self, name: &str, seeds: &DeviceSeeds) -> Result<()> {
        let mut bytes = seeds.to_bytes();
        // The secret wipes itself when dropped; wipe the stack copy here.
        let secret = oo7::Secret::from(&bytes);
        bytes.fill(0);
        std::hint::black_box(&bytes);
        block_on(
            self.keyring
                .create_item(LABEL, &[(ATTRIBUTE, name)], secret, true),
        )
        .map_err(keyring_error)
    }
}
