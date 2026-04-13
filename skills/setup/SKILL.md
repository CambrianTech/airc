---
name: relay:setup
description: Set up Agent Relay — initialize this machine and pair with another.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "<peer@user@host#key>"
---

# Set Up Agent Relay

Do everything yourself — don't ask the user to run commands.

1. If `relay` is not on PATH, install it:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
   ```

2. If `$ARGUMENTS` contains an `@`, it's a join target. Pass the ENTIRE argument including the `#key` portion — that's the host's SSH public key:
   ```bash
   relay join $ARGUMENTS
   ```
   This initializes if needed, authorizes the host's key, and exchanges keys via SSH.
   - If join fails with SSH errors, **diagnose and fix it yourself**:
     - Check if sshd is running: `pgrep -x sshd` or `launchctl list | grep ssh`
     - If not running, enable it:
       - macOS: `sudo systemsetup -setremotelogin on` or `sudo launchctl load -w /System/Library/LaunchDaemons/ssh.plist`
       - Linux: `sudo systemctl start sshd` or `sudo service ssh start`
     - Check if port 22 is reachable on the host: `nc -z <host-ip> 22`
     - Check if your SSH key is authorized: look at `~/.ssh/authorized_keys`
     - Fix whatever is broken and retry `relay join`.
   - Once paired, start the monitor and send a test message:
   ```
   Monitor(persistent=true, command="relay monitor")
   ```
   ```bash
   relay send <peer> "connected"
   ```

3. If no arguments (or no `@` in arguments), this machine is the host:
   - Run `relay start <name>` — picks a name from local hostname if not specified.
   - Start the monitor:
   ```
   Monitor(persistent=true, command="relay monitor")
   ```
   - Show the join string from `relay start` output. Tell the user: "Give this to the other Claude:" followed by `/relay:setup <the join string>`
