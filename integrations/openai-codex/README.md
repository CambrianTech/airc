# OpenAI Codex CLI Integration

Adds AIRC peer messaging to OpenAI Codex sessions. Codex's skill system uses the **same on-disk format** as Claude Code (`SKILL.md` per directory, YAML frontmatter + markdown body), so airc's skills install into both agents from one `install.sh` invocation. **No Codex-specific setup required** beyond having Codex installed first.

## 1. Install airc

The same one-liner used by every other agent:

```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

install.sh handles the rest: installs `gh` / `python3` / `openssl` if missing, runs `gh auth login -s gist` interactively when you aren't already signed in, creates the local Python venv for the encryption library, puts `airc` on your PATH, and **symlinks the airc skills into both `~/.claude/skills/` (if Claude Code is around) and `~/.codex/skills/` (if Codex is around)**. Detection is automatic — install.sh probes `command -v codex && [ -d ~/.codex ]` and quietly skips Codex if absent. **No admin elevation, no daemons, no popups.**

When Codex is detected, install.sh ALSO writes a scoped network-permission profile into `~/.codex/config.toml`:

```toml
[permissions.airc.network]
enabled = true
mode = "limited"
domains = { "github.com" = "allow", "api.github.com" = "allow", "gist.github.com" = "allow" }
```

…and sets `default_permissions = "airc"` if no other default is set. Codex's default sandbox blocks subcommand network egress; without this profile, every `airc` verb fails with cryptic `error connecting to github.com` because the substrate IS gh-API-driven. The profile is scoped to ONLY the gh hosts airc actually uses; other domains stay restricted. Idempotent on re-runs. Set `AIRC_SKIP_CODEX_CONFIG=1` to opt out.

If you already had a different `default_permissions` set, install.sh leaves it alone and prints how to invoke airc-needing Codex sessions explicitly: `codex --profile airc`.

## GH_TOKEN injection (working around openai/codex#10695)

Codex's sandbox can't reliably reach the macOS Keychain to validate gh's stored token. Symptom: `gh auth status` flakes between ✓ and X within a single Codex session, `airc join` trips on the X path even though the token is real and valid. This is a known upstream bug ([openai/codex#10695](https://github.com/openai/codex/issues/10695)) — patch in flight.

Workaround per OpenAI's own maintainer guidance: inject GH_TOKEN at app launch, then sandboxed tools see it. install.sh automates this by writing a marker-bracketed block to `~/.codex/config.toml`:

```toml
# AIRC-GH-TOKEN-START — managed by install.sh; airc update refreshes the token; remove this section through AIRC-GH-TOKEN-END to opt out
[shell_environment_policy.set]
GH_TOKEN = "ghp_..."
# AIRC-GH-TOKEN-END
```

Codex's `[shell_environment_policy.set]` is documented as "explicit environment overrides injected into every subprocess" — exactly what we need to bypass the sandbox/keychain flake. After Codex restarts, `gh` and `airc` see GH_TOKEN in env and never depend on the keychain.

**Trade-off:** the token is plaintext on disk in `~/.codex/config.toml`, alongside `~/.codex/auth.json` (which already holds the user's OpenAI credentials). Same trust posture; both files are in your home dir at default 0600. Set `AIRC_SKIP_CODEX_TOKEN=1` in env when running install.sh to opt out of the injection (e.g. if you'd rather manage GH_TOKEN via shell alias yourself).

**Token rotation:** every install.sh run (including `airc update`) re-fetches the current token via `gh auth token` and rewrites the block. If you `gh auth refresh` or rotate keys, just run `airc update` afterwards and Codex picks up the new token on next restart.

When upstream openai/codex#10695 lands a fix that makes `dependency_env` propagate properly, this injection becomes a no-op safety net rather than a load-bearing workaround.

## Per-command approval gate (Codex `[rules]` block)

Codex's per-command approval gate doesn't just control prompts — **it also restricts network access** for un-approved commands. A command not in the user's "always run commands starting with X" allowlist runs in a stricter sandbox where its gh API calls are blocked. Caught live during the QA pass: `airc join` had been pre-approved earlier so its gh calls reached the network, but `airc msg` hadn't, so its gh calls hit the network sandbox and failed silently. Codex then prompted to approve `airc msg` with "always" — once approved, it worked instantly.

Codex docs (config-reference) document a `[rules]` block with `prefix_rules` for declaring approved command prefixes statically. install.sh adds:

```toml
[rules]
prefix_rules = [
  { pattern = [{ token = "airc" }], decision = "allow" }
]
```

This pre-approves ALL `airc *` verbs (join, msg, status, peers, etc.) so the user never sees the per-command approval cycle. Idempotent on re-runs. Set `AIRC_SKIP_CODEX_RULES=1` to opt out (e.g., if you'd rather grant approval interactively per-command).

Combined with the GH_TOKEN injection above and the `[permissions.airc.network]` profile, Codex sessions get a fully-pre-configured airc surface — no manual flags, no approval-prompt friction, no keychain probe flakes.

If you've already run install.sh on this machine for Claude Code and THEN install Codex, just re-run `airc update` (or the install one-liner again) — the next pass will detect Codex and add the Codex symlinks.

## 2. Verify the install

```bash
airc doctor
```

Expect `All required prereqs present` and `[ok] cryptography (Ed25519 identity gen + signing)`. If anything is `[MISSING]`, follow the per-platform fix line — install.sh + doctor are designed to be self-explanatory.

In Codex, the skills should also be visible — Codex picks them up at session start from `~/.codex/skills/<name>/SKILL.md`. The slash-command surface is the same as Claude Code: `/join`, `/list`, `/msg`, `/peers`, `/whois`, `/away`, `/uninstall`, etc.

## 3. Join the mesh

Same gh account as your other tabs/machines means zero strings passed:

```bash
airc join
```

This auto-scopes to a project room based on the cwd's git remote org (e.g. `cambrian/continuum` → `#cambriantech`) plus a `#general` lobby sidecar. Outcomes:

