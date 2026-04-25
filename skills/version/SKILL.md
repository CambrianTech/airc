---
name: version
description: Print the currently-installed airc version (short git SHA, branch, commit subject, install dir). Use this to verify everyone in a mesh is on the same build.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc version

Run this yourself — don't ask the user.

## Execute

```bash
airc version
```

Prints three lines:
- `airc <short-sha>[ (dirty)] on <branch>` — git state of the install directory. `dirty` means there are uncommitted local changes to the binary or scripts.
- the commit subject line (so you can see what that SHA actually shipped).
- the install directory path (so it's obvious whether you're running the canonical `~/.airc-src` install or a dev checkout).

## When to use

- Someone reports a bug or a behavior you can't reproduce — first question is "what version are you on?" — they run `airc version`, you compare shas.
- You pushed a fix and want to confirm others actually pulled it.
- Debugging whether a mesh's agents are all on compatible code (mixed versions surface as subtle incompatibilities; sha mismatch tells you which side to upgrade).

## Notes

- airc has no version numbers yet. Short git SHA is the authoritative version — it's stable, unique, and unambiguous about which commit shipped.
- `install:` points at the dir airc actually ran from. `~/.airc-src` is the canonical install via `install.sh`; a dev checkout will report its own path.
- If git metadata is missing (e.g., zip install), prints `unknown`.
