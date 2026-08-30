use std::fmt::Write as _;

use jiff::Timestamp;
use sha2::{Digest as _, Sha256};

use crate::domain::{OrganizationId, RunId};

/// The credential as the supervisor presents it. Never stored: `Store` keeps only its digest,
/// so a copy of the database is not a set of usable credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn mint() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the operating system should have entropy to spare");

        Self(hex(&bytes))
    }

    pub fn presented(token: &str) -> Self {
        Self(token.to_owned())
    }

    pub fn digest(&self) -> String {
        hex(&Sha256::digest(self.0.as_bytes()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[derive(Debug, Clone)]
pub struct Credential {
    pub run: RunId,
    pub organization: OrganizationId,
    pub expires_at: Timestamp,
    pub invalidated_at: Option<Timestamp>,
}

impl Credential {
    pub fn is_live_at(&self, moment: Timestamp) -> bool {
        self.invalidated_at.is_none() && moment < self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_minted_secrets_differ() {
        assert_ne!(Secret::mint(), Secret::mint());
    }

    #[test]
    fn a_secrets_digest_is_not_the_secret() {
        let secret = Secret::mint();

        assert_ne!(secret.digest(), secret.as_str());
        assert_eq!(secret.digest(), Secret::presented(secret.as_str()).digest());
    }

    #[test]
    fn an_invalidated_credential_is_not_live_however_far_off_its_expiry_is() {
        let credential = Credential {
            run: RunId::generate(),
            organization: OrganizationId::generate(),
            expires_at: Timestamp::MAX,
            invalidated_at: Some(Timestamp::now()),
        };

        assert!(!credential.is_live_at(Timestamp::now()));
    }

    #[test]
    fn a_credential_is_not_live_once_its_expiry_has_passed() {
        let expires_at = Timestamp::now();
        let credential = Credential {
            run: RunId::generate(),
            organization: OrganizationId::generate(),
            expires_at,
            invalidated_at: None,
        };

        assert!(credential.is_live_at(expires_at - jiff::SignedDuration::from_secs(1)));
        assert!(!credential.is_live_at(expires_at));
    }
}
