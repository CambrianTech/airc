//! The GitHub sign-in prompt a person sees when this machine can't reach its
//! other machines because its GitHub sign-in stopped working.
//!
//! airc finds an account's other machines through a private gist in that
//! account (the account-registry rendezvous). When `gh` is not signed in, that
//! discovery silently stops: on 2026-10-05 a 5090 rebooted with an invalid gh
//! token and sat at 0 of 142 peers until a human noticed and ran
//! `gh auth login` by hand. The fix is to ask, once, in words a person can trust.
//!
//! WHEN is decided by [`should_offer`], a pure function: only the login-time
//! supervisor (a desktop session exists, so a window is visible), only when gh
//! is installed, only when it is not signed in. WHAT the person reads is
//! [`signin_copy`], the one copy of that text, written for the reader's moment
//! (Joel, 2026-10-06): what's wrong and who says so, why, the one action and how
//! to know it's genuine, and what happens if they decline.

use std::path::{Path, PathBuf};

use crate::runtime_context::RuntimeContext;

/// The four lines the person reads, before anything is asked of them. Line 3
/// names the click this platform's terminal uses to open a link, and says the
/// code is "already copied" only when this gh can copy it (`--clipboard` is gh
/// 2.25+; Cormac's IntelMac runs 2.24.3), so the window never tells a person
/// something untrue.
pub fn signin_copy(open_click: &str, code_copied: bool) -> [String; 4] {
    let code = if code_copied {
        "Paste the code (already copied)"
    } else {
        "Copy the code shown below, paste it there,"
    };
    [
        "Continuum: this computer can't reach your other machines. GitHub sign-in needed.".into(),
        "airc finds your machines through your GitHub account, and that sign-in expired.".into(),
        format!("{code} and click Authorize. Open ({open_click}): https://github.com/login/device"),
        "Not now? Close this window. Everything keeps running, just not connected.".into(),
    ]
}

/// gh's arguments for the device sign-in: `--clipboard` only when this gh has it.
fn gh_login_args(code_copied: bool) -> &'static str {
    if code_copied {
        "auth login --hostname github.com --git-protocol https --web --clipboard"
    } else {
        "auth login --hostname github.com --git-protocol https --web"
    }
}

/// Can this gh copy the one-time code to the clipboard? Read from gh's own help,
/// so the answer is this binary's, not a version table's.
fn gh_can_copy_code(gh: &Path) -> bool {
    std::process::Command::new(gh)
        .args(["auth", "login", "--help"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("--clipboard"))
        .unwrap_or(false) // a gh that cannot answer is asked for the shown-code flow, which every gh supports
}

/// What the window says once GitHub's sign-in finishes, or doesn't.
const SIGNED_IN: &str =
    "Signed in. Your computers can reach each other again. You can close this window.";
const NOT_SIGNED_IN: &str =
    "Sign-in didn't finish. Nothing changed; it will ask again at your next login.";

/// Offer the sign-in window? Only to a person who can see it (the login-time
/// supervisor), only when gh is installed (without it there is nothing to run),
/// and only when gh is not signed in.
pub fn should_offer(context: &RuntimeContext, gh_installed: bool, gh_signed_in: bool) -> bool {
    matches!(context, RuntimeContext::Supervisor) && gh_installed && !gh_signed_in
}

/// The script the window runs: the copy first, then GitHub's own device sign-in
/// with the code on the clipboard and the Enter it waits for already supplied,
/// then a plain-words result.
#[cfg(any(windows, test))] // the Windows window; tested everywhere
fn windows_script(gh: &Path, code_copied: bool) -> String {
    let say = |line: &str, color: &str| {
        format!(
            "Write-Host '  {}' -ForegroundColor {color}\n",
            line.replace('\'', "''")
        )
    };
    let mut script = String::from(
        "$Host.UI.RawUI.WindowTitle = 'Continuum: GitHub sign-in needed'\nWrite-Host ''\n",
    );
    let copy = signin_copy("Ctrl+click", code_copied);
    script += &say(&copy[0], "Yellow");
    script += &say(&copy[1], "Gray");
    script += &say(&copy[2], "Cyan");
    script += &say(&copy[3], "Gray");
    script += "Write-Host ''\n";
    script += &format!(
        "\"`n\" | & '{}' {}\n",
        gh.display().to_string().replace('\'', "''"),
        gh_login_args(code_copied)
    );
    script += &format!(
        "if ($LASTEXITCODE -eq 0) {{ {} }} else {{ {} }}\nRead-Host '  Press Enter to close'\n",
        say(SIGNED_IN, "Green").trim_end(),
        say(NOT_SIGNED_IN, "Red").trim_end()
    );
    script
}

#[cfg(any(target_os = "macos", test))] // the macOS window; tested everywhere
fn macos_script(gh: &Path, code_copied: bool) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut script = String::from("#!/bin/sh\nprintf '\\n'\n");
    for line in signin_copy("Cmd+click", code_copied) {
        script += &format!("printf '  %s\\n' {}\n", quote(&line));
    }
    script += "printf '\\n'\n";
    script += &format!(
        "if printf '\\n' | {} {}; then printf '\\n  %s\\n' {}; else printf '\\n  %s\\n' {}; fi\n",
        quote(&gh.display().to_string()),
        gh_login_args(code_copied),
        quote(SIGNED_IN),
        quote(NOT_SIGNED_IN)
    );
    script += "printf '  Press Return to close. '\nread _\n";
    script
}

