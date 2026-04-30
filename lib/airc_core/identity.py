"""Identity bootstrap — generate / read the local peer's X25519 keypair.

A scope's `identity/` directory holds three keypairs after Phase E.2:
  ssh_key      / ssh_key.pub      — Ed25519 SSH key (auth to remote sshd)
  private.pem  / public.pem       — Ed25519 envelope-signing key
  x25519_priv  / x25519_pub       — X25519 ECDH key (Phase E.2, this module)

The first two pre-date this module. This module owns the third. They
serve different purposes:
  * ssh_key signs SSH-protocol challenges (for sshd auth)
  * private.pem signs envelope `sig` field (origin proof)
  * x25519_priv participates in ECDH for envelope-body encryption

We DON'T derive X25519 from Ed25519 (the curve birational map is
standard but conflating signing + key-agreement in one keypair is
discouraged in modern crypto practice — separate keys cleanly limit
the blast radius if one is compromised).

The bootstrap is idempotent: if the X25519 keypair files already
exist, we leave them alone. First-call generates fresh keys via the
crypto module; subsequent calls just load.
"""

from __future__ import annotations

import os
from typing import Optional


X25519_PRIV_FILENAME = "x25519_priv"
X25519_PUB_FILENAME = "x25519_pub"

# Ed25519 envelope-signing keypair lives as PEM files at fixed names —
# matching the on-disk shape that prior shell-openssl init wrote, so a
# scope upgraded across the cryptography-migration boundary doesn't see
# its identity rotate. Different file names from X25519 (which uses raw
# bytes) because the openssl-created private.pem / public.pem pre-date
# this module and we keep them.
ED25519_PRIV_FILENAME = "private.pem"
ED25519_PUB_FILENAME = "public.pem"


def x25519_paths(identity_dir: str) -> tuple[str, str]:
    """Return (priv_path, pub_path) for an identity directory."""
    return (
        os.path.join(identity_dir, X25519_PRIV_FILENAME),
        os.path.join(identity_dir, X25519_PUB_FILENAME),
    )


def has_x25519_keypair(identity_dir: str) -> bool:
    """True if both files exist. Used by callers that want to do
    encryption-conditional logic without raising on missing keys."""
    priv_path, pub_path = x25519_paths(identity_dir)
    return os.path.isfile(priv_path) and os.path.isfile(pub_path)


def bootstrap(identity_dir: str) -> tuple[bytes, bytes]:
    """Idempotent: generate the X25519 keypair if missing, return raw bytes.

    Returns (priv_raw, pub_raw), 32 bytes each. On subsequent calls
    just reads the existing files. This is the single entry point
    init_identity() (in the airc bash) calls during scope setup.

    Raises ImportError if the cryptography package isn't installed,
    so callers can detect a no-crypto environment and either fall back
    to plaintext-only operation or surface a setup error. The
    `cryptography_available()` function below is the cheap check
    callers should use to gate features without forcing the import
    cost upfront.
    """
    from . import crypto  # raises ImportError if cryptography is missing

    priv_path, pub_path = x25519_paths(identity_dir)
    if has_x25519_keypair(identity_dir):
        return (crypto.load_priv(priv_path), crypto.load_pub(pub_path))
    os.makedirs(identity_dir, exist_ok=True)
    priv, pub = crypto.generate_x25519_keypair()
    crypto.save_keypair(priv, pub, priv_path, pub_path)
    return (priv, pub)


# ── Ed25519 envelope-signing keypair (issue #341 migration) ────────
#
# Pre-#341 init_identity() shelled out to `openssl genpkey -algorithm
# Ed25519`. macOS LibreSSL doesn't implement Ed25519, which silently
# broke fresh-Mac installs (the `2>/dev/null` ate the error). The
# cryptography venv module already supports Ed25519 natively and is
# already an install.sh prereq for envelope encryption — so the shell
# openssl is a redundant footgun. These functions are the migration
# target.

def ed25519_paths(identity_dir: str) -> tuple[str, str]:
    """Return (priv_path, pub_path) for an identity directory."""
    return (
        os.path.join(identity_dir, ED25519_PRIV_FILENAME),
        os.path.join(identity_dir, ED25519_PUB_FILENAME),
    )


def has_ed25519_keypair(identity_dir: str) -> bool:
    priv_path, pub_path = ed25519_paths(identity_dir)
    return os.path.isfile(priv_path) and os.path.isfile(pub_path)


