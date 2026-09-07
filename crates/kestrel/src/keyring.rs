use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit as _, XChaCha20Poly1305, XNonce};

use crate::hex;

const KEY: &str = "kestrel.key";
const NONCE: usize = 24;

/// The key every Provider Credential is encrypted with, generated the first time kestrel opens
/// a data directory: an operator supplies provider keys, and never a key of kestrel's.
pub struct Keyring {
    cipher: XChaCha20Poly1305,
}

impl Keyring {
    pub fn beside(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(KEY);
        let key = match generate(&path) {
            Ok(key) => key,
            // Two roles booting at once race for the file; the one that loses reads what the
            // other wrote rather than overwriting a key credentials are already sealed with.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => read(&path)?,
            Err(error) => {
                return Err(error).with_context(|| format!("generating {}", path.display()));
            }
        };

        Ok(Self {
            cipher: XChaCha20Poly1305::new(&key),
        })
    }

    /// `bound_to` is authenticated alongside the secret without being encrypted, so a sealed
    /// value moved to another row of the database no longer opens.
    pub fn seal(&self, bound_to: &str, secret: &str) -> Result<String> {
        let mut nonce = [0u8; NONCE];
        getrandom::fill(&mut nonce).expect("the operating system should have entropy to spare");

        let sealed = self
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: secret.as_bytes(),
                    aad: bound_to.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("a credential could not be encrypted"))?;

        Ok(hex::encode(&nonce) + &hex::encode(&sealed))
    }

    pub fn unseal(&self, bound_to: &str, sealed: &str) -> Result<String> {
        let sealed = hex::decode(sealed).context("a credential is not stored as it was sealed")?;
        let Some((nonce, secret)) = sealed.split_at_checked(NONCE) else {
            bail!("a credential is shorter than the nonce it was sealed with");
        };

        let opened = self
            .cipher
            .decrypt(
                &XNonce::try_from(nonce).expect("a nonce this long is an XNonce"),
                Payload {
                    msg: secret,
                    aad: bound_to.as_bytes(),
                },
            )
            .map_err(|_| {
                anyhow::anyhow!("a credential does not open with the key beside the database")
            })?;

        String::from_utf8(opened).context("a credential did not seal as text")
    }
}

fn generate(path: &Path) -> io::Result<Key> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system should have entropy to spare");

    let mut file = OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        file.mode(0o600);
    }
    writeln!(file.open(path)?, "{}", hex::encode(&bytes))?;

    Ok(Key::from(bytes))
}

fn read(path: &Path) -> Result<Key> {
    let read =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let bytes = hex::decode(read.trim())
        .with_context(|| format!("{} is not a key kestrel generated", path.display()))?;

    Key::try_from(bytes.as_slice())
        .map_err(|_| anyhow::anyhow!("{} is not a key kestrel generated", path.display()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn keyring(data_dir: &TempDir) -> Keyring {
        Keyring::beside(data_dir.path()).expect("a keyring")
    }

    #[test]
    fn a_secret_opens_with_the_key_that_sealed_it() {
        let data_dir = TempDir::new().expect("a temporary data directory");
        let keyring = keyring(&data_dir);

        let sealed = keyring
            .seal("acme/A_KEY", "a-provider-key")
            .expect("sealed");

        assert!(!sealed.contains("a-provider-key"));
        assert_eq!(
            keyring.unseal("acme/A_KEY", &sealed).expect("opened"),
            "a-provider-key"
        );
    }

    #[test]
    fn the_key_is_generated_once_and_read_back_ever_after() {
        let data_dir = TempDir::new().expect("a temporary data directory");
        let sealed = keyring(&data_dir)
            .seal("acme/A_KEY", "a-provider-key")
            .expect("sealed");

        assert_eq!(
            keyring(&data_dir)
                .unseal("acme/A_KEY", &sealed)
                .expect("opened"),
            "a-provider-key"
        );
    }

    #[test]
    fn a_secret_sealed_under_one_name_does_not_open_under_another() {
        let data_dir = TempDir::new().expect("a temporary data directory");
        let keyring = keyring(&data_dir);

        let sealed = keyring
            .seal("acme/A_KEY", "a-provider-key")
            .expect("sealed");

        assert!(keyring.unseal("other/A_KEY", &sealed).is_err());
        assert!(keyring.unseal("acme/ANOTHER_KEY", &sealed).is_err());
    }

    #[test]
    fn a_secret_does_not_open_with_another_installations_key() {
        let sealed = keyring(&TempDir::new().expect("a temporary data directory"))
            .seal("acme/A_KEY", "a-provider-key")
            .expect("sealed");

        let elsewhere = TempDir::new().expect("a temporary data directory");
        assert!(keyring(&elsewhere).unseal("acme/A_KEY", &sealed).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_key_is_readable_only_by_the_operator_who_generated_it() {
        use std::os::unix::fs::PermissionsExt as _;

        let data_dir = TempDir::new().expect("a temporary data directory");
        let _ = keyring(&data_dir);

        let mode = std::fs::metadata(data_dir.path().join(KEY))
            .expect("the key should be beside the database")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the key is readable by more than its owner"
        );
    }
}
