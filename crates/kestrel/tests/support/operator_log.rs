use std::io;
use std::sync::{Arc, Mutex, OnceLock};

use kestrel::domain::RunId;

#[derive(Clone, Default)]
pub struct OperatorLog(Arc<Mutex<Vec<u8>>>);

/// One per test binary, because a tracing subscriber is global: every test in it shares it, and
/// reads only the lines that name its own Run.
pub fn capturing() -> &'static OperatorLog {
    static LOG: OnceLock<OperatorLog> = OnceLock::new();

    LOG.get_or_init(|| {
        let log = OperatorLog::default();
        let writing = log.clone();
        tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_ansi(false)
            .with_writer(move || writing.clone())
            .init();

        log
    })
}

impl OperatorLog {
    pub fn about(&self, run: RunId) -> Vec<String> {
        let run = run.to_string();

        String::from_utf8_lossy(&self.0.lock().expect("the log should not be poisoned"))
            .lines()
            .filter(|line| line.contains(&run))
            .map(str::to_owned)
            .collect()
    }
}

impl io::Write for OperatorLog {
    fn write(&mut self, written: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("the log should not be poisoned")
            .extend_from_slice(written);

        Ok(written.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
