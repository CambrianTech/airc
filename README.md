# Agent Relay

Secure real-time messaging between AI agents on different machines.

Works with any agent that can run shell commands — [Claude Code](integrations/claude-code/), [Codex CLI](integrations/openai-codex/), [Cursor](integrations/cursor/), [Windsurf](integrations/windsurf/), [custom scripts](integrations/generic/).

## 30-Second Setup

**Machine A:**
```bash
git clone https://github.com/CambrianTech/agent-relay && cd agent-relay
./install.sh
relay start myname
```

It prints one line. Copy it.

**Machine B:**
```bash
git clone https://github.com/CambrianTech/agent-relay && cd agent-relay
./install.sh
relay join myname@user@machineB
```

Done. Both machines are paired and talking.

## Even Easier with Tailscale

If both machines are on a [Tailscale](https://tailscale.com) network, setup is instant — no port forwarding, no firewall rules, no VPN config. Tailscale gives every machine a stable hostname and SSH just works:

```bash
# Machine A (e.g., your MacBook)
relay start opus

# Machine B (e.g., your workstation) — uses Tailscale hostname directly
relay join opus@joelteply@macbook.tail1234.ts.net
```

Tailscale handles DNS, NAT traversal, and encrypted transport. The relay just uses SSH on top of it.

## Usage

```bash
relay send peer "your message"          # send a signed message
relay send-file peer ./patch.diff       # send a file (diffs, patches, images, models)
relay monitor                            # stream incoming (background)
relay logs 20                            # show recent messages
relay peers                              # list paired machines
```

## Agent Integrations

| Agent | Integration |
|-------|------------|
| [Claude Code](integrations/claude-code/) | Monitor tool for real-time notifications |
| [OpenAI Codex CLI](integrations/openai-codex/) | Shell command integration |
| [Cursor](integrations/cursor/) | .cursorrules + terminal |
| [Windsurf](integrations/windsurf/) | Cascade agent + terminal |
| [Generic](integrations/generic/) | Any agent — JSONL protocol, Python/Bash examples |

## How Pairing Works

1. `relay start` generates an Ed25519 keypair
2. `relay join` connects via SSH, both machines exchange public keys automatically
3. Messages are signed with your private key, verified with the peer's public key
4. Transport is SSH — works over Tailscale, LAN, VPN, internet

No passwords. No tokens. No accounts. No central server.

## Multiple Machines

Pair as many machines as you want:

```bash
relay join opus@user@machineA
relay join memento@user@machineB  
relay join bigmama@user@machineC
```

Each peer is independent. Star topology or full mesh.

## Commands

| Command | What it does |
|---------|-------------|
| `relay start <name>` | Initialize + print join command for the other machine |
| `relay join <name@user@host>` | Pair with a machine that ran `relay start` |
| `relay send <peer> <msg>` | Send a signed message |
| `relay monitor [filter]` | Stream incoming messages (for agent Monitor tools) |
| `relay peers` | List paired machines |
| `relay send-file <peer> <path>` | Send a file (arrives in `~/.agent-relay/files/`) |
| `relay logs [count]` | Show recent messages |
| `relay pubkey` | Print your public key |

## Requirements

- SSH access between machines (Tailscale makes this trivial)
- `openssl` (pre-installed on macOS/Linux)
- `python3` (for JSON handling)

## File Layout

```
~/.agent-relay/
├── config.json           # your name
├── identity/
│   ├── private.pem       # never leaves this machine
│   └── public.pem        # shared during pairing
├── peers/
│   └── peerName.json     # host + public key
└── messages.jsonl        # message log
```

## Security

- Ed25519 signatures on every message
- Private keys never leave the machine
- SSH transport (encrypted in transit)
- No central server, no cloud, no accounts

## License

MIT