def bootstrap_ed25519(identity_dir: str) -> None:
    """Idempotent: generate the Ed25519 envelope-signing keypair if
    missing. Writes private.pem (PKCS#8 PEM, 0600) + public.pem (SPKI
    PEM, 0644) — same on-disk format the prior `openssl genpkey` path
    produced, so an upgraded scope's identity is unchanged.

    Raises ImportError if cryptography is unavailable. Callers higher
    up (`airc init_identity` bash) treat that as a hard fatal: the
    envelope signing key is required, not optional.
    """
    from . import crypto

    priv_path, pub_path = ed25519_paths(identity_dir)
    if has_ed25519_keypair(identity_dir):
        return
    os.makedirs(identity_dir, exist_ok=True)
    priv_pem, pub_pem = crypto.generate_ed25519_keypair_pem()
    crypto.save_ed25519_keypair_pem(priv_pem, pub_pem, priv_path, pub_path)


def sign_ed25519(identity_dir: str, data: bytes) -> bytes:
    """Sign `data` with the scope's Ed25519 private key. Raw 64-byte
    signature output — same as `openssl pkeyutl -sign` produces. Caller
    base64s for invite-string / envelope embedding.
    """
    from . import crypto

    priv_path, _ = ed25519_paths(identity_dir)
    return crypto.sign_ed25519_pem(priv_path, data)


def cryptography_available() -> bool:
    """Cheap check for the cryptography package's presence. Used by
    higher-layer callers (cmd_send, monitor_formatter) to gate the
    encrypt/decrypt path. Returns False if the package isn't installed
    OR the import itself fails for any reason — caller treats either
    case as "fall back to plaintext."

    Implementation note: import attempt is cheap (Python caches
    successful imports). For the failure path we don't want a stack
    trace bubble up; we just want True/False so the gate is binary.
    """
    try:
        from . import crypto  # noqa: F401
        return True
    except ImportError:
        return False


def load_priv(identity_dir: str) -> Optional[bytes]:
    """Read X25519 private key. Returns None if missing or cryptography
    isn't installed. Used when the caller wants "best effort" — try to
    decrypt, fall back to plaintext display if we can't."""
    if not has_x25519_keypair(identity_dir):
        return None
    if not cryptography_available():
        return None
    from . import crypto
    priv_path, _ = x25519_paths(identity_dir)
    try:
        return crypto.load_priv(priv_path)
    except (OSError, ValueError):
        return None


def load_pub(identity_dir: str) -> Optional[bytes]:
    """Read X25519 public key. Returns None if missing."""
    if not has_x25519_keypair(identity_dir):
        return None
    if not cryptography_available():
        return None
    from . import crypto
    _, pub_path = x25519_paths(identity_dir)
    try:
        return crypto.load_pub(pub_path)
    except (OSError, ValueError):
        return None


# ── Peer pubkey storage in peer records ────────────────────────────

def peer_x25519_pub(peers_dir: str, peer_name: str) -> Optional[bytes]:
    """Look up a peer's X25519 public key from peers/<name>.json.

    The pubkey is stored base64-url-encoded in the peer record under
    'x25519_pub'. Returns None if the peer record doesn't exist, has no
    pubkey, or the encoded value is malformed.

    Why base64 in JSON: peer records are small JSON files read/written
    by both bash and Python; raw bytes don't round-trip cleanly through
    JSON. URL-safe base64 (no padding) is the convention used elsewhere
    in airc for binary-in-JSON.
    """
    if not cryptography_available():
        return None
    import json
    path = os.path.join(peers_dir, peer_name + ".json")
    try:
        with open(path) as f:
            d = json.load(f)
    except (OSError, ValueError):
        return None
    encoded = d.get("x25519_pub")
    if not encoded or not isinstance(encoded, str):
        return None
    from . import crypto
    try:
        raw = crypto.b64decode(encoded)
    except ValueError:
        return None
    if len(raw) != 32:
        return None
    return raw


def store_peer_x25519_pub(peers_dir: str, peer_name: str, pub_raw: bytes) -> bool:
    """Write peer's X25519 pubkey into their peer record. Reads the
    existing record, adds the pubkey field, writes atomically.

    Returns True on success, False if the peer record is missing or
    unwritable. Errors are not raised because callers are usually in
    the middle of a pair handshake and want to continue regardless of
    storage success.
    """
    if not cryptography_available():
        return False
    import json
    if len(pub_raw) != 32:
        return False
    path = os.path.join(peers_dir, peer_name + ".json")
    try:
        with open(path) as f:
            d = json.load(f)
    except (OSError, ValueError):
        return False
    from . import crypto
    d["x25519_pub"] = crypto.b64encode(pub_raw)
    try:
        # Atomic via temp + replace.
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(d, f, indent=2)
        os.replace(tmp, path)
        return True
    except OSError:
        try:
            os.unlink(path + ".tmp")
        except OSError:
            pass
        return False


# ── CLI entry — bash invokes this during init_identity ─────────────

