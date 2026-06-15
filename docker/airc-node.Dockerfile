# airc grid node — a single airc mesh node in a Linux container.
#
# Purpose: real-isolation test nodes for the mesh-convergence harness
# (each container = a separate "machine": own ~/.airc, own network
# namespace) AND the base image continuum grid-node images layer on top
# of. Per ZERO-FRICTION-PATH.md the END state is signed prebuilt binaries
# (no compiler in the user path); until that release pipeline exists this
# builds airc from source — the container is the dev/test/grid-sim path,
# not the user install.
#
# Convergence in the harness rides the gh account-registry (one shared
# token across nodes = one account = one mesh), so the image carries `gh`.
# The governor (the gh request counter) is the same one the harness reads
# to assert the footprint stays machine-bounded across N nodes.

# ---- build stage: compile the airc binary from the workspace ----
FROM rust:1-slim-bookworm AS build
WORKDIR /src
# System deps for the airc crates (ring/webrtc need a C toolchain + perl
# for openssl-style builds; pkg-config for native libs).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev cmake perl clang \
 && rm -rf /var/lib/apt/lists/*
# Copy the workspace and build only the CLI binary (the public `airc`).
COPY . .
RUN cargo build --release -p airc-cli \
 && cp "$(find target/release -maxdepth 1 -name airc -type f | head -1)" /airc

# ---- runtime stage: slim image with airc + gh ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates git curl gnupg procps \
 # GitHub CLI (the rendezvous/registry transport — the ONLY gh path,
 # governed by airc's request counter).
 && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      | gpg --dearmor -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update && apt-get install -y --no-install-recommends gh \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /airc /usr/local/bin/airc
# Card 7e3c9a1f: the node entrypoint runs `airc daemon` as the container's
# long-lived process and advertises a routable endpoint. Inlined via
# heredoc (the build context excludes docker/ — see .dockerignore — so a
# COPY from there can't see it; this keeps the entrypoint self-contained).
#   Bug 1: `airc join` returns without leaving a daemon, so nothing
#          advertises. The container must RUN the daemon — this is it.
#   Bug 2: detect_lan_ip() bails inside a container; AIRC_ADVERTISE_IP is
#          the explicit handoff of the routable endpoint (host launcher
#          passes the host LAN/Tailscale IP; same-host convergence falls
#          back to this container's own bridge IP, reachable by siblings).
COPY <<'EOF' /usr/local/bin/airc-node-run
#!/bin/sh
set -eu
if [ -z "${AIRC_ADVERTISE_IP:-}" ]; then
  AIRC_ADVERTISE_IP="$(hostname -i 2>/dev/null | awk '{print $1}')"
  export AIRC_ADVERTISE_IP
fi
echo "airc-node: advertising endpoint IP ${AIRC_ADVERTISE_IP:-<none>} (override via AIRC_ADVERTISE_IP)"
exec airc daemon
EOF
RUN chmod +x /usr/local/bin/airc-node-run

# Each node is its own "machine": a distinct HOME → distinct ~/.airc
# identity + local wire + governor state. The harness overrides HOME per
# replica so N containers are N separate machines on one account.
ENV HOME=/node
RUN useradd -m -d /node node && chown -R node:node /node
USER node
WORKDIR /node

# Default: run the daemon (Card 7e3c9a1f) as the container's long-lived
# process so the node advertises a routable endpoint and stays alive for
# the harness / operator to `docker exec airc-node airc join` + drive it.
# Override the CMD (e.g. `sleep infinity`) for a passive container.
CMD ["airc-node-run"]
