# Checkout removal and retained branch outcomes

Card cbcf9529-ff7f-4b0f-8b29-42e151b440f9, base28917c11.

After supported adoption of AIRC1613, cleanup removed18 checkouts:10 branches
also retired, while8 were retained by Git branch -d with exact recovery refs.
One detached checkout cbaf84de was untouched. The old summary counted only10
completed and9failed, obscuring eight successful disk reclamations.

Root measured C: free87013601280 ->105315700736, net18302099456 bytes.
Read-only follow-up verified all eight branch tips equal their saved recovery
refs and the detached checkout still has a clean detached HEAD. Evidence:
C:/Users/joelt/.continuum/state/team-proof-20260921/cleanup-2157-apply.txt and
20261022-cleanup-retained-refs.json. This follow-up ran no cleanup or runtime mutation.

The existing shared retirement owner now returns a typed removed-checkout
outcome containing an optional retained-branch diagnostic. Both callers preserve
partial-attention semantics; manual output counts removed checkouts, retained
branches and failed checkout operations separately and remains nonzero for partial
retirement. The same branch probe classifies detached terminal checkouts as
SkipDetached and refuses them again at mutation. Captured branch/HEAD are rechecked
before submodule release. No force-delete, recovery-ref removal or relaxed
history checks were introduced.

Existing squash fixture asserts the partial result and retained original history;
a real detached fixture checks classification and absence of mutation. Formatting
and diff checks pass. Compilation/tests/Clippy await CI to preserve the serving
machine's ownership. Independent review requested; installed follow-up pending.