//! What an Environment says on its way through a Run, and the waiting a test does on it.

use std::io::{BufRead as _, BufReader, Read};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const PATIENCE: Duration = Duration::from_secs(30);

pub struct Diagnostics {
    whose: &'static str,
    lines: UnboundedReceiver<String>,
    seen: Vec<String>,
}

impl Diagnostics {
    pub fn pumped(whose: &'static str, pipe: impl Read + Send + 'static) -> Self {
        let (lines, received) = unbounded_channel();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            whose,
            lines: received,
            seen: Vec::new(),
        }
    }

    pub async fn wait_until_it_says(&mut self, what: &str) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if self.said(what) {
                return;
            }
            match tokio::time::timeout_at(deadline, self.lines.recv()).await {
                Ok(Some(line)) => self.seen.push(line),
                Ok(None) => panic!(
                    "{} stopped saying anything before it said {what:?}. it said:\n{}",
                    self.whose,
                    self.everything_it_said()
                ),
                Err(_) => panic!(
                    "timed out waiting for {} to say {what:?}. it said:\n{}",
                    self.whose,
                    self.everything_it_said()
                ),
            }
        }
    }

    pub fn said(&self, what: &str) -> bool {
        self.seen.iter().any(|line| line.contains(what))
    }

    pub fn everything_it_said(&self) -> String {
        self.seen.join("\n")
    }

    pub fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }
}
