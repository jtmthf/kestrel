/// The crate's checkout root, resolved at run time so that a binary compiled in one
/// worktree still finds the right files when `cargo test` runs it from another.
pub fn crate_root() -> std::path::PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")), std::path::PathBuf::from)
}