/// Open the window, detached: the join it came from keeps running whether the
/// person signs in now, later, or not at all. The discovery tick re-checks gh
/// every cadence, so a sign-in is picked up without a restart.
pub fn open_signin_window(runtime_dir: &Path, gh: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(runtime_dir)?;
    let code_copied = gh_can_copy_code(gh);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let path = runtime_dir.join("gh-signin.ps1");
        std::fs::write(&path, windows_script(gh, code_copied))?;
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&path)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()?;
        Ok(path)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = runtime_dir.join("gh-signin.command");
        std::fs::write(&path, macos_script(gh, code_copied))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        std::process::Command::new("open").arg(&path).spawn()?;
        Ok(path)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (runtime_dir, gh);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no sign-in window on this platform yet: run `gh auth login`",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches (2026-10-05, the 5090): a machine cut off from the grid by an
    // invalid gh token with nobody asked to fix it, and the opposite failure, a window
    // popping up where nobody can see it (a boot service, an agent, a test run).
    #[test]
    fn only_a_person_at_a_login_is_asked_and_only_when_it_would_help() {
        assert!(should_offer(&RuntimeContext::Supervisor, true, false));
        assert!(
            !should_offer(&RuntimeContext::Supervisor, true, true),
            "already signed in"
        );
        assert!(
            !should_offer(&RuntimeContext::Supervisor, false, false),
            "no gh to run"
        );
        assert!(!should_offer(&RuntimeContext::Automation, true, false));
        assert!(!should_offer(&RuntimeContext::TestHarness, true, false));
        assert!(
            !should_offer(&RuntimeContext::InteractiveTerminal, true, false),
            "a terminal user gets the CLI message, not a window"
        );
    }

    // what this catches (Joel, 2026-10-06): a credential prompt that shows a code and
    // "press Enter" with no word of who is asking or why. Every line of the approved
    // copy is shown, in order, BEFORE gh runs, on both platforms.
    #[test]
    fn the_window_explains_itself_before_it_asks() {
        let gh = Path::new("C:/tools/gh.exe");
        for code_copied in [true, false] {
            for (script, click) in [
                (windows_script(gh, code_copied), "Ctrl+click"),
                (macos_script(gh, code_copied), "Cmd+click"),
            ] {
                let first_gh = script.find("auth login").expect("runs gh");
                let mut at = 0;
                for line in signin_copy(click, code_copied) {
                    let line = line.as_str();
                    // as each script spells an apostrophe: PowerShell '' and POSIX '\''
                    let found = [
                        line.replace('\'', "''"),
                        line.replace('\'', "'\\''"),
                        line.to_string(),
                    ]
                    .iter()
                    .find_map(|form| script[at..].find(form.as_str()))
                    .map(|i| i + at);
                    let found =
                        found.unwrap_or_else(|| panic!("copy line missing: {line}\n{script}"));
                    assert!(found < first_gh, "explained before asking: {line}");
                    at = found;
                }
                // Cormac on #1542: gh 2.24 has no --clipboard. The flag is passed, and
                // the copy says "already copied", only when this gh can copy it.
                assert_eq!(
                    script.contains("--clipboard"),
                    code_copied,
                    "flag only when supported"
                );
                assert_eq!(
                    script.contains("already copied"),
                    code_copied,
                    "never claim a copy that didn't happen"
                );
                assert!(
                    script.contains("--web"),
                    "the browser opens on GitHub's page"
                );
                assert!(script.contains("https://github.com/login/device"));
            }
        }
    }
}
