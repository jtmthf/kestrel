//! A stand-in for the ACP-speaking agent runtime (ADR-0007) the supervisor does not drive yet:
//! the test-only "hook" 0.1/03 asks for, a canned sequence a test can select instead of a real
//! runtime, deterministic and free of network or model spend.

use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    Message(String),
    Exit(i32),
}

pub struct ScriptedRuntime {
    script: VecDeque<RuntimeEvent>,
}

impl ScriptedRuntime {
    pub fn new(script: Vec<RuntimeEvent>) -> Self {
        Self {
            script: script.into(),
        }
    }

    pub fn next_event(&mut self) -> Option<RuntimeEvent> {
        self.script.pop_front()
    }
}

pub enum RuntimeDriver {
    Scripted(ScriptedRuntime),
}

/// `env_value` mirrors `KESTREL_AGENT_RUNTIME`, the hook a real supervisor will read once a
/// second driver exists to choose between.
pub fn select(env_value: Option<&str>, script: Vec<RuntimeEvent>) -> RuntimeDriver {
    match env_value {
        None | Some("scripted") => RuntimeDriver::Scripted(ScriptedRuntime::new(script)),
        Some(other) => panic!("no agent-runtime driver named {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_selects_the_scripted_driver_and_plays_back_its_canned_sequence() {
        let RuntimeDriver::Scripted(mut runtime) = select(
            Some("scripted"),
            vec![
                RuntimeEvent::Message("hello".to_owned()),
                RuntimeEvent::Exit(0),
            ],
        );

        assert_eq!(
            runtime.next_event(),
            Some(RuntimeEvent::Message("hello".to_owned()))
        );
        assert_eq!(runtime.next_event(), Some(RuntimeEvent::Exit(0)));
        assert_eq!(runtime.next_event(), None);
    }

    #[test]
    fn selecting_with_no_driver_named_defaults_to_scripted() {
        let RuntimeDriver::Scripted(mut runtime) = select(None, vec![RuntimeEvent::Exit(1)]);

        assert_eq!(runtime.next_event(), Some(RuntimeEvent::Exit(1)));
    }

    #[test]
    #[should_panic(expected = "no agent-runtime driver named real")]
    fn selecting_an_unknown_driver_is_rejected() {
        select(Some("real"), vec![]);
    }
}
