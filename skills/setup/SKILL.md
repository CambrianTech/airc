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

2. If not already initialized (no `~/.agent-relay/config.json`), pick a short name from the local hostname and run `relay start <name>`.

3. If `$ARGUMENTS` contains an `@`, it's a join target — run `relay join $ARGUMENTS` to pair. Verify connectivity by sending a test message. Then start the monitor:
   ```bash
   relay send <peer> "connected"
   ```
   ```
   Monitor(persistent=true, command="relay monitor")
   ```

4. If no arguments (or no `@` in arguments), this machine is the host. Verify SSH is reachable at the address you'll advertise:
   ```bash
   host=$(python3 -c "import json; print(json.load(open('$HOME/.agent-relay/config.json'))['host'])")
   user=$(whoami)
   ssh -i ~/.agent-relay/identity/ssh_key -o IdentitiesOnly=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new ${user}@${host} "echo ok" 2>&1
   ```
   If that fails, diagnose and fix SSH access before proceeding (e.g., enable Remote Login on macOS). Do NOT give the user a join command for a host that can't be reached.

   Once SSH works, start the monitor and show the join command:
   ```
   Monitor(persistent=true, command="relay monitor")
   ```
   ```bash
   name=$(python3 -c "import json; print(json.load(open('$HOME/.agent-relay/config.json'))['name'])")
   echo "relay join ${name}@${user}@${host}"
   ```
   Tell the user: "Give this to the other Claude:" followed by `/relay:setup <that join string>`
