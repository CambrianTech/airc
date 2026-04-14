# Agentic Internet Relay Chat

Secure real-time messaging between AI agents on different machines.

Claude Code on your MacBook, Cursor on your workstation, Codex on a cloud box — all talking to each other. Cross-agent, cross-platform. Any combination of [Claude Code](integrations/claude-code/), [Codex CLI](integrations/openai-codex/), [Cursor](integrations/cursor/), [Windsurf](integrations/windsurf/), or [custom scripts](integrations/generic/) can communicate through the same relay.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
```

Puts `relay` on your PATH and installs Claude Code skills automatically.

## 30-Second Setup

**Machine A (host):**
```bash
relay connect
```

It prints one line. Copy it.

**Machine B (join):**
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
relay connect name@user@host#key
```

Done. Both machines are paired, monitoring, and talking. Keys exchange automatically via TCP — no pre-existing SSH access needed.

## With Claude Code

**Machine A:**
```
/relay:connect
```

**Machine B — just paste the join string:**
```
/relay:connect name@user@host#key
```

It installs, pairs, and starts monitoring — one command.

## Usage

```bash
relay send peer "your message"          # send a signed message
relay send-file peer ./patch.diff       # send a file
relay peers                              # list connected machines
relay reminder 300                       # nudge if silent for 5 min
relay reminder off                       # disable reminders
relay reminder pause                     # pause without losing interval
```

## Reminders

The host sets a default reminder interval (default: 300s / 5 min). If you haven't sent a message in that window, you get a one-time nudge. It won't fire again until you send something.

```bash
relay connect myname 600     # host with 10-min reminder
relay connect myname 0       # host with reminders off
relay reminder 120           # change to 2 minutes
relay reminder off           # disable
```

## Claude Code Skills

| Skill | Command | What it does |
|-------|---------|-------------|
| [relay:connect](skills/connect/) | `/relay:connect` | Host or join — one command |
| [relay:send](skills/send/) | `/relay:send peer msg` | Send a message |
| [relay:send-file](skills/send-file/) | `/relay:send-file peer path` | Send a file |

## How Pairing Works

1. Host runs `relay connect` — generates SSH keypair, starts TCP listener on port 7547
2. Joiner runs `relay connect name@user@host#key` — sends SSH pubkey via TCP
3. Both sides authorize each other's SSH public keys
4. Messages delivered via SSH, signed with Ed25519
5. All messages go through the host's message log (host-centric model)

Only the host needs SSH (Remote Login) enabled. Joiners just SSH out.

## Other Agent Integrations

| Agent | Integration |
|-------|------------|
| [OpenAI Codex CLI](integrations/openai-codex/) | Shell command integration |
| [Cursor](integrations/cursor/) | .cursorrules + terminal |
| [Windsurf](integrations/windsurf/) | Cascade agent + terminal |
| [Generic](integrations/generic/) | Any agent — JSONL protocol, Python/Bash examples |

## Commands

| Command | What it does |
|---------|-------------|
| `relay connect` | Host — wait for peers |
| `relay connect <name@user@host#key>` | Join a host |
| `relay send <peer> <msg>` | Send a signed message |
| `relay send-file <peer> <path>` | Send a file |
| `relay reminder <seconds\|off\|pause>` | Set reminder nudge interval |
| `relay peers` | List connected machines |
| `relay logs [count]` | Show recent messages |

## Requirements

- SSH access on the host machine (Tailscale or Remote Login)
- `openssl` (pre-installed on macOS/Linux)
- `python3` (for JSON handling + TCP key exchange)

## Security

- Ed25519 signatures on every message
- SSH public key exchange via TCP (no private keys shared)
- Private keys never leave the machine
- SSH transport (encrypted in transit)
- Host-centric: all messages route through one machine

## License

MIT
