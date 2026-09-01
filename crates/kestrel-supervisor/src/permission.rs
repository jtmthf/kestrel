//! Answering `session/request_permission`, which ACP makes option-selection only in both
//! versions (ADR-0007): kestrel never rewrites what the agent asked to do.

use std::fmt;

use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionRequest,
};

/// v2's permission `subject` union, filled under v1 with `ToolCall` and nothing else, so v2
/// arrives as a wire change rather than a change to what kestrel decides about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    ToolCall { call: String, title: Option<String> },
}

impl From<&RequestPermissionRequest> for Subject {
    fn from(request: &RequestPermissionRequest) -> Self {
        Subject::ToolCall {
            call: request.tool_call.tool_call_id.0.to_string(),
            title: request.tool_call.fields.title.clone(),
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Subject::ToolCall { call, title: None } => write!(f, "tool call {call}"),
            Subject::ToolCall {
                call,
                title: Some(title),
            } => write!(f, "tool call {call} ({title})"),
        }
    }
}

/// Every subject is allowed once, and `None` means the agent offered no option saying exactly
/// that — one that remembers the answer allows more than once.
pub fn allow_once(options: &[PermissionOption]) -> Option<PermissionOptionId> {
    options
        .iter()
        .find(|option| option.kind == PermissionOptionKind::AllowOnce)
        .map(|option| option.option_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: &'static str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption::new(id, id, kind)
    }

    #[test]
    fn allowing_once_selects_the_option_the_agent_offered_for_it() {
        let options = [
            option("no", PermissionOptionKind::RejectOnce),
            option("yes", PermissionOptionKind::AllowOnce),
        ];

        assert_eq!(
            allow_once(&options).map(|id| id.0.to_string()),
            Some("yes".to_owned())
        );
    }

    #[test]
    fn an_option_that_remembers_the_answer_does_not_stand_in_for_allowing_once() {
        let options = [option("always", PermissionOptionKind::AllowAlways)];

        assert_eq!(allow_once(&options), None);
    }
}
