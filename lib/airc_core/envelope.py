"""Envelope wrap/unwrap — the read/write seam for E2E-encrypted msg fields.

The on-the-wire envelope shape after Phase E.3:
    {
      "from":    "alpha",
      "to":      "bob"   | "all",
      "ts":      "2026-04-29T01:23:45Z",
      "channel": "general",
      "msg":     "<plaintext>" | "<base64-ciphertext>",
      "sig":     "...",                    # Ed25519 signature (existing)
      "enc":     "v1"          [optional], # marks msg as ciphertext
      "nonce":   "<base64>"    [optional]  # AEAD nonce, only when enc set
    }

When `enc` is absent, the envelope is plaintext (current pre-Phase-E
shape; preserved for backward compat with peers running older airc).
When `enc` is present, msg is the AEAD ciphertext and nonce is the
12-byte ChaCha20-Poly1305 nonce.

Associated data binding: AEAD AD includes the plaintext envelope
metadata (from, to, ts, channel) so a tampered metadata field
invalidates auth and the receiver drops the message. This is what
prevents "alpha→bob" envelopes from being silently rerouted as
"alpha→carol" by anyone (including the bearer).

Plaintext compat policy:
  - If recipient has no stored x25519_pub: fall back to plaintext send.
  - If sender has no x25519_priv (cryptography not installed): plaintext.
  - If envelope arrives with enc set but our recipient pubkey lookup
    fails: log + drop (vs displaying ciphertext; the latter would
    leak structure to the user as garbage text).

This is the layer above the bearer. The bearer carries opaque bytes;
this module reads/writes those bytes' shape.
"""

from __future__ import annotations

import json
import os
from typing import Any, Optional


# Wire format version — bump only on breaking changes.
ENC_VERSION = "v1"

# HKDF info string for envelope-body AEAD keys. Domain-separated from
# any future purpose (e.g. file-transfer AEAD keys would use a different
# info string so the same X25519 pair can't be misused across purposes).
AEAD_INFO = b"airc-aead-v1"


def encrypt_msg(
    msg: str,
    sender_x25519_priv: bytes,
    recipient_x25519_pub: bytes,
    metadata_for_ad: dict,
) -> tuple[str, str]:
    """Encrypt `msg` with AEAD bound to envelope metadata.

    Returns (ciphertext_b64, nonce_b64). Caller embeds these in the
    envelope as msg + nonce, sets enc=ENC_VERSION.

    The associated_data is a stable serialization of the plaintext
    envelope fields (sender, recipient, ts, channel) so the recipient
    can verify nothing was tampered. Stable means: same keys + same
    values produce the same byte sequence on every machine. We use
    sorted-key JSON for that.
    """
    from . import crypto
    key = crypto.derive_pairwise_key(
        sender_x25519_priv, recipient_x25519_pub, info=AEAD_INFO
    )
    ad = _serialize_ad(metadata_for_ad)
    nonce, ct = crypto.aead_encrypt(key, msg.encode("utf-8"), associated_data=ad)
    return (crypto.b64encode(ct), crypto.b64encode(nonce))


def decrypt_msg(
    ciphertext_b64: str,
    nonce_b64: str,
    recipient_x25519_priv: bytes,
    sender_x25519_pub: bytes,
    metadata_for_ad: dict,
) -> Optional[str]:
    """Decrypt an envelope's ciphertext. Returns plaintext str, or None
    if AEAD auth fails / inputs are malformed.

    Caller decides what to do on None — typically log a warning and
    drop the message rather than displaying garbage text. The drop is
    silent at the formatter level (per the existing "drop malformed
    envelopes" policy).
    """
    from . import crypto
    try:
        ct = crypto.b64decode(ciphertext_b64)
        nonce = crypto.b64decode(nonce_b64)
    except (ValueError, TypeError):
        return None
    if len(nonce) != 12:
        return None
    key = crypto.derive_pairwise_key(
        recipient_x25519_priv, sender_x25519_pub, info=AEAD_INFO
    )
    ad = _serialize_ad(metadata_for_ad)
    try:
        plaintext = crypto.aead_decrypt(key, nonce, ct, associated_data=ad)
    except Exception:
        return None
    try:
        return plaintext.decode("utf-8")
    except UnicodeDecodeError:
        return None


