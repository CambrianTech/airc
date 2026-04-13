---
name: relay:setup
description: Set up Agent Relay — initialize this machine and pair with another.
user-invocable: true
allowed-tools: Bash
argument-hint: "[name] [peer@host]"
---

# Set Up Agent Relay

Walk the user through setup:

1. If `relay` is not on PATH, install it:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
   ```

2. If no arguments, ask for a name and run `relay start <name>`.
   Show the join command they need to run on the other machine.

3. If arguments include a peer@host, run `relay join <peer@host>` to pair.

4. After pairing, start the monitor:
   ```
   Monitor(persistent=true, command="relay monitor")
   ```

5. Send a test message to confirm it works.
