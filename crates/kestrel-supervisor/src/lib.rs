pub mod checkout;
pub mod link;
pub mod login;
pub mod permission;
pub mod runtime;

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::link::{Checkout, Exit, Instruction, Link, Report};
use crate::runtime::{Conversation, Runtime};

const RECONNECT_AFTER: Duration = Duration::from_millis(250);
/// Often enough that the control plane keeps its hold on this Environment through a handful
/// of these going missing, and through the control plane itself restarting under it.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(2);
const STDERR_LINES_PER_REPORT: usize = 64;
const STDERR_REPORT_PATIENCE: Duration = Duration::from_secs(5);
/// Long enough for the last lines of an agent that has just exited, which are often why.
const STDERR_DRAINING: Duration = Duration::from_secs(1);

pub trait Diagnostics {
    fn info(&self, message: &str);
}

pub struct Stderr;

impl Diagnostics for Stderr {
    /// A control plane that dies takes the pipe it was reading these over with it, and an
    /// Environment outlives a control-plane restart (ADR-0002) rather than dying into one.
    fn info(&self, message: &str) {
        let _ = writeln!(io::stderr(), "{message}");
    }
}

enum Attended {
    Stopped,
    Finished,
    LostTheLink,
}

/// Held across a reconnect: the conversation goes on whether or not the link does, and what
/// is left to say about it is what the supervisor comes back to.
#[derive(Default)]
struct Attending {
    cursor: Option<String>,
    started: bool,
    checkout: Option<Checkout>,
    prompt: Option<String>,
    conversation: Option<Conversation>,
    finished: bool,
    taken: i64,
    /// Handed back before anything is said, because saying the Run finished ends it and with it
    /// this Environment's right to hand anything back.
    refreshed: BTreeMap<String, String>,
    written: Option<login::Written>,
    saying: VecDeque<Report>,
}

pub async fn run(diagnostics: &dyn Diagnostics, variables: &BTreeMap<String, String>) -> i32 {
    diagnostics.info("supervisor started");

    let Some(link) = dialled(variables) else {
        diagnostics
            .info("no link to dial: set KESTREL_LINK, KESTREL_RUN and KESTREL_RUN_CREDENTIAL");
        return 1;
    };
    let link = Arc::new(link);
    let (stderr, written) = mpsc::unbounded_channel();
    let runtime = Runtime {
        command: set(variables, "KESTREL_AGENT_RUNTIME")
            .unwrap_or_default()
            .to_owned(),
        auth: set(variables, "KESTREL_AGENT_AUTH").map(str::to_owned),
        model: set(variables, "KESTREL_AGENT_MODEL").map(str::to_owned),
        stderr,
    };
    let home = set(variables, "HOME").map(PathBuf::from);

    // Nothing else reaches the link while a turn is being worked, so this Environment says it
    // is alive beside the work rather than between the steps of it.
    let alive = tokio::spawn(saying_it_is_alive(Arc::clone(&link)));
    let mut relaying = tokio::spawn(relaying_stderr(Arc::clone(&link), written));
    let status = attending(&link, &runtime, home.as_deref(), diagnostics).await;
    alive.abort();
    drop(runtime);
    if tokio::time::timeout(STDERR_DRAINING, &mut relaying)
        .await
        .is_err()
    {
        relaying.abort();
    }

    status
}

async fn saying_it_is_alive(link: Arc<Link>) {
    loop {
        tokio::time::sleep(HEARTBEAT_EVERY).await;
        // Whether the link is there at all is the attending loop's to notice and reconnect
        // through; this one says what it can, whenever it can.
        let _ = link.report(&Report::Heartbeat, None).await;
    }
}

/// A line the link was not there to take in time is lost, never queued behind a reconnect.
async fn relaying_stderr(link: Arc<Link>, mut written: mpsc::UnboundedReceiver<String>) {
    let mut lines = Vec::new();
    while written.recv_many(&mut lines, STDERR_LINES_PER_REPORT).await > 0 {
        let report = Report::Stderr {
            lines: std::mem::take(&mut lines),
        };
        let _ = tokio::time::timeout(STDERR_REPORT_PATIENCE, link.report(&report, None)).await;
    }
}

