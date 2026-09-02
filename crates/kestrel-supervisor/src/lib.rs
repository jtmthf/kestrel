pub mod link;
pub mod permission;
pub mod runtime;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::link::{Instruction, Link, Report};

const RECONNECT_AFTER: Duration = Duration::from_millis(250);
/// Often enough that the control plane keeps its hold on this Environment through a handful
/// of these going missing, and through the control plane itself restarting under it.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(2);

pub trait Diagnostics {
    fn info(&self, message: &str);
}

pub struct Stderr;

impl Diagnostics for Stderr {
    fn info(&self, message: &str) {
        eprintln!("{message}");
    }
}

enum Attended {
    Stopped,
    Finished,
    LostTheLink,
}

/// Held across a reconnect: the turn is worked once, and what is left to say about it is what
/// the supervisor comes back to.
#[derive(Default)]
struct Attending {
    cursor: Option<String>,
    started: bool,
    worked: Option<runtime::Worked>,
}

pub async fn run(diagnostics: &dyn Diagnostics, variables: &BTreeMap<String, String>) -> i32 {
    diagnostics.info("supervisor started");

    let Some(link) = dialled(variables) else {
        diagnostics
            .info("no link to dial: set KESTREL_LINK, KESTREL_RUN and KESTREL_RUN_CREDENTIAL");
        return 1;
    };
    let runtime = set(variables, "KESTREL_AGENT_RUNTIME")
        .unwrap_or_default()
        .to_owned();
    let link = Arc::new(link);

    // Nothing else reaches the link while a turn is being worked, so this Environment says it
    // is alive beside the work rather than between the steps of it.
    let alive = tokio::spawn(saying_it_is_alive(Arc::clone(&link)));
    let status = attending(&link, &runtime, diagnostics).await;
    alive.abort();

    status
}

async fn saying_it_is_alive(link: Arc<Link>) {
    loop {
        tokio::time::sleep(HEARTBEAT_EVERY).await;
        // Whether the link is there at all is the attending loop's to notice and reconnect
        // through; this one says what it can, whenever it can.
        let _ = link.report(&Report::Heartbeat).await;
    }
}

async fn attending(link: &Link, runtime: &str, diagnostics: &dyn Diagnostics) -> i32 {
    let mut attending = Attending::default();

    loop {
        match attend(link, runtime, &mut attending, diagnostics).await {
            Ok(Attended::Stopped) => {
                diagnostics.info("supervisor stopped");
                return 0;
            }
            Ok(Attended::Finished) => {
                diagnostics.info("supervisor finished");
                return 0;
            }
            Ok(Attended::LostTheLink) => diagnostics.info("lost the link"),
            Err(link::Error::Refused(why)) => {
                diagnostics.info(&format!("the link refused this environment: {why}"));
                return 1;
            }
            Err(link::Error::Lost(why)) => diagnostics.info(&format!("lost the link: {why}")),
        }

        tokio::time::sleep(RECONNECT_AFTER).await;
    }
}

