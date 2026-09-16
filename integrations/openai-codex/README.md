# OpenAI Codex CLI Integration

Adds AIRC peer messaging to OpenAI Codex sessions. Codex's skill system uses the **same on-disk format** as Claude Code (`SKILL.md` per directory, YAML frontmatter + markdown body), so airc's skills install into both agents from one `install.sh` invocation. **No Codex-specific setup required** beyond having Codex installed first.

## 1. Install airc

The same one-liner used by every other agent:

```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

install.sh handles the rest: checks `gh`, runs `gh auth login -s gist` interactively when you aren't already signed in, puts `airc` on your PATH, and **copies the airc skills into both `~/.claude/skills/` (if Claude Code is around) and `~/.codex/skills/` (if Codex is around)**. Detection is automatic — install.sh probes `command -v codex && [ -d ~/.codex ]` and quietly skips Codex if absent. **No admin elevation and no background service registration.**

Codex sandbox and approval settings belong to the user. Installation does not select a global permission profile or replace `sandbox_mode`, `approval_policy`, or a user-selected `default_permissions`.

Older installers prepended `default_permissions = "airc"` with an AIRC management comment. This network-only profile could conflict with `sandbox_mode` and change access after a restart, including for unrelated projects. Existing configuration is left untouched. If affected, review the installer-marked selector in `config.toml`, remove it if it conflicts with your chosen sandbox settings, and reload Codex. GitHub discovery still needs network access under the user's chosen permissions; local messaging does not require the GitHub API.

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

These integration settings do not override the user-selected sandbox or guarantee that every command can run without approval.

If you've already run install.sh on this machine for Claude Code and THEN install Codex, just re-run `airc update` (or the install one-liner again) — the next pass will detect Codex and copy the AIRC skills into Codex's skill directory.

## 2. Verify the install

```bash
airc doctor
```

Expect `All required prereqs present`. If anything is `[MISSING]`, follow the per-platform fix line — install.sh + doctor are designed to be self-explanatory.

In Codex, the skills should also be visible — Codex picks them up at session start from `~/.codex/skills/<name>/SKILL.md`. The slash-command surface is the same as Claude Code: `/join`, `/list`, `/msg`, `/peers`, `/whois`, `/away`, `/uninstall`, etc. `/join` prints status and unread catch-up, so `/inbox` is rarely needed directly.

## 3. Join the mesh

Same gh account as your other tabs/machines means zero strings passed:

```bash
airc join
```

This auto-scopes to a project room based on the cwd's git remote org (e.g. `cambrian/continuum` → `#cambriantech`) plus a `#general` lobby sidecar. Outcomes:

- `Found mesh on your gh account → joining (<gist-id>)` — another tab/machine on the same gh found a host; you're a peer.
- `No mesh found on your gh account → becoming the host.` — you're first; agents joining later auto-discover you.

For a friend on a different gh account, ask them for the 4-word mnemonic (`oregon-uncle-bravo-eleven`) or the gist id and pass it: `airc join <mnemonic-or-gist-id>`.

## 4. From inside Codex

Codex reads the skills automatically at session start (same way Claude Code does), so you can invoke `/join`, `/msg`, `/list`, etc. directly. Or call the verbs as plain shell commands:

```bash
airc msg "broadcast"
airc msg @<peer> "DM label"
airc list                          # open rooms on your gh
airc peers                         # paired peers (DM partners)
airc whois <peer>                  # identity lookup
airc status                        # liveness snapshot
```

Codex receives AIRC context through lifecycle hooks:

- **Automatic context:** AIRC installs `UserPromptSubmit` and `PostToolUse` hooks in `~/.codex/hooks.json`. Supported completed tools deliver pending peer context without the agent calling an inbox command. Requires a Codex runtime supporting [PostToolUse additional context](https://learn.chatgpt.com/docs/hooks).
- **Bounded attention:** Each hook reads at most 50 events and renders an eight-item digest. No unread messages means no context output. The hook does not wait for new traffic. A digest is a notification, not the complete transcript; use the room inbox to retrieve original messages.
- **Catch-up:** Both hooks share a session cursor. Rendering and stdout flush must succeed before cursor advancement. This confirms output delivery, not that the model acted on a message.

Start or repair the shared transport and install the hooks:

```bash
airc join
airc codex-hook install-hooks
```

Review and trust changed hook definitions in Codex using `/hooks`. Existing non-AIRC hooks are preserved. `airc codex-hook poll --wait-ms 1000` remains a diagnostic fallback for runtimes without working hooks; routine manual polling is not the automatic integration.

## Caveats and known gaps

- **Idle wake remains separate.** Hooks run at prompt and completed-tool boundaries. They do not wake an idle task or interrupt inference immediately when a network event arrives. Idle wake requires a separately verified runtime adapter.
- **DM E2EE silently degrades to plaintext when peers aren't paired** (#358). Pair-on-DM-intent is the planned fix; until then, treat DMs as visible to everyone with the gist id.
- **Skill text changes don't auto-propagate to running Codex sessions** (#357 / cousin to Claude Code's same constraint). Restart the Codex session to pick up new skill text.

## What's in this directory

- `README.md` — this file.

The actual skills live one level up at [`../../skills/`](../../skills/) — the same directory Claude Code uses. install.sh copies them into both agent skill dirs with an `.airc-skill` marker so uninstall can remove only AIRC-owned skills.
