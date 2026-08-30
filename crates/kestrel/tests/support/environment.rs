//! A local-exec Environment as a test sees it: one a test scripts instead of the supervisor,
//! and the process tree behind an Environment a Run recorded.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(5);

pub struct Environment {
    _directory: TempDir,
    path: PathBuf,
}

impl Environment {
    #[cfg(unix)]
    pub fn executing(shell: &str) -> Self {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = TempDir::new().expect("a temporary directory");
        let path = directory.path().join("environment");
        fs::write(&path, format!("#!/bin/sh\n{shell}\n")).expect("the environment should write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("the environment should be executable");

        Self {
            _directory: directory,
            path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn named(environment: &str) -> Pid {
        let (driver, pid) = environment
            .split_once('/')
            .unwrap_or_else(|| panic!("{environment} does not name a driver and an instance"));
        assert_eq!(driver, "local-exec");

        Pid(pid.parse().unwrap_or_else(|_| panic!("{pid} is not a pid")))
    }
}

pub struct Pid(i32);

impl Pid {
    /// Reaped is gone: a process kestrel spawned and waited on leaves no zombie to signal.
    /// Waits without blocking, because the control plane doing the destroying shares this
    /// test's runtime.
    pub async fn is_gone(&self) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        while tokio::time::Instant::now() < deadline {
            if !self.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        panic!("the environment {} was never destroyed", self.0);
    }

    #[cfg(unix)]
    fn exists(&self) -> bool {
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(self.0, 0) == 0
        }
    }

    #[cfg(not(unix))]
    fn exists(&self) -> bool {
        false
    }
}
