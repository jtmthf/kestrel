use std::fs;
use std::path::{Path, PathBuf};

const STORE: &str = "src/store";
const LOG: &str = "src/log.rs";
const FANOUT: &str = "src/fanout.rs";
const WORK: &str = "src/work.rs";
const COMPUTE: &str = "src/compute";

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    for entry in fs::read_dir(directory).expect("a readable directory") {
        let path = entry.expect("a readable entry").path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }

    files
}

#[test]
fn store_log_fanout_work_and_compute_are_modules_you_can_grep_for() {
    for boundary in [STORE, LOG, FANOUT, WORK, COMPUTE] {
        assert!(
            crate_root().join(boundary).exists(),
            "{boundary} is a port ADR-0005 says is a named module, and it is not there"
        );
    }
}

#[test]
fn no_sql_is_issued_from_anywhere_but_store_and_log() {
    let store = crate_root().join(STORE);
    let log = crate_root().join(LOG);

    for file in rust_files(&crate_root().join("src")) {
        if file.starts_with(&store) || file == log {
            continue;
        }
        assert!(
            !fs::read_to_string(&file)
                .expect("a readable source file")
                .contains("sqlx"),
            "{} reaches for sqlx; a session's whole truth is Store's and Log's to hold",
            file.display()
        );
    }
}
