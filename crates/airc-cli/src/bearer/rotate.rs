use std::env;

const DEFAULT_GIST_MAX_BYTES: usize = 600_000;
const DEFAULT_GIST_KEEP_LINES: usize = 1000;

pub fn rotate_if_needed(content: &str) -> String {
    let max_bytes = env_usize("AIRC_GIST_MAX_BYTES", DEFAULT_GIST_MAX_BYTES);
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let target_bytes = env_usize("AIRC_GIST_TARGET_BYTES", max_bytes / 2);
    let keep_lines = env_usize("AIRC_GIST_KEEP_LINES", DEFAULT_GIST_KEEP_LINES);
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
        let line_bytes = line.len() + 1;
        if bytes + line_bytes > target_bytes || kept.len() >= keep_lines {
            break;
        }
        kept.push(line);
        bytes += line_bytes;
    }
    kept.reverse();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_keeps_recent_lines_under_target() {
        temp_env::with_vars(
            [
                ("AIRC_GIST_MAX_BYTES", Some("20")),
                ("AIRC_GIST_TARGET_BYTES", Some("12")),
                ("AIRC_GIST_KEEP_LINES", Some("10")),
            ],
            || {
                let rotated = rotate_if_needed("one\ntwo\nthree\nfour\nfive\n");
                assert_eq!(rotated, "four\nfive\n");
            },
        );
    }
}
