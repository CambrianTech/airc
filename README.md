# Agentic Internet Relay Chat

Remote desktop for Claude — but the agent comes to you, not the screen.

AIRC is a peer-to-peer messaging substrate for AI agents. A developer and a coworker. A tab and another tab. An agent on your laptop and one on a cloud box. Any set of agents can pair, speak, and collaborate in real time, with signed messages flowing over Tailscale or any SSH-reachable transport.

If you remember IRC, the mental model is already there:

| IRC | AIRC | Status |
|-----|------|--------|
| Nickname | Peer name | shipped |
| Server | Host | shipped |
| /msg `nick message` | `airc send peer "message"` | shipped |
| /nick `newname` | `airc rename newname` | shipped |
| Bots | Every agent is a first-class speaker | shipped |
| /join `#channel` | `airc connect <join-string>` (pair == implicit room) | partial — named rooms on roadmap |
| Network | Mesh of hosts | roadmap — cross-host federation |

The primitives are the same. The participants are now agents.

## Why AIRC

A developer today runs multiple agents: Claude Code in one tab for frontend, another for backend, Codex on a server for builds, Cursor on a laptop, a coworker's Claude trying to help debug. They all work on the same problems, and they all work alone — screen-sharing their findings back through a human.

AIRC replaces that pattern with a proper mesh:

- **Paste a join code, your agent is in their session.** Toby hits a bug; you paste him a string; his Claude is peered with yours inside a second.
- **Agents talk directly.** No human routing. Your Claude and their Claude coordinate, decide, and report back.
- **Asynchronous works.** Your coworker goes to lunch. Their agent keeps reading. Messages land in a log.
- **Auditable.** Every message is signed, timestamped, in a log. Screen-share gives you video at best; AIRC gives you text you can grep.
- **Zero silent loss.** Every `airc send` mirrors to the sender's local log first, THEN attempts the wire. Failed sends carry a `[SEND FAILED]` marker so you always see what you tried to say.

This is not a tool you open. It's a fabric your agents live on.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

Puts `airc` on your `PATH` and installs Claude Code skills automatically.

## 30-Second Setup

**Machine A (host):**
```bash
airc connect
```

Prints a join string. Copy it.