def _cli() -> int:
    """Bash-callable entry: `python -m airc_core.identity bootstrap --dir <path>`.

    Idempotent. Prints the resulting public key (base64) on stdout so
    bash can capture it for inclusion in pair-handshake metadata.
    Exit 0 on success, 1 on failure (cryptography missing, IO error,
    etc).

    Why a CLI entry rather than direct python: bash needs to run this
    during `init_identity()` and capture the pubkey for handshake
    parameters. A subprocess + stdout capture is the natural seam.
    """
    import argparse
    import sys

    parser = argparse.ArgumentParser(prog="airc_core.identity")
    sub = parser.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("bootstrap", help="Generate X25519 keypair if missing; print pubkey")
    b.add_argument("--dir", required=True, help="Identity directory path")
    p = sub.add_parser("get_pub", help="Print existing X25519 pubkey (b64); fails if absent")
    p.add_argument("--dir", required=True)
    pp = sub.add_parser(
        "peer_pub",
        help="Print stored peer's X25519 pubkey (b64); empty stdout if absent",
    )
    pp.add_argument("--peers-dir", required=True)
    pp.add_argument("--peer-name", required=True)
    # Ed25519 envelope-signing key — replaces the prior shell openssl
    # path. bootstrap_ed25519 is invoked from airc's init_identity;
    # sign_ed25519 from sign_message. See module docstring (Ed25519
    # block above) for migration context.
    be = sub.add_parser(
        "bootstrap-ed25519",
        help="Generate Ed25519 envelope-signing keypair if missing",
    )
    be.add_argument("--dir", required=True, help="Identity directory path")
    se = sub.add_parser(
        "sign-ed25519",
        help="Sign stdin bytes; print base64 signature on stdout",
    )
    se.add_argument("--dir", required=True, help="Identity directory path")
    args = parser.parse_args()

    if args.cmd == "bootstrap":
        if not cryptography_available():
            print(
                "cryptography package not available; install via "
                "`<airc-dir>/.venv/bin/pip install cryptography` or run install.sh",
                file=sys.stderr,
            )
            return 1
        try:
            _, pub = bootstrap(args.dir)
        except OSError as e:
            print(f"identity bootstrap failed: {e}", file=sys.stderr)
            return 1
        from . import crypto
        print(crypto.b64encode(pub))
        return 0

    if args.cmd == "get_pub":
        pub = load_pub(args.dir)
        if pub is None:
            print("no x25519 pubkey found", file=sys.stderr)
            return 1
        from . import crypto
        print(crypto.b64encode(pub))
        return 0

    if args.cmd == "peer_pub":
        # Used by cmd_send to look up recipient's pubkey for envelope
        # encryption. Empty stdout = peer has no stored pubkey OR
        # cryptography isn't installed (in either case caller falls
        # back to plaintext send). Exit always 0 to keep bash happy.
        if not cryptography_available():
            return 0
        pub = peer_x25519_pub(args.peers_dir, args.peer_name)
        if pub is None:
            return 0
        from . import crypto
        print(crypto.b64encode(pub))
        return 0

    if args.cmd == "bootstrap-ed25519":
        # Hard-fatal if cryptography missing — Ed25519 signing is required
        # for the envelope path, not optional. install.sh's prereq check
        # ensures the venv has cryptography; if we got here without it,
        # the install is broken.
        if not cryptography_available():
            print(
                "cryptography package not available; airc envelope signing "
                "requires it. Re-run install.sh to set up the venv.",
                file=sys.stderr,
            )
            return 1
        try:
            bootstrap_ed25519(args.dir)
        except OSError as e:
            print(f"ed25519 bootstrap failed: {e}", file=sys.stderr)
            return 1
        return 0

    if args.cmd == "sign-ed25519":
        # Reads message bytes from stdin (binary-safe; bash pipes message
        # data via `printf '%s' "$msg" | python -m airc_core.identity
        # sign-ed25519 ...`). Prints b64 signature on stdout, matching
        # the prior `openssl pkeyutl -sign | base64` output shape so
        # callers don't need to change.
        if not cryptography_available():
            print("cryptography missing", file=sys.stderr)
            return 1
        try:
            data = sys.stdin.buffer.read()
            sig = sign_ed25519(args.dir, data)
        except (OSError, ValueError) as e:
            print(f"ed25519 sign failed: {e}", file=sys.stderr)
            return 1
        # Standard base64 (with padding, newline-terminated) — matches
        # `openssl pkeyutl -sign | base64` byte-for-byte. Don't use the
        # urlsafe variant `crypto.b64encode` here; bash callers expect
        # the openssl-compatible format.
        import base64
        sys.stdout.write(base64.b64encode(sig).decode("ascii") + "\n")
        return 0

    return 1


if __name__ == "__main__":
    import sys
    sys.exit(_cli())
