# airc node image for the grid-resiliency harness.
#
# A minimal, real airc daemon node — the unit the resiliency scenarios spin
# up N of, on controllable Docker networks, to test the grid forming,
# delivering, and self-healing (partition, node death, relay re-election)
# the way it must in real life.
#
# Build context is the airc repo root (so the workspace is available):
#   docker build -f integrations/grid-resiliency/Dockerfile -t airc-node:test .
# CI builds airc on stock ubuntu with only the Rust toolchain, so the
# official rust image (which ships a C toolchain) needs no extra apt deps.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# Release build of just the CLI — the `airc` binary is the whole node.
RUN cargo build --release -p airc-cli

FROM debian:bookworm-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/airc /usr/local/bin/airc
# Each node owns its own home (identity + ORM + sockets) under /data, so
# containers never share mesh state.
ENV AIRC_HOME=/data/.airc
RUN useradd --create-home --uid 10001 airc && mkdir -p /data && chown -R airc /data
USER airc
WORKDIR /data
# The scenario runner drives lifecycle (init / enrol / send / read) via
# `docker exec`; the container's job is just to BE a reachable node. The
# entrypoint is the airc binary so `docker run airc-node <verb>` works and
# the scenario can override the command per node.
ENTRYPOINT ["airc"]
CMD ["--help"]
