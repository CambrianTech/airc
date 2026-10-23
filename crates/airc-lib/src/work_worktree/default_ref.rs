//! Default-branch discovery for both ordinary clones and bare staging mirrors.

use std::path::Path;

use airc_work::local_git::{CommandGitRunner, GitCommandRunner};

pub(super) fn origin_default(source: &Path) -> Result<String, String> {
    let git = CommandGitRunner;
    if git.run_git(source, &["rev-parse", "--is-bare-repository"])? != "true" {
        return git
            .run_git(source, &["symbolic-ref", "refs/remotes/origin/HEAD"])
            .map_err(|e| {
                format!("remote default unavailable; supply start_point explicitly: {e}")
            });
    }

    // A mirror stores the remote's branches under refs/heads, not refs/remotes.
    // Its own HEAD was copied from the source checkout and may name an agent's
    // branch or an old default. Ask the actual origin instead of guessing from
    // those local refs. Allocation still fetches and resolves the selected branch
    // once, recording that immutable commit before creating the card checkout.
    let advertised = git
        .run_git(source, &["ls-remote", "--symref", "origin", "HEAD"])
        .map_err(|e| format!("remote default unavailable; supply start_point explicitly: {e}"))?;
    advertised_default(&advertised)
}

fn advertised_default(advertised: &str) -> Result<String, String> {
    let mut heads = advertised.lines().filter_map(|line| {
        let (target, name) = line.split_once('\t')?;
        (name == "HEAD").then_some(target.strip_prefix("ref: refs/heads/")?)
    });
    let branch = heads
        .next()
        .filter(|branch| !branch.is_empty() && *branch != "HEAD")
        .ok_or_else(|| {
            "origin did not advertise a symbolic default branch; supply start_point explicitly"
                .to_owned()
        })?;
    if heads.next().is_some() {
        return Err("origin advertised multiple symbolic default branches".to_owned());
    }
    Ok(format!("refs/remotes/origin/{branch}"))
}

#[cfg(test)]
mod tests;
