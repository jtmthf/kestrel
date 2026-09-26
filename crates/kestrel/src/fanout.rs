use crate::domain::Workspace;

pub enum Change<'a> {
    WorkspaceOpened(&'a Workspace),
    WorkspaceSealed(&'a Workspace),
}

/// Nothing subscribes at 0.1 (ADR-0005). The boundary is named now so that the day something
/// does, there is one place for it to attach to.
pub fn publish(_change: Change<'_>) {}
