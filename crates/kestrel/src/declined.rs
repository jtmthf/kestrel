use std::fmt;

/// Why kestrel would not do what it was asked, told apart so a boundary can answer each
/// differently. Its `Display` is the reason alone.
#[derive(Debug)]
pub enum Declined {
    Unacceptable(String),
    Missing(String),
    Ambiguous(String),
    Taken(String),
}

impl fmt::Display for Declined {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Declined::Unacceptable(why)
            | Declined::Missing(why)
            | Declined::Ambiguous(why)
            | Declined::Taken(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Declined {}
