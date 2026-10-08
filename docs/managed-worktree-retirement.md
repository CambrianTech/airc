# Managed worktree retirement

`airc work cleanup` previews managed card worktrees. `--force` applies the
preview candidates, checking their files again before each removal. Card close
and merge completion use the same retirement operation.

Only a known terminal card is eligible. A missing remote branch does not prove
completion and cannot override an active or unknown card. Dirty, unpushed,
unclassified, ignored-file, primary, locked and initialized-submodule checkouts
are retained. Cleanup does not fetch, delete remote branches, or change unrelated
local branches.

Before removing a clean managed checkout, AIRC saves its exact commit at
`refs/airc/retired/<branch>/<commit>`. It then removes the checkout without forcing
Git and retires its local branch using the observed commit as a compare-and-delete
condition. These recovery refs preserve history without cluttering `git branch`.

Inspect and restore a retained commit:

```sh
git for-each-ref refs/airc/retired/ --format='%(refname) %(objectname)'
git branch recovered-work refs/airc/retired/BRANCH/COMMIT
```

A squash-equivalent patch can pass the existing integration-ref proof even when
its original tracking ref is gone. Ambiguous multi-commit squash histories stay
retained; deleting an upstream alone never proves them disposable. Standalone
branches whose worktrees were removed by older versions are not automatically
attributed to a card or swept. Recovery refs have no automatic expiration.
