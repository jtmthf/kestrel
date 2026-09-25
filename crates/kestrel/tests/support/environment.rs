//! A local-exec supervisor as a test sees it: one a test scripts instead of the real one, the
//! process tree behind a supervisor a Run recorded, and the directory a local Instance is.

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
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nexport GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false\n{shell}\n"
            ),
        )
        .expect("the environment should write");
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

    /// What the script wrote beside itself, where it outlives every Instance.
    pub fn wrote(&self, name: &str) -> String {
        fs::read_to_string(self._directory.path().join(name))
            .unwrap_or_else(|error| panic!("the environment wrote no {name}: {error}"))
            .trim()
            .to_owned()
    }

    pub fn named(supervisor: &str) -> Pid {
        let pid = supervisor
            .strip_prefix("local-exec/")
            .unwrap_or_else(|| panic!("{supervisor} is not a local supervisor"));

        Pid(pid.parse().unwrap_or_else(|_| panic!("{pid} is not a pid")))
    }

    pub fn process(pid: &str) -> Pid {
        Pid(pid.parse().unwrap_or_else(|_| panic!("{pid} is not a pid")))
    }

    pub fn workspace_of(instance: &str) -> PathBuf {
        let name = instance
            .strip_prefix("local-exec/")
            .unwrap_or_else(|| panic!("{instance} is not a local instance"));

        std::env::temp_dir().join(name)
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

        panic!("the supervisor {} was never stopped", self.0);
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
