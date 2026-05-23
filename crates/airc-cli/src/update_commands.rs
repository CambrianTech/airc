use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn run_update() -> Result<(), Box<dyn std::error::Error>> {
    let source = install_source_dir()?;
    validate_source_checkout(&source)?;

    let branch = git_text(&source, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" || branch.is_empty() {
        return Err(format!(
            "install source is detached at {}; check out a branch before updating",
            source.display()
        )
        .into());
    }

    let before = git_text(&source, ["rev-parse", "--short", "HEAD"])?;
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .arg("fetch")
            .arg("--quiet")
            .arg("origin")
            .arg(&branch),
        "git fetch",
    )?;
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .arg("pull")
            .arg("--ff-only")
            .arg("--quiet"),
        "git pull --ff-only",
    )?;
    let after = git_text(&source, ["rev-parse", "--short", "HEAD"])?;

    run_installer(&source)?;

    if before == after {
        println!("Already at {after}.");
    } else {
        println!("Updated: {before} -> {after}");
    }
    Ok(())
}

fn install_source_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = env::var_os("AIRC_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or("HOME is not set; cannot resolve ~/.airc/src")?;
    Ok(PathBuf::from(home).join(".airc").join("src"))
}

fn validate_source_checkout(source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !source.join(".git").exists() {
        return Err(format!(
            "No git checkout at {}. Reinstall airc from the install script.",
            source.display()
        )
        .into());
    }
    if !source.join("install.sh").is_file() {
        return Err(format!(
            "install source {} is missing install.sh; reinstall airc from the install script",
            source.display()
        )
        .into());
    }
    Ok(())
}

fn run_installer(source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    run_checked(
        Command::new("bash")
            .arg(source.join("install.sh"))
            .env("AIRC_DIR", source)
            .env("AIRC_INSTALL_NO_PULL", "1"),
        "install.sh",
    )
}

fn git_text<const N: usize>(
    source: &Path,
    args: [&str; N],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(command_error("git", &output).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn run_checked(
    command: &mut Command,
    label: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_error(label, &output).into())
}

fn command_error(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };
    format!("{label} failed: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_source_prefers_airc_dir() {
        temp_env::with_vars(
            [
                ("AIRC_DIR", Some("/tmp/custom-airc")),
                ("HOME", Some("/tmp/home")),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/tmp/custom-airc")
                );
            },
        );
    }

    #[test]
    fn install_source_defaults_to_home_airc_src() {
        temp_env::with_vars(
            [
                ("AIRC_DIR", None::<&str>),
                ("HOME", Some("/tmp/home")),
                ("USERPROFILE", Some("/tmp/userprofile")),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/tmp/home/.airc/src")
                );
            },
        );
    }

    #[test]
    fn validate_source_requires_git_checkout() {
        let temp = tempfile::TempDir::new().unwrap();
        let error = validate_source_checkout(temp.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("No git checkout"));
    }
}