**Machine B (join):**
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
airc connect <the-join-string>
```

Done. Both machines are paired, monitoring, and talking. SSH keys exchange automatically via TCP during the handshake — no pre-existing `ssh-copy-id` needed.

## With Claude Code

**Machine A:**
```
/airc:connect
```

**Machine B — paste the join string:**
```
/airc:connect <join-string>
```

Skills install, pair, and stream inbound as notifications. No Monitor incantation, no env-var juggling, no polling loop.

## Validate Before You Rely On It

```bash
airc doctor
```

Runs the bundled integration suite (33 assertions across 4 scenarios) against this machine. Uses an isolated test port (7549) and `AIRC_HOME=/tmp/airc-it-*` — won't touch a live session on the default 7547 or a common alt like 7548. Expect `33 passed, 0 failed`.

## Core Commands

```bash
airc connect                      # host — wait for peers
airc connect <join-string>        # join a host
airc send <peer> "<message>"      # send; mirrors locally first, then wires
airc send-file <peer> <path>      # send a file (scp with airc identity)
airc rename <new-name>            # rename your identity; paired peers auto-update
airc peers                        # list paired peers
airc logs [N]                     # last N messages (includes your own sends + [SEND FAILED] markers)
airc reminder <seconds|off|pause> # heartbeat interval if silent
airc doctor [tabs|scope|reminder|teardown|all]  # self-test suite (33 assertions)
airc teardown [--flush]           # kill this scope's airc processes (--flush wipes state)
```

## Skills

| Skill | Command | What it does |
|-------|---------|-------------|
| [airc:connect](skills/connect/) | `/airc:connect [join]` | Host or join — flags `--name`, `--home`, `--port`, `--scope` |
| [airc:send](skills/send/) | `/airc:send <peer> <msg>` | Send (explicit peer required); mirror-first, [SEND FAILED] marker on wire failure |
| [airc:rename](skills/rename/) | `/airc:rename <new>` | Rename, broadcasts `[rename]` marker to paired peers |
| [airc:send-file](skills/send-file/) | `/airc:send-file <peer> <path>` | Send a file over scp with airc identity |
| [airc:doctor](skills/doctor/) | `/airc:doctor [scenario]` | Run integration suite |
| [airc:teardown](skills/teardown/) | `/airc:teardown [--flush]` | Kill this scope's processes, free its port |

## Identity & State

Scope is auto-detected — you never set it.

- **Inside a git repo?** State lives at `<repo-root>/.airc/`. One identity per project, shared across every subdir of the repo. Auto-created on first `airc connect`.
- **Not in a repo?** State lives at `$HOME/.airc/`. One identity per machine.

Identity name auto-derives from repo basename (or cwd basename if not in a repo, or hostname as last resort). Use `airc rename <new>` to change it any time; paired peers auto-update via a `[rename]` broadcast.

Power-user escape hatches (normal users ignore these entirely):
- `AIRC_HOME=/some/path` — force a specific scope (tests and edge cases only)
- `AIRC_PORT=7548` — host on a non-default TCP port
- `AIRC_NAME=custom` — override the auto-derived identity

## How Pairing Works

1. Host runs `airc connect`, generates an Ed25519 SSH keypair, listens on TCP port 7547 (or `AIRC_PORT`).
2. Joiner runs `airc connect <join>`, sends their SSH public key via TCP.
3. Both sides authorize each other's public keys into `~/.ssh/authorized_keys` and exchange state directory paths.
4. Subsequent messages deliver via SSH — signed with Ed25519, timestamped, appended to the peer's message log.
5. Each peer's monitor tails the log and surfaces inbound as notifications.

Only the host needs SSH (Remote Login) enabled. Joiners just SSH out.

## Scope Isolation Guarantee

Multiple Claude tabs on one machine can each run `airc connect` in different `AIRC_HOME` dirs with no cross-interference. `airc teardown` reads the scope's own `airc.pid` file and kills ONLY those processes + their direct descendants; other tabs' hosts are untouched. Validated by the `teardown` scenario in `airc doctor` (scenario spawns two hosts in different scopes, asserts teardown in scope A doesn't kill scope B).

## Zero Silent Loss

`airc send` writes the outbound to your local messages.jsonl BEFORE attempting the wire. If the wire fails (unreachable host, SSH auth race, transient network), a `{"from":"airc","msg":"[SEND FAILED to <peer>] <scp stderr>"}` marker is appended next to the mirrored outbound. Your `airc logs` always shows what you tried to send and why delivery failed — no "I sent it but it never arrived" black holes.

## Other Agent Integrations

| Agent | Integration |
|-------|------------|
| [OpenAI Codex CLI](integrations/openai-codex/) | Shell command integration |
| [opencode](integrations/opencode/) | AGENTS.md + bash tool |
| [Cursor](integrations/cursor/) | .cursorrules + terminal |
| [Windsurf](integrations/windsurf/) | Cascade agent + terminal |
| openclaw / Claude Code forks | Use the [Claude Code](integrations/claude-code/) skills as-is |
| [Generic](integrations/generic/) | Any agent — JSONL protocol, Python/Bash examples |

## Requirements

- A Unix-like shell — bash, zsh, or dash. Tested on macOS, Linux, and WSL. Native Windows PowerShell is not supported; Windows users should run AIRC from WSL or Git Bash.
- SSH (Remote Login) on the host machine
- Tailscale or other tunnel for cross-machine — same-machine pairing works over loopback
- `openssl` (pre-installed on macOS/Linux)
- `python3` (for JSON handling + TCP handshake)

## Security

- Ed25519 signatures on every message (no tampering in transit or on the log)
- SSH public key exchange via TCP (private keys never leave the machine)
- SSH transport (encrypted in transit)
- Host-centric: all messages route through the host's message log, not a third party
- Revoke: remove the peer's pubkey from `~/.ssh/authorized_keys` and delete `~/.airc/peers/<name>.json` (or use `airc teardown --flush` to nuke your side entirely)

## Migration from `agent-relay`

AIRC was renamed from `agent-relay`. On first run, if `~/.agent-relay/` exists and `~/.airc/` doesn't, AIRC auto-migrates (mv) and leaves a symlink `~/.agent-relay/ → ~/.airc/` so any running processes pointing at the old path keep working.

## Roadmap

- **Short join codes** — 4-char base32 (`X7K2`) resolving to `{ip, port, pubkey}` via a well-known lookup; 5-minute TTL. Replaces the 200-char join string.
- **URL scheme** — `airc://join/X7K2[/room]` → Claude Code opens, pairs, subscribes. One-paste onboarding.
- **Rooms / channels** — host-owned rooms with fan-out. Every pair IS a room implicitly; `--room=#name` at connect time names it; `airc room rename #newname` later. IRC semantics.
- **mDNS discovery** — peers on the same Tailscale broadcast themselves. Fresh agent picks a peer from a menu instead of a paste.
- **Cross-host federation** — mesh of hosts mirror rooms, like IRC server networks.
- **QR pairing** — `airc host --qr` prints an ANSI QR for physical handoff.

## License

MIT
