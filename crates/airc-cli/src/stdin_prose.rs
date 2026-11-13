//! `--stdin` prose bodies: the one reader `airc msg --stdin` and `airc publish --stdin` share.
//!
//! Both guards were found by review rather than by design (#1382): an empty pipe posted a
//! BLANK message, and a terminal blocked forever waiting for an EOF the operator had to know
//! to send. The two verbs carried separate copies of them; a flag whose guards could drift
//! between verbs teaches one contract and delivers another, so there is one reader.
//!
//! The source is injected so tests never read the test runner's own stdin — a runner whose
//! stdin stays open (a backgrounded agent shell) hung the whole `cargo test` run in that read
//! (card f4703abe: 38 minutes on IntelMac, 2026-10-15).

use std::error::Error;
use std::io::{IsTerminal, Read};

/// The process's stdin as `verb`'s `--stdin` body.
pub(crate) fn read_from_stdin(verb: &str) -> Result<String, Box<dyn Error>> {
    let stdin = std::io::stdin();
    let is_terminal = stdin.is_terminal();
    read(stdin.lock(), is_terminal, verb)
}

/// Read `input` to EOF verbatim — no trim, no interpretation; a heredoc's trailing newline is
/// the author's — refusing a terminal (it would wait forever) and an empty or whitespace-only
/// body (it would post a blank frame that reads as a delivery failure).
pub(crate) fn read(
    mut input: impl Read,
    is_terminal: bool,
    verb: &str,
) -> Result<String, Box<dyn Error>> {
    if is_terminal {
        return Err(format!(
            "`--stdin` was passed but stdin is a terminal — nothing is piped in and this would \
             wait forever. Redirect a file (`airc {verb} --stdin < body.txt`) or use a heredoc."
        )
        .into());
    }
    let mut buf = String::new();
    input
        .read_to_string(&mut buf)
        .map_err(|error| format!("reading message body from stdin: {error}"))?;
    if buf.trim().is_empty() {
        return Err(format!(
            "`--stdin` produced an empty message body — nothing was piped in, or it expanded to \
             whitespace. Refusing rather than sending a blank `airc {verb}`."
        )
        .into());
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the two silent failures #1382 found — a terminal that hangs and an
    // empty pipe that posts a blank body — without reading the runner's real stdin.
    #[test]
    fn refuses_a_terminal_and_an_empty_body_and_keeps_text_verbatim() {
        let terminal = read(std::io::empty(), true, "msg").unwrap_err().to_string();
        assert!(
            terminal.contains("stdin is a terminal") && terminal.contains("airc msg --stdin"),
            "{terminal}"
        );
        let empty = read(" \n\t".as_bytes(), false, "publish")
            .unwrap_err()
            .to_string();
        assert!(
            empty.contains("empty message body") && empty.contains("airc publish"),
            "{empty}"
        );
        assert_eq!(read("body\n".as_bytes(), false, "msg").unwrap(), "body\n");
    }
}
