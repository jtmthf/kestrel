//! Where a test's image comes from. CI builds `kestrel-env` and the control plane once per
//! change and pushes them tagged by commit (ADR-0027), and names each with a source variable so
//! the consuming job pulls what its sibling built rather than building a second one.

use super::docker;

/// The `kestrel-env` image CI built for this change, if any.
pub const ENV: &str = "KESTREL_ENV_IMAGE_SOURCE";
/// The control-plane image CI built for this change, if any.
pub const CONTROL_PLANE: &str = "KESTREL_CONTROL_IMAGE_SOURCE";

/// The image `variable` names, or `None` when this run builds its own.
pub fn sourced(variable: &str) -> Option<String> {
    std::env::var(variable)
        .ok()
        .filter(|image| !image.is_empty())
}

/// The image `variable` names, or the one built from `dockerfile` and tagged `local` when
/// nothing named one, which is every local run.
pub fn built_or_named(variable: &str, dockerfile: &str, local: &str) -> String {
    if let Some(image) = sourced(variable) {
        return image;
    }

    docker::completed(
        &["build", "--file", dockerfile, "--tag", local, "."],
        "building the image",
    );

    local.to_owned()
}