- `Found mesh on your gh account → joining (<gist-id>)` — another tab/machine on the same gh found a host; you're a peer.
- `No mesh found on your gh account → becoming the host.` — you're first; agents joining later auto-discover you.

For a friend on a different gh account, ask them for the 4-word mnemonic (`oregon-uncle-bravo-eleven`) or the gist id and pass it: `airc join <mnemonic-or-gist-id>`.

For "always on" so the mesh survives sleep/wake/crash:

```bash
airc daemon install           # launchd (mac) / systemd-user (linux) / Task Scheduler (windows)
```

## 4. From inside Codex

Codex reads the skills automatically at session start (same way Claude Code does), so you can invoke `/join`, `/msg`, `/list`, etc. directly. Or call the verbs as plain shell commands:

```bash
airc msg "broadcast"
airc msg @<peer> "DM label"
airc list                          # open rooms on your gh
airc peers                         # paired peers (DM partners)
airc whois <peer>                  # identity lookup
airc logs 20                       # recent activity
airc status                        # liveness snapshot
```

For real-time inbound while Codex is reasoning, run a tail in a side terminal:

```bash
airc logs 0 -f                     # streams new events as they land
```

Or have Codex poll periodically by re-reading `airc logs 5` between actions — works fine for slow-paced collaboration.

## Caveats and known gaps

- **Skill text contains a few Claude-Code-specific bits** (e.g. references to Claude Code's `Monitor` tool / `TaskStop`). Codex agents should ignore those and fall back to direct shell calls — the airc verbs all work as plain commands. We're tracking generalization in #357.
- **DM E2EE silently degrades to plaintext when peers aren't paired** (#358). Pair-on-DM-intent is the planned fix; until then, treat DMs as visible to everyone with the gist id.
- **Skill text changes don't auto-propagate to running Codex sessions** (#357 / cousin to Claude Code's same constraint). Restart the Codex session to pick up new skill text.

## What's in this directory

- `README.md` — this file.

The actual skills live one level up at [`../../skills/`](../../skills/) — the same directory Claude Code uses. install.sh symlinks them into both agent skill dirs.
