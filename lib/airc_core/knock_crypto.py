"""Crypto helpers for the airc knock/approve flow (airc#559 PR-2).

Exposes a tiny CLI wrapper around lib/airc_core/crypto.py so the bash
cmd_knock + cmd_approve + cmd_decrypt-approval functions can do X25519
ECDH + ChaCha20-Poly1305 AEAD without re-implementing crypto in shell.

Why ephemeral-per-knock + ephemeral-per-approval (forward secrecy):

  Knock posts the knocker's ephemeral pubkey on a public GitHub issue.
  Approver derives a shared key via ECDH(approver_ephemeral, knocker_ephemeral)
  and posts the approver's ephemeral pubkey + nonce + ChaCha20-Poly1305
  ciphertext as a comment.

  Both ephemerals are per-message (not long-term) so even if either
  party's long-term identity key leaks YEARS later, every prior
  approval's join string is unrecoverable. The ephemerals are NEVER
  written to disk after the protocol completes — knocker keeps their
  priv key only between knock-time and approval-decrypt time.

Verbs:

  gen-knock-keys
      Emit a fresh X25519 keypair as JSON: {"priv": "<hex>", "pub": "<hex>"}.
      Knocker generates this per knock; saves priv securely (or stdout
      one-shot for now); embeds pub in the issue envelope.

  encrypt-for-knocker --knocker-pub <hex> --plaintext <str>
      Generate per-approval ephemeral keypair, derive shared key via
      ECDH(approver_priv, knocker_pub), AEAD-encrypt plaintext.
      Emits JSON: {"ver": "v1", "approver_pub": "<hex>",
                   "nonce": "<hex>", "ciphertext": "<hex>"}.

  decrypt-from-approver --knocker-priv <hex> --approver-pub <hex>
                        --nonce <hex> --ciphertext <hex>
      Derive shared key via ECDH(knocker_priv, approver_pub),
      AEAD-decrypt. Emits the plaintext to stdout. Exit 1 on auth
      failure (InvalidTag).

All hex inputs are case-insensitive; outputs lower-case. Hex is used
(not base64) so the values fit cleanly inside a JSON envelope embedded
in a markdown fenced block + are easy to eyeball/copy-paste in
operator UX.

Domain-separated HKDF context: `airc-knock-approve-v1`. Bumping v1→v2
later signals an incompatible wire format change.
"""

from __future__ import annotations

import argparse
import json
import sys

try:
    from . import crypto as airc_crypto
except ImportError:
    # When invoked as a script (python3 -m airc_core.knock_crypto), the
    # relative import works. When run as a standalone file (rare; tests
    # may do this), fall back to the absolute import.
    from airc_core import crypto as airc_crypto


HKDF_INFO = b"airc-knock-approve-v1"


def _hex_to_bytes(name: str, value: str) -> bytes:
    try:
        return bytes.fromhex(value.strip())
    except ValueError as exc:
        raise SystemExit(f"{name}: not valid hex: {exc}")


def cmd_gen_knock_keys(_args: argparse.Namespace) -> int:
    priv, pub = airc_crypto.generate_x25519_keypair()
    print(json.dumps({"priv": priv.hex(), "pub": pub.hex()}))
    return 0


def cmd_encrypt_for_knocker(args: argparse.Namespace) -> int:
    knocker_pub = _hex_to_bytes("--knocker-pub", args.knocker_pub)
    if len(knocker_pub) != 32:
        raise SystemExit("--knocker-pub must decode to 32 bytes")

    approver_priv, approver_pub = airc_crypto.generate_x25519_keypair()
    shared_key = airc_crypto.derive_pairwise_key(
        approver_priv, knocker_pub, info=HKDF_INFO
    )
    plaintext = args.plaintext.encode("utf-8")
    nonce, ciphertext = airc_crypto.aead_encrypt(shared_key, plaintext)

    print(json.dumps({
        "ver": "v1",
        "approver_pub": approver_pub.hex(),
        "nonce": nonce.hex(),
        "ciphertext": ciphertext.hex(),
    }))
    return 0


def cmd_decrypt_from_approver(args: argparse.Namespace) -> int:
    knocker_priv = _hex_to_bytes("--knocker-priv", args.knocker_priv)
    approver_pub = _hex_to_bytes("--approver-pub", args.approver_pub)
    nonce = _hex_to_bytes("--nonce", args.nonce)
    ciphertext = _hex_to_bytes("--ciphertext", args.ciphertext)

    if len(knocker_priv) != 32 or len(approver_pub) != 32:
        raise SystemExit("knocker-priv + approver-pub must each decode to 32 bytes")

    shared_key = airc_crypto.derive_pairwise_key(
        knocker_priv, approver_pub, info=HKDF_INFO
    )
    try:
        plaintext = airc_crypto.aead_decrypt(shared_key, nonce, ciphertext)
    except Exception as exc:  # InvalidTag is the expected one; surface anything
        # InvalidTag means either the wrong key, wrong nonce, or
        # tampered ciphertext. Per Joel's "never swallow errors" rule,
        # exit non-zero with the actual exception type so the caller
        # sees what failed (vs returning empty string).
        print(f"airc knock decrypt: AEAD authentication failed: {exc}",
              file=sys.stderr)
        return 1

    sys.stdout.buffer.write(plaintext)
    if not plaintext.endswith(b"\n"):
        sys.stdout.buffer.write(b"\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="airc_core.knock_crypto",
        description="Crypto helpers for airc knock/approve flow.",
    )
    sub = parser.add_subparsers(dest="cmd", required=True)

    gen = sub.add_parser("gen-knock-keys",
                         help="Emit a fresh X25519 keypair as JSON.")
    gen.set_defaults(func=cmd_gen_knock_keys)

    enc = sub.add_parser(
        "encrypt-for-knocker",
        help="Encrypt plaintext to a knocker's pubkey via ECDH+AEAD.",
    )
    enc.add_argument("--knocker-pub", required=True,
                     help="Knocker's ephemeral X25519 pubkey, hex.")
    enc.add_argument("--plaintext", required=True,
                     help="String to encrypt (UTF-8).")
    enc.set_defaults(func=cmd_encrypt_for_knocker)

    dec = sub.add_parser(
        "decrypt-from-approver",
        help="Decrypt approver-posted ciphertext using knocker's priv key.",
    )
    dec.add_argument("--knocker-priv", required=True,
                     help="Knocker's ephemeral X25519 priv key, hex.")
    dec.add_argument("--approver-pub", required=True,
                     help="Approver's ephemeral X25519 pubkey from comment, hex.")
    dec.add_argument("--nonce", required=True,
                     help="ChaCha20-Poly1305 nonce from comment, hex.")
    dec.add_argument("--ciphertext", required=True,
                     help="AEAD ciphertext+tag from comment, hex.")
    dec.set_defaults(func=cmd_decrypt_from_approver)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
