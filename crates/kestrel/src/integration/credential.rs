use std::fmt;

/// The Organization's credential for an external system. Kept rather than digested — kestrel
/// has to present this one back to GitHub — so it carries no `Display` and a `Debug` that
/// redacts, and reaching the token at all is spelled out at the one call that needs it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn held(token: &str) -> Self {
        Self(token.to_owned())
    }

    pub fn presented_to_the_external_system(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(redacted)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_does_not_appear_in_what_is_logged_of_it() {
        let token = Token::held("ghp_notinalog");

        assert!(!format!("{token:?}").contains("ghp_notinalog"));
    }
}
