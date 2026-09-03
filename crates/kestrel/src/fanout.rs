use crate::domain::Session;

pub enum Change<'a> {
    SessionOpened(&'a Session),
    SessionSealed(&'a Session),
}

/// Nothing subscribes at 0.1 (ADR-0005). The boundary is named now so that the day something
/// does, there is one place for it to attach to.
pub fn publish(_change: Change<'_>) {}