async fn attending(
    link: &Link,
    runtime: &Runtime,
    home: Option<&Path>,
    diagnostics: &dyn Diagnostics,
) -> i32 {
    let mut attending = Attending::default();
    let status = attended(link, runtime, home, &mut attending, diagnostics).await;
    if let Some(conversation) = attending.conversation.take() {
        conversation.end().await;
    }

    status
}

async fn attended(
    link: &Link,
    runtime: &Runtime,
    home: Option<&Path>,
    attending: &mut Attending,
    diagnostics: &dyn Diagnostics,
) -> i32 {
    loop {
        match attend(link, runtime, home, attending, diagnostics).await {
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
    runtime: &Runtime,
    home: Option<&Path>,
    attending: &mut Attending,
    diagnostics: &dyn Diagnostics,
) -> Result<Attended, link::Error> {
    let mut instructions = link.open(attending.cursor.as_deref()).await?;
    match attending.cursor.as_deref() {
        None => diagnostics.info("link open"),
        Some(held) => diagnostics.info(&format!("link open after {held}")),
    }

    link.report(
        &Report::Connected {
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        None,
    )
    .await?;
    diagnostics.info("reported connected");

    loop {
        if !attending.refreshed.is_empty() {
            link.refresh(&attending.refreshed).await?;
            diagnostics.info(&format!(
                "handed back {}",
                attending
                    .refreshed
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            attending.refreshed.clear();
        }
        say(link, attending, diagnostics).await?;
        if attending.finished {
            return Ok(Attended::Finished);
        }
        if attending.started && attending.conversation.is_none() {
            attending.conversation =
                conversation(link, runtime, home, attending, diagnostics).await?;
        }

        tokio::select! {
            delivered = instructions.next() => {
                let Some(delivered) = delivered? else {
                    return Ok(Attended::LostTheLink);
                };
                attending.cursor = Some(delivered.id.clone());
                diagnostics.info(&format!(
                    "instruction {} {}",
                    delivered.instruction.kind(),
                    delivered.id
                ));

                match delivered.instruction {
                    // Stopped is told after the run has already ended, its credential invalidated
                    // with it, so whatever a turn last refreshed was already handed back before
                    // this arrived; only the local copy is left to clean up.
                    Instruction::Stop => {
                        if let Some(written) = attending.written.take() {
                            written.remove();
                        }
                        return Ok(Attended::Stopped);
                    }
                    Instruction::Start { checkout, prompt } if !attending.started => {
                        attending.started = true;
                        attending.prompt = prompt;
                        match checkout::check_out(&checkout).await {
                            Ok(()) => attending.saying.push_back(Report::Started),
                            Err(because) => {
                                diagnostics.info(&because);
                                attending.finished = true;
                                attending.saying.push_back(Report::Checkout {
                                    repositories: checkout::observe(&checkout).await,
                                });
                                attending.saying.push_back(Report::Finished {
                                    exit: Exit::Failed { because },
                                });
                            }
                        }
                        attending.checkout = Some(checkout);
                    }
                    Instruction::Prompt { prompt } => match &attending.conversation {
                        Some(conversation) => conversation.prompt(prompt),
                        None => diagnostics.info("prompted before the run started"),
                    },
                    Instruction::Start { .. } | Instruction::Unrecognized => {}
                }
            }
            worked = turn(&mut attending.conversation) => {
                if let Some(on) = &worked.on {
                    diagnostics.info(&format!("on the model {}", on.model));
                }
                for subject in &worked.allowed {
                    diagnostics.info(&format!("allowed once  {subject}"));
                }
                // Handed back after every turn, not only a finishing one: a login the runtime
                // rotates mid-conversation is refreshed while the run's credential still lets it
                // through, not saved up for a Stop that arrives once that credential is gone.
                if let Some(written) = &attending.written {
                    attending.refreshed = written.refreshed();
                }
                if worked.failed.is_some() {
                    attending.finished = true;
                    attending.conversation = None;
                    if let Some(written) = attending.written.take() {
                        written.remove();
                    }
                }
                // Reported after every turn, not only a finishing one: a Run waiting between
                // turns may be stopped at any moment, and what it last observed is what decides
                // whether its Instance is held.
                let observed = match &attending.checkout {
                    Some(checkout) => Some(checkout::observe(checkout).await),
                    None => None,
                };
                attending.saying.extend(everything_left_to_say(worked, observed));
            }
        }
    }
}

async fn conversation(
    link: &Link,
    runtime: &Runtime,
    home: Option<&Path>,
    attending: &mut Attending,
    diagnostics: &dyn Diagnostics,
) -> Result<Option<Conversation>, link::Error> {
    let prompt = match attending.prompt.clone() {
        Some(prompt) => prompt,
        None => runtime::prompt(&all_entries(link).await?),
    };
    let credentials = link.credentials().await?;
    let provider = credentials.variables;
    if !provider.is_empty() {
        diagnostics.info(&format!(
            "carrying {} into the agent runtime",
            provider.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }

    match written(home, credentials.files, diagnostics) {
        Ok(written) => {
            attending.written = written;
            Ok(Some(Conversation::open(
                runtime,
                provider,
                prompt,
                checkout::root(attending.checkout.as_ref()),
            )))
        }
        Err(because) => {
            diagnostics.info(&because);
            attending.finished = true;
            attending.saying.push_back(Report::Finished {
                exit: Exit::Failed { because },
            });
            Ok(None)
        }
    }
}

async fn turn(conversation: &mut Option<Conversation>) -> runtime::Worked {
    match conversation {
        Some(conversation) => conversation.turn().await,
        None => std::future::pending().await,
    }
}

fn written(
    home: Option<&Path>,
    files: BTreeMap<String, String>,
    diagnostics: &dyn Diagnostics,
) -> Result<Option<login::Written>, String> {
    if files.is_empty() {
        return Ok(None);
    }
    let Some(home) = home else {
        return Err(
            "the run's subscription profile holds files, and this environment has no home to \
             put them in"
                .to_owned(),
        );
    };

    let written = login::write(home, files).map_err(|error| {
        format!("the subscription profile's files could not be written: {error}")
    })?;
    diagnostics.info(&format!(
        "writing {} beneath the agent's home",
        written.paths().collect::<Vec<_>>().join(", ")
    ));

    Ok(Some(written))
}

async fn all_entries(link: &Link) -> Result<Vec<link::Entry>, link::Error> {
    let mut entries = Vec::new();
    let mut cursor = None;

    loop {
        let page = link.entries(cursor.as_deref()).await?;
        entries.extend(page.entries.into_iter().map(|recorded| recorded.entry));
        cursor = page.cursor;
        if !page.more {
            return Ok(entries);
        }
    }
}

/// Numbered from the last one the link took, and dropped once it has been taken: a reconnect
/// says only what is left, and a replay carries the number the attempt that was lost carried.
async fn say(
    link: &Link,
    attending: &mut Attending,
    diagnostics: &dyn Diagnostics,
) -> Result<(), link::Error> {
    while let Some(report) = attending.saying.front() {
        let seq = attending.taken + 1;
        link.report(report, Some(seq)).await?;
        let kind = report.kind();
        attending.taken = seq;
        attending.saying.pop_front();
        diagnostics.info(&format!("reported {kind} {seq}"));
    }

    Ok(())
}

fn everything_left_to_say(
    worked: runtime::Worked,
    observed: Option<Vec<link::Observed>>,
) -> impl Iterator<Item = Report> {
    worked
        .on
        .map(|on| Report::Model { model: on.model })
        .into_iter()
        .chain(
            worked
                .said
                .into_iter()
                .map(|message| Report::Said { message }),
        )
        .chain(worked.usage.map(|usage| Report::Used { usage }))
        .chain(observed.map(|repositories| Report::Checkout { repositories }))
        .chain(std::iter::once(match worked.failed {
            Some(because) => Report::Finished {
                exit: Exit::Failed { because },
            },
            None => Report::Answered,
        }))
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
