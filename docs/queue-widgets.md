# AIRC Queue Widgets

AIRC queue widgets are static, read-only views over GitHub issues labeled
`airc-queue`. They do not create a second source of truth: the issue body JSON
envelope remains canonical.

## Queue Board

Embed the board on any static page:

```html
<link rel="stylesheet" href="https://cdn.example/airc/widgets/queue-board.css">
<script src="https://cdn.example/airc/widgets/queue-board.js" defer></script>

<airc-queue-board repo="CambrianTech/airc" limit="50"></airc-queue-board>
```

For a local checkout, open `widgets/queue-board.html`.

The widget fetches:

```text
https://api.github.com/repos/<owner>/<repo>/issues?state=open&labels=airc-queue
```

It renders the queue cards into status columns:

- `claimed`
- `in-progress`
- `blocked`
- `review`
- `merged`

Displayed fields come from the queue envelope:

- issue number, title, and URL
- `id`
- `branch`
- `owner`
- `env`
- `next_action`
- `last_heartbeat`

## Constraints

- Static hosting compatible; GitHub Pages can serve the files directly.
- Public pages can only show public repos or issues visible to the browser.
- No tokens are embedded. Private queues need a separate authenticated surface.
- Mutations stay in AIRC/GitHub flows: `airc queue claim`, `release`,
  `set-status`, `heartbeat`, and PR close automation.

## Parser Helpers

`widgets/queue-board.js` also exposes helper functions for other portals:

```js
const {
  parseQueueCard,
  normalizeIssue,
  groupCards,
  renderQueueBoard
} = window.AircQueueBoard;
```

Node-based tests can import the same file with `require()`.

## Room Directory

The room directory widget renders public or approved room metadata from a JSON
config. It intentionally does not discover private rooms or publish gist ids.

```html
<link rel="stylesheet" href="./queue-board.css">
<script src="./room-directory.js" defer></script>

<airc-room-directory src="./rooms.json"></airc-room-directory>
```

`rooms.json`:

```json
{
  "title": "Project Rooms",
  "rooms": [
    {
      "name": "#cambriantech",
      "scope": "CambrianTech",
      "description": "Project coordination room.",
      "visibility": "approved",
      "joinHint": "airc join"
    }
  ]
}
```

Inline config is also supported:

```html
<airc-room-directory>
  <script type="application/json">
    { "rooms": [{ "name": "#general", "joinHint": "airc join --room general" }] }
  </script>
</airc-room-directory>
```