async fn attend(
    link: &Link,
    runtime: &str,
    attending: &mut Attending,
    diagnostics: &dyn Diagnostics,
) -> Result<Attended, link::Error> {
    let mut instructions = link.open(attending.cursor.as_deref()).await?;
    match attending.cursor.as_deref() {
        None => diagnostics.info("link open"),
        Some(held) => diagnostics.info(&format!("link open after {held}")),
    }

    link.report(&Report::Connected {
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
    .await?;
    diagnostics.info("reported connected");

    // Nothing is read off the stream once the Run has started: saying how it went is all that
    // is left, and a reconnection resumes at that rather than waiting to be told to start again.
    if !attending.started {
        loop {
            let Some(delivered) = instructions.next().await? else {
                return Ok(Attended::LostTheLink);
            };
            attending.cursor = Some(delivered.id.clone());
            diagnostics.info(&format!(
                "instruction {} {}",
                delivered.instruction.kind(),
                delivered.id
            ));

            match delivered.instruction {
                Instruction::Stop => return Ok(Attended::Stopped),
                Instruction::Start => break,
                Instruction::Unrecognized => {}
            }
        }
        attending.started = true;
    }

    link.report(&Report::Started).await?;
    diagnostics.info("reported started");

    if attending.worked.is_none() {
        let worked = runtime::work(runtime).await;
        for subject in &worked.allowed {
            diagnostics.info(&format!("allowed once  {subject}"));
        }
        attending.worked = Some(worked);
    }
    let worked = attending
        .worked
        .as_mut()
        .expect("the turn has been worked by here");

    // Each of these is dropped once the link has taken it, so a reconnect says what is left
    // rather than saying any of it twice.
    while let Some(message) = worked.said.front() {
        link.report(&Report::Said {
            message: message.clone(),
        })
        .await?;
        worked.said.pop_front();
        diagnostics.info("reported what the agent said");
    }
    if let Some(usage) = worked.usage.clone() {
        link.report(&Report::Used { usage }).await?;
        worked.usage = None;
        diagnostics.info("reported what the agent used");
    }

    link.report(&Report::Finished {
        exit: worked.exit.clone(),
    })
    .await?;
    diagnostics.info("reported finished");

    Ok(Attended::Finished)
}

fn dialled(variables: &BTreeMap<String, String>) -> Option<Link> {
    let base = set(variables, "KESTREL_LINK")?;
    let run = set(variables, "KESTREL_RUN")?;
    let credential = set(variables, "KESTREL_RUN_CREDENTIAL")?;

    Some(Link::to(base, run, credential))
}

fn set<'a>(variables: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    variables
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Recorder(Mutex<Vec<String>>);

    impl Recorder {
        fn new() -> Self {
            Self(Mutex::new(Vec::new()))
        }

        fn everything_it_said(&self) -> String {
            self.0
                .lock()
                .expect("the recorder should not be poisoned")
                .join("\n")
        }
    }

    impl Diagnostics for Recorder {
        fn info(&self, message: &str) {
            self.0
                .lock()
                .expect("the recorder should not be poisoned")
                .push(message.to_owned());
        }
    }

    fn variables(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn the_supervisor_says_when_it_started() {
        let diagnostics = Recorder::new();

        run(&diagnostics, &variables(&[])).await;

        assert!(
            diagnostics
                .everything_it_said()
                .contains("supervisor started")
        );
    }

    #[tokio::test]
    async fn the_supervisor_fails_when_it_is_given_no_link_to_dial() {
        let diagnostics = Recorder::new();

        assert_eq!(run(&diagnostics, &variables(&[])).await, 1);
        assert!(diagnostics.everything_it_said().contains("no link to dial"));
    }

    #[tokio::test]
    async fn a_variable_set_to_nothing_is_not_a_link_to_dial() {
        let diagnostics = Recorder::new();

        let status = run(
            &diagnostics,
            &variables(&[
                ("KESTREL_LINK", ""),
                ("KESTREL_RUN", "01999cf2-0000-7000-8000-000000000000"),
                ("KESTREL_RUN_CREDENTIAL", "a-credential"),
            ]),
        )
        .await;

        assert_eq!(status, 1);
        assert!(diagnostics.everything_it_said().contains("no link to dial"));
    }

    #[tokio::test]
    async fn the_supervisor_fails_when_it_is_given_a_link_but_no_credential() {
        let diagnostics = Recorder::new();

        let status = run(
            &diagnostics,
            &variables(&[
                ("KESTREL_LINK", "http://127.0.0.1:1"),
                ("KESTREL_RUN", "01999cf2-0000-7000-8000-000000000000"),
            ]),
        )
        .await;

        assert_eq!(status, 1);
        assert!(diagnostics.everything_it_said().contains("no link to dial"));
    }
}
