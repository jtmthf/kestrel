use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

pub struct Written {
    home: PathBuf,
    files: BTreeMap<String, String>,
}

pub fn write(home: &Path, files: BTreeMap<String, String>) -> io::Result<Written> {
    for (path, contents) in &files {
        let at = home.join(path);
        if let Some(parent) = at.parent() {
            let mut directory = DirBuilder::new();
            directory.recursive(true);
            #[cfg(unix)]
            directory.mode(0o700);
            directory.create(parent)?;
        }

        let mut file = OpenOptions::new();
        file.write(true).create(true).truncate(true);
        #[cfg(unix)]
        file.mode(0o600);
        file.open(&at)?.write_all(contents.as_bytes())?;
    }

    Ok(Written {
        home: home.to_owned(),
        files,
    })
}

impl Written {
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// A file the harness removed is a login it gave up, which the next Run may still need.
    pub fn refreshed(&self) -> BTreeMap<String, String> {
        self.files
            .iter()
            .filter_map(|(path, written)| {
                let now = fs::read_to_string(self.home.join(path)).ok()?;
                (now != *written).then(|| (path.clone(), now))
            })
            .collect()
    }

    /// An Instance outlives its Runs, so what a Run was handed leaves with it.
    pub fn remove(self) {
        for path in self.files.keys() {
            let _ = fs::remove_file(self.home.join(path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "kestrel-login-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).expect("a home");
        home
    }

    fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(path, contents)| ((*path).to_owned(), (*contents).to_owned()))
            .collect()
    }

    #[test]
    fn only_what_the_harness_rewrote_is_handed_back() {
        let home = home();
        let written = write(
            &home,
            files(&[(".harness/auth.json", "first"), (".other/token", "kept")]),
        )
        .expect("written");

        fs::write(home.join(".harness/auth.json"), "refreshed").expect("rewritten");

        assert_eq!(
            written.refreshed(),
            files(&[(".harness/auth.json", "refreshed")])
        );
        written.remove();
        assert!(!home.join(".harness/auth.json").exists());
        assert!(!home.join(".other/token").exists());
    }

    #[test]
    fn a_login_the_harness_removed_is_not_handed_back_as_nothing() {
        let home = home();
        let written = write(&home, files(&[("auth.json", "first")])).expect("written");

        fs::remove_file(home.join("auth.json")).expect("removed");

        assert!(written.refreshed().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_login_is_readable_by_the_agents_user_alone() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = home();
        write(&home, files(&[(".harness/auth.json", "secret")])).expect("written");

        let mode = |path: &str| {
            fs::metadata(home.join(path))
                .expect("written")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(".harness/auth.json"), 0o600);
        assert_eq!(mode(".harness"), 0o700);
    }
}
