pub mod link;

use std::collections::BTreeMap;
use std::time::Duration;

use crate::link::{Instruction, Link, Report};

const RECONNECT_AFTER: Duration = Duration::from_millis(250);

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
    LostTheLink,
}

pub async fn run(diagnostics: &dyn Diagnostics, variables: &BTreeMap<String, String>) -> i32 {
    diagnostics.info("supervisor started");

    let Some(link) = dialled(variables) else {
        diagnostics
            .info("no link to dial: set KESTREL_LINK, KESTREL_RUN and KESTREL_RUN_CREDENTIAL");
        return 1;
    };

    let mut cursor = None;

    loop {
        match attend(&link, &mut cursor, diagnostics).await {
            Ok(Attended::Stopped) => {
                diagnostics.info("supervisor stopped");
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
    cursor: &mut Option<String>,
    diagnostics: &dyn Diagnostics,
) -> Result<Attended, link::Error> {
    let mut instructions = link.open(cursor.as_deref()).await?;
    match cursor.as_deref() {
        None => diagnostics.info("link open"),
        Some(held) => diagnostics.info(&format!("link open after {held}")),
    }

    link.report(&Report::Connected {
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
    .await?;
    diagnostics.info("reported connected");

    while let Some(delivered) = instructions.next().await? {
        *cursor = Some(delivered.id.clone());
        diagnostics.info(&format!(
            "instruction {} {}",
            delivered.instruction.kind(),
            delivered.id
        ));

        if delivered.instruction == Instruction::Stop {
            return Ok(Attended::Stopped);
        }
    }

    Ok(Attended::LostTheLink)
}

fn dialled(variables: &BTreeMap<String, String>) -> Option<Link> {
    let base = variables.get("KESTREL_LINK")?;
    let run = variables.get("KESTREL_RUN")?;
    let credential = variables.get("KESTREL_RUN_CREDENTIAL")?;

    Some(Link::to(base, run, credential))
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
