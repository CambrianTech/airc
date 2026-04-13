---
name: relay:setup
description: Set up Agent Relay — initialize this machine and pair with another.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "<peer@user@host>"
---

# Set Up Agent Relay

Do everything yourself — don't ask the user to run commands.

1. If `relay` is not on PATH, install it:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
   ```

2. If `$ARGUMENTS` contains an `@`, it's a join target:
   - If not already initialized, pick a short name from the local hostname and run `relay start <name>` (this will auto-detect it's a secondary and not block on the listener).
   - Run `relay join $ARGUMENTS` to pair via TCP key exchange.
   - Start the monitor, then send a test message:
   ```
   Monitor(persistent=true, command="relay monitor")
   ```
   ```bash
   relay send <peer> "connected"
   ```

3. If no arguments (or no `@` in arguments), this machine is the host:
   - Run `relay start <name>` — this prints the join command and waits for the joiner to connect via TCP key exchange (port 7547). No SSH needed for pairing.
   - Once pairing completes, start the monitor:
   ```
   Monitor(persistent=true, command="relay monitor")
   ```
   - Tell the user: "Give this to the other Claude:" followed by `/relay:setup <the join string from relay start output>`
