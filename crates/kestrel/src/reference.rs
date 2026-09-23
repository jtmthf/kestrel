//! What an operator typed where a Session or a Run is named: its generated name, its UUID,
//! any unambiguous prefix of one, or the most recent in scope.

use anyhow::Error;

use crate::declined::Declined;

/// The word that names the most recent record in scope rather than one by identifier.
pub const LATEST: &str = "latest";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference<'a> {
    Latest,
    Given(&'a str),
}

impl<'a> Reference<'a> {
    pub fn read(text: &'a str) -> Self {
        if text.eq_ignore_ascii_case(LATEST) {
            Reference::Latest
        } else {
            Reference::Given(text)
        }
    }

    pub const fn is_latest(self) -> bool {
        matches!(self, Reference::Latest)
    }

    pub const fn given(self) -> Option<&'a str> {
        match self {
            Reference::Given(text) => Some(text),
            Reference::Latest => None,
        }
    }

    /// The canonical identifier prefix this reference spells, when it spells one: hyphens
    /// removed, lowercased, and every character a hex digit. `None` for a name, and for a
    /// reference longer than an identifier.
    pub fn prefix(self) -> Option<String> {
        let Reference::Given(text) = self else {
            return None;
        };
        let normalized: String = text
            .chars()
            .filter(|character| *character != '-')
            .map(|character| character.to_ascii_lowercase())
            .collect();

        (!normalized.is_empty()
            && normalized.len() <= 32
            && normalized
                .chars()
                .all(|character| character.is_ascii_hexdigit()))
        .then_some(normalized)
    }
}

/// One record a reference could have meant, named so a refusal can say which ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
}

/// Nothing in scope matched, said so that naming one exactly is the obvious next step.
pub fn missing(what: &str, organization: &str, reference: &str) -> Error {
    Declined::Missing(format!(
        "no {what} in the organization {organization} matches {reference}; \
         name it by its generated name, its identifier, or `{LATEST}`"
    ))
    .into()
}

/// Several records in scope matched, named rather than chosen between.
pub fn ambiguous(
    what: &str,
    organization: &str,
    reference: &str,
    candidates: &[Candidate],
) -> Error {
    let matched = candidates
        .iter()
        .map(|candidate| format!("{} ({})", candidate.name, candidate.id))
        .collect::<Vec<_>>()
        .join(", ");

    Declined::Ambiguous(format!(
        "{reference} is ambiguous: it matches {} {what}s in the organization {organization}: \
         {matched}; name one of them exactly",
        candidates.len()
    ))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_is_read_however_it_is_cased() {
        assert!(Reference::read("latest").is_latest());
        assert!(Reference::read("LATEST").is_latest());
        assert_eq!(Reference::read("lately").given(), Some("lately"));
    }

    #[test]
    fn a_uuid_is_a_prefix_whether_or_not_it_is_hyphenated_or_cased() {
        let hyphenated = "01A0A2D8-BAF8-7C02-99FA-7280F174C14A";

        assert_eq!(
            Reference::read(hyphenated).prefix().as_deref(),
            Some("01a0a2d8baf87c0299fa7280f174c14a")
        );
        assert_eq!(
            Reference::read("01a0a2d8").prefix().as_deref(),
            Some("01a0a2d8")
        );
        assert_eq!(
            Reference::read("01a0-a2d8").prefix().as_deref(),
            Some("01a0a2d8")
        );
    }

    #[test]
    fn a_generated_name_is_never_read_as_a_prefix() {
        assert_eq!(Reference::read("amber-fox-abcdefgh").prefix(), None);
        assert_eq!(Reference::read("01a0a2d8z").prefix(), None);
        assert_eq!(Reference::read("").prefix(), None);
    }
}
