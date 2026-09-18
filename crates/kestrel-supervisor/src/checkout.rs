use tokio::process::Command;

use crate::link::Checkout;

/// Into the working directory the agent is then spawned in. A branch the remote does not have
/// yet is cut from the base.
pub async fn check_out(checkout: &Checkout) -> Result<(), String> {
    let Checkout {
        repositories,
        base,
        branch,
    } = checkout;

    for repository in repositories {
        let directory = cloned_into(repository);
        let failed = |why: String| {
            format!("{repository} could not be checked out on the branch {branch}: {why}")
        };

        git(&["clone", "--branch", base, repository, directory])
            .await
            .map_err(failed)?;
        if branch != base
            && git(&["-C", directory, "checkout", branch, "--"])
                .await
                .is_err()
        {
            git(&["-C", directory, "checkout", "-b", branch])
                .await
                .map_err(failed)?;
        }
    }

    Ok(())
}

/// The directory `git clone` would choose for itself, named so the checkout after it can find it.
fn cloned_into(repository: &str) -> &str {
    let name = repository
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(repository);

    name.strip_suffix(".git").unwrap_or(name)
}

async fn git(arguments: &[&str]) -> Result<(), String> {
    let ran = Command::new("git")
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|error| format!("git could not be run: {error}"))?;

    if !ran.status.success() {
        return Err(String::from_utf8_lossy(&ran.stderr).trim().to_owned());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_cloned_into_the_directory_git_names_for_it() {
        assert_eq!(
            cloned_into("https://github.com/acme/widgets.git"),
            "widgets"
        );
        assert_eq!(cloned_into("https://github.com/acme/widgets/"), "widgets");
        assert_eq!(cloned_into("file:///tmp/kestrel"), "kestrel");
    }
}
