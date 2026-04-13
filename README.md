# Claude Relay

Secure real-time messaging between Claude Code instances on different machines.

## 30-Second Setup

**Machine A:**
```bash
git clone https://github.com/CambrianTech/claude-relay && cd claude-relay
./install.sh
relay start myname
```

It prints one line. Copy it.

**Machine B:**
```bash
git clone https://github.com/CambrianTech/claude-relay && cd claude-relay
./install.sh
relay join myname@machineA.tail1234.ts.net
```

Done. Both machines are paired and talking.

## Sending Messages

```bash
relay send peerName "hello from this machine"
```

## What Claude Code Does Automatically

Once paired, Claude Code starts a background monitor. When the other Claude sends a message, yours gets notified inline — no polling, no checking. To send:

```bash
relay send peerName "your message"
```

## How Pairing Works

1. `relay start` generates an Ed25519 keypair and listens for a join request
2. `relay join` connects via SSH, both machines exchange public keys automatically
3. Future messages are signed with your private key and verified with the peer's public key
4. Transport is SSH over whatever network you have (Tailscale, LAN, VPN, internet)

No passwords. No tokens. No accounts. Just SSH + public key crypto.

## Requirements

- SSH access between machines (Tailscale makes this trivial)
- `openssl` (pre-installed on macOS/Linux)
- `python3` (for JSON handling)

## Commands

| Command | What it does |
|---------|-------------|
| `relay start <name>` | Initialize + print join command for the other machine |
| `relay join <name@host>` | Pair with a machine that ran `relay start` |
| `relay send <peer> <msg>` | Send a signed message |
| `relay monitor` | Stream incoming messages (used by Claude Code Monitor tool) |
| `relay peers` | List paired machines |
| `relay pubkey` | Print your public key |
| `relay logs` | Show recent messages |

## File Layout

```
~/.claude-relay/
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
- Peer public keys verified during pairing

## License

MIT
