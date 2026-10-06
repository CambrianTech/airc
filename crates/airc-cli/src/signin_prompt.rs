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
/// names the click this platform's terminal uses to open a link.
pub fn signin_copy(open_click: &str) -> [String; 4] {
    [
        "Continuum: this computer can't reach your other machines. GitHub sign-in needed.".into(),
        "airc finds your machines through your GitHub account, and that sign-in expired.".into(),
        format!("Paste the code (already copied) and click Authorize. Open ({open_click}): https://github.com/login/device"),
        "Not now? Close this window. Everything keeps running, just not connected.".into(),
    ]
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
fn windows_script(gh: &Path) -> String {
    let say = |line: &str, color: &str| {
        format!(
            "Write-Host '  {}' -ForegroundColor {color}\n",
            line.replace('\'', "''")
        )
    };
    let mut script = String::from(
        "$Host.UI.RawUI.WindowTitle = 'Continuum: GitHub sign-in needed'\nWrite-Host ''\n",
    );
    let copy = signin_copy("Ctrl+click");
    script += &say(&copy[0], "Yellow");
    script += &say(&copy[1], "Gray");
    script += &say(&copy[2], "Cyan");
    script += &say(&copy[3], "Gray");
    script += "Write-Host ''\n";
    script += &format!(
        "\"`n\" | & '{}' auth login --hostname github.com --git-protocol https --web --clipboard\n",
        gh.display().to_string().replace('\'', "''")
    );
    script += &format!(
        "if ($LASTEXITCODE -eq 0) {{ {} }} else {{ {} }}\nRead-Host '  Press Enter to close'\n",
        say(SIGNED_IN, "Green").trim_end(),
        say(NOT_SIGNED_IN, "Red").trim_end()
    );
    script
}

#[cfg(any(target_os = "macos", test))] // the macOS window; tested everywhere
fn macos_script(gh: &Path) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut script = String::from("#!/bin/sh\nprintf '\\n'\n");
    for line in signin_copy("Cmd+click") {
        script += &format!("printf '  %s\\n' {}\n", quote(&line));
    }
    script += "printf '\\n'\n";
    script += &format!(
        "if printf '\\n' | {} auth login --hostname github.com --git-protocol https --web --clipboard; then printf '\\n  %s\\n' {}; else printf '\\n  %s\\n' {}; fi\n",
        quote(&gh.display().to_string()),
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
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let path = runtime_dir.join("gh-signin.ps1");
        std::fs::write(&path, windows_script(gh))?;
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
        std::fs::write(&path, macos_script(gh))?;
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
        for (script, click) in [
            (windows_script(gh), "Ctrl+click"),
            (macos_script(gh), "Cmd+click"),
        ] {
            let first_gh = script.find("auth login").expect("runs gh");
            let mut at = 0;
            for line in signin_copy(click) {
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
                let found = found.unwrap_or_else(|| panic!("copy line missing: {line}\n{script}"));
                assert!(found < first_gh, "explained before asking: {line}");
                at = found;
            }
            assert!(
                script.contains("--web --clipboard"),
                "the code is copied and the browser opens"
            );
            assert!(script.contains("https://github.com/login/device"));
        }
    }
}