def wrap_envelope(
    envelope: dict,
    sender_x25519_priv: bytes,
    recipient_x25519_pub: bytes,
) -> dict:
    """Wrap an outgoing envelope: encrypt msg, set enc + nonce fields.
    Returns a new dict (input is not mutated). The output retains all
    the envelope's plaintext metadata (from, to, ts, channel, sig if
    present) — only msg is replaced and enc/nonce are added.
    """
    out = dict(envelope)
    msg_plain = out.get("msg", "")
    if not isinstance(msg_plain, str):
        msg_plain = str(msg_plain)
    metadata = _ad_fields(out)
    ct_b64, nonce_b64 = encrypt_msg(
        msg_plain, sender_x25519_priv, recipient_x25519_pub, metadata,
    )
    out["msg"] = ct_b64
    out["nonce"] = nonce_b64
    out["enc"] = ENC_VERSION
    return out


def unwrap_envelope(
    envelope: dict,
    recipient_x25519_priv: bytes,
    sender_x25519_pub: bytes,
) -> Optional[dict]:
    """Unwrap an incoming envelope: decrypt msg, drop enc + nonce fields.

    Returns a new dict with msg replaced by plaintext, or None on
    decryption failure / version mismatch / unknown format.
    """
    enc = envelope.get("enc")
    if enc != ENC_VERSION:
        return None  # unknown version; caller decides whether to drop or pass-through
    ct_b64 = envelope.get("msg", "")
    nonce_b64 = envelope.get("nonce", "")
    if not isinstance(ct_b64, str) or not isinstance(nonce_b64, str):
        return None
    metadata = _ad_fields(envelope)
    plaintext = decrypt_msg(
        ct_b64, nonce_b64, recipient_x25519_priv, sender_x25519_pub, metadata,
    )
    if plaintext is None:
        return None
    out = dict(envelope)
    out["msg"] = plaintext
    out.pop("enc", None)
    out.pop("nonce", None)
    return out


def is_encrypted(envelope: dict) -> bool:
    """Cheap predicate: does this envelope have the enc marker? Used
    by monitor_formatter to gate the decrypt path without unconditionally
    importing crypto."""
    return envelope.get("enc") == ENC_VERSION


# ── Internal: associated-data binding ──────────────────────────────

def _ad_fields(envelope: dict) -> dict:
    """Subset of envelope fields that are bound by AEAD's associated
    data. Anyone tampering with these on the wire invalidates the
    authentication tag and the receiver drops the message.

    We bind: from, to, ts, channel — everything that the bearer or
    a transit attacker might be tempted to swap. We do NOT bind sig
    because sig is computed AFTER encryption (sig signs the ciphertext +
    metadata together) and binding it inside the AD would create a
    chicken-and-egg.
    """
    return {
        "from": envelope.get("from", ""),
        "to": envelope.get("to", ""),
        "ts": envelope.get("ts", ""),
        "channel": envelope.get("channel", ""),
    }


def _serialize_ad(d: dict) -> bytes:
    """Stable serialization for AEAD associated data. sort_keys + no
    whitespace produces the same byte sequence on both sides of a
    pair regardless of dict insertion order or platform JSON quirks.
    """
    return json.dumps(d, sort_keys=True, separators=(",", ":")).encode("utf-8")


# ── CLI entry — bash invokes this from cmd_send to wrap envelopes ──

