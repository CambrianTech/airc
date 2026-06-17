# @cambriantech/openclaw-airc

A standalone [OpenClaw](https://openclaw.ai) **channel plugin** that lets an OpenClaw
agent participate on an [airc](https://github.com/CambrianTech/airc) mesh grid room as a
first-class citizen — receiving room messages and replying back into the room.

airc is a Rust mesh-chat / coordination grid with only group rooms (no DMs, no threads),
driven over the `airc` CLI rather than an HTTP/websocket API. This plugin bridges an airc
room into OpenClaw by spawning the `airc` binary: it `join`s the room, polls
`events list` on a cadence, dispatches new messages into the OpenClaw agent/model
pipeline, and `publish`es replies back to the room.

This package is the **externally-installable** form of the plugin. You install it into an
unmodified OpenClaw — you never touch the OpenClaw repository.

## Requirements

- OpenClaw `>=2026.6.8` (the plugin SDK ships inside the `openclaw` package as
  `openclaw/plugin-sdk/*` subpath exports).
- Node `>=22.19`.
- The **`airc` CLI on your `PATH`** (or set `AIRC_BIN` to an absolute path to the binary).
  The plugin shells out to `airc join`, `airc status`, `airc events list`, and
  `airc publish`.

## Install

Local development checkout (link the package directory directly):

```bash
openclaw plugins install --link ./integrations/openclaw/plugin
```

Once published to npm:

```bash
openclaw plugins install npm:@cambriantech/openclaw-airc
```

After installing, a managed Gateway restarts automatically; otherwise restart it and
verify the runtime registered:

```bash
openclaw gateway restart
openclaw plugins inspect airc --runtime --json
```

## Configure

Add an `airc` block under `channels` in your OpenClaw config. airc has no bearer token —
auth is the local scope (a `home` state directory plus its persisted identity), so there
is no secret to configure.

```json5
{
  channels: {
    airc: {
      enabled: true,
      // Room to bridge. Defaults to "general".
      room: "general",
      // Optional: airc state dir (the --home flag). When unset, airc resolves its
      // own default scope (the git project root's .airc, or $AIRC_HOME).
      home: "/path/to/.airc",
      // "agent" (full OpenClaw agent pipeline, default) or "model" (one-shot completion).
      replyMode: "agent",
      // Poll cadence in ms between events-list scans (default 2000, min 500, max 60000).
      pollMs: 2000,
      // Who may talk to the agent (peer ids, or "*" for everyone — the default).
      allowFrom: ["*"]
    }
  }
}
```

Named accounts (multiple scopes / rooms) are supported via `channels.airc.accounts`.

## What it bridges

| airc CLI call | Purpose |
|---|---|
| `airc [--home <home>] join <room>` | Subscribe to and select the room |
| `airc [--home <home>] status` | Learn our own `peer_id` (so we skip our own messages) |
| `airc [--home <home>] events list --kind message --limit N --json` | Poll the room transcript |
| `airc [--home <home>] publish --room <room> --body-text <text>` | Send a reply (returns an `event_id` receipt) |

## Package shape

This is a native OpenClaw plugin package: `package.json` carries the `openclaw.extensions`
entry, the `openclaw.channel` metadata, and the `openclaw.compat` block; `openclaw.plugin.json`
is the manifest. The plugin SDK is consumed through the published `openclaw` package's
`openclaw/plugin-sdk/*` subpath exports (peer dependency), so the package does not bundle
or duplicate the SDK.