def _cli() -> int:
    """Bash-callable: wrap (encrypt) or unwrap (decrypt) an envelope.

    Usage:
      echo '<envelope-json>' | python -m airc_core.envelope wrap \
          --recipient-pub <b64> --identity-dir <path>
      echo '<envelope-json>' | python -m airc_core.envelope unwrap \
          --sender-pub <b64> --identity-dir <path>

    Wrap: reads envelope JSON from stdin, encrypts msg field with sender's
    private key + recipient's public key, prints the wrapped envelope to
    stdout. Sets enc='v1' and nonce.

    Unwrap: reverses, prints the plaintext envelope. Returns nonzero on
    auth failure (caller can decide to log + drop).

    On any error (cryptography missing, malformed input), prints the
    INPUT envelope unchanged to stdout and exits 0. This is the
    plaintext-fallback path: a peer whose recipient lacks a stored
    x25519_pub gets called with --recipient-pub "" and we pass through.
    Same for receivers without crypto.
    """
    import argparse
    import sys

    parser = argparse.ArgumentParser(prog="airc_core.envelope")
    sub = parser.add_subparsers(dest="cmd", required=True)

    w = sub.add_parser("wrap")
    w.add_argument("--recipient-pub", default="",
                   help="Recipient's X25519 pubkey (b64). Empty = pass-through plaintext.")
    w.add_argument("--identity-dir", required=True,
                   help="Sender's identity directory (for X25519 priv key)")

    u = sub.add_parser("unwrap")
    u.add_argument("--sender-pub", default="",
                   help="Sender's X25519 pubkey (b64). Empty = drop (can't decrypt).")
    u.add_argument("--identity-dir", required=True,
                   help="Recipient's identity directory (for X25519 priv key)")

    args = parser.parse_args()

    raw = sys.stdin.read().strip()
    if not raw:
        return 0
    try:
        env = json.loads(raw)
    except (ValueError, TypeError):
        # Malformed input — echo back so caller's queue/log doesn't lose it.
        sys.stdout.write(raw + "\n")
        return 0

    # Plaintext-fallback: missing recipient pubkey or cryptography
    # unavailable → pass through unchanged.
    try:
        from . import identity as _identity
        from . import crypto as _crypto
    except ImportError:
        sys.stdout.write(json.dumps(env) + "\n")
        return 0

    if args.cmd == "wrap":
        if not args.recipient_pub:
            # No recipient pubkey stored → plaintext send (peer is on
            # pre-Phase-E airc, transparent fallback).
            sys.stdout.write(json.dumps(env) + "\n")
            return 0
        my_priv = _identity.load_priv(args.identity_dir)
        if my_priv is None:
            sys.stdout.write(json.dumps(env) + "\n")
            return 0
        try:
            recipient_pub = _crypto.b64decode(args.recipient_pub)
        except ValueError:
            sys.stdout.write(json.dumps(env) + "\n")
            return 0
        try:
            wrapped = wrap_envelope(env, my_priv, recipient_pub)
        except Exception as e:
            # Log loud; per CLAUDE.md "never swallow errors", surface
            # to stderr so the user sees crypto failures rather than
            # silent plaintext fallback hiding a real bug.
            print(f"envelope wrap failed: {e}; sending plaintext", file=sys.stderr)
            sys.stdout.write(json.dumps(env) + "\n")
            return 0
        sys.stdout.write(json.dumps(wrapped) + "\n")
        return 0

    if args.cmd == "unwrap":
        if not is_encrypted(env):
            # Plaintext envelope (pre-Phase-E peer or unencrypted broadcast).
            sys.stdout.write(json.dumps(env) + "\n")
            return 0
        if not args.sender_pub:
            # We can't decrypt without the sender's pubkey. Drop the
            # message rather than display ciphertext as garbage.
            print(
                f"envelope unwrap: no sender_pub for from={env.get('from','?')}; dropping",
                file=sys.stderr,
            )
            return 1
        my_priv = _identity.load_priv(args.identity_dir)
        if my_priv is None:
            print("envelope unwrap: no x25519 priv key in identity dir; dropping", file=sys.stderr)
            return 1
        try:
            sender_pub = _crypto.b64decode(args.sender_pub)
        except ValueError:
            print("envelope unwrap: malformed sender_pub", file=sys.stderr)
            return 1
        unwrapped = unwrap_envelope(env, my_priv, sender_pub)
        if unwrapped is None:
            print(
                f"envelope unwrap: AEAD auth failed for from={env.get('from','?')}; dropping",
                file=sys.stderr,
            )
            return 1
        sys.stdout.write(json.dumps(unwrapped) + "\n")
        return 0

    return 1


if __name__ == "__main__":
    import sys
    sys.exit(_cli())
