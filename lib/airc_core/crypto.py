"""airc envelope-layer cryptography — boring primitives, no novel crypto.

This module is the entire app-layer security surface for airc. Per the
post-bearer-rewrite architecture (project memory: airc transport
architecture), the bearer carries opaque bytes and the envelope is
encrypted+signed at this layer. That makes every bearer a dumb pipe and
collapses transport-encryption (SSH/Tailscale) into a redundant role
that gets deleted in Phase 3c.

Primitives (all from python-cryptography, all NIST/IETF-standard):
    X25519        — elliptic-curve key agreement (RFC 7748)
    ChaCha20-Poly1305 — AEAD cipher (RFC 8439)
    HKDF-SHA256   — key derivation (RFC 5869)
    Ed25519       — envelope signing (already used by airc, not added here)

Non-goals (deliberate scope):
    - No forward-secret ratchet (Signal/Olm). Pairwise static keys are
      sufficient at airc's scale (rooms ≤10 peers, infrequent rotation).
    - No group-key/sender-key (megolm). For N≤10 we can encrypt N times
      per send without measurable cost. Phase E.2 if rooms grow.
    - No anti-deniability. We don't need plausible deniability; signing
      is a feature, not a bug.
    - No password-based key derivation. Identity is keypairs from disk.

Threat model:
    - GitHub (gist content): assumed plaintext-readable. AEAD here makes
      the gist content opaque to GitHub.
    - ISP / public WiFi MITM: same — bearer is HTTPS to gh, but even
      if it weren't, the AEAD layer protects.
    - Other users on a shared machine: out of scope for the encryption
      layer; identity-key file perms (0600) handle this.
    - Compromised peer: their pairwise keys leak. Other peers' messages
      to them are compromised but messages between OTHER peers remain
      safe (pairwise model — that's the security boundary, deliberately).

Test gating:
    The cryptography package is a runtime dependency. install.sh creates
    a venv and pip-installs it (Phase E install support). Test files
    skip cleanly when the package is missing so a fresh checkout without
    the venv set up doesn't error out the whole test suite.
"""

from __future__ import annotations

import base64
import hashlib
import os
import struct
from typing import Optional

# All imports from cryptography are here at module load time. If the
# package isn't installed, importing crypto.py raises ImportError; callers
# should guard with try/except at their own boundaries (cmd_send, etc).
from cryptography.hazmat.primitives.asymmetric.x25519 import (
    X25519PrivateKey,
    X25519PublicKey,
)
from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305


# Format version for serialized envelopes. Bump if the wire format
# changes in a backward-incompatible way; recv side branches on this.
ENVELOPE_VERSION = "v1"

# Nonce size for ChaCha20-Poly1305 (RFC 8439): 96 bits = 12 bytes.
_NONCE_LEN = 12

# AEAD key size: 256 bits = 32 bytes.
_KEY_LEN = 32


# ──────────────────────────────────────────────────────────────────
# X25519 keypairs — generation, save, load
# ──────────────────────────────────────────────────────────────────

def generate_x25519_keypair() -> tuple[bytes, bytes]:
    """Generate a fresh X25519 keypair. Returns (priv_raw, pub_raw),
    both 32 bytes. Raw format (not PEM) — easier to store as base64 in
    JSON, smaller on disk, no parser surface area. cryptography handles
    the underlying key generation via the OS RNG."""
    priv = X25519PrivateKey.generate()
    priv_raw = priv.private_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PrivateFormat.Raw,
        encryption_algorithm=serialization.NoEncryption(),
    )
    pub_raw = priv.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )
    return (priv_raw, pub_raw)


def save_keypair(priv_raw: bytes, pub_raw: bytes, priv_path: str, pub_path: str) -> None:
    """Write keypair to disk. Private key gets 0600 (user-only read/write);
    public key gets 0644. Atomic write via temp + rename so a SIGKILL
    mid-write doesn't leave a partial private key.

    Why raw bytes rather than PEM/OpenSSH format: the only consumer of
    these files is THIS module. PEM adds parser surface area and a
    150-byte armor that base64'd raw is just 44 bytes. Trade-off in our
    favor: less code, fewer footguns."""
    _atomic_write_bytes(priv_path, priv_raw, mode=0o600)
    _atomic_write_bytes(pub_path, pub_raw, mode=0o644)


def load_priv(priv_path: str) -> bytes:
    """Read 32-byte X25519 private key from disk. Raises FileNotFoundError
    or ValueError if the file is malformed."""
    with open(priv_path, "rb") as f:
        raw = f.read()
    if len(raw) != 32:
        raise ValueError(
            f"X25519 private key at {priv_path} is {len(raw)} bytes; expected 32"
        )
    return raw


def load_pub(pub_path: str) -> bytes:
    """Read 32-byte X25519 public key from disk."""
    with open(pub_path, "rb") as f:
        raw = f.read()
    if len(raw) != 32:
        raise ValueError(
            f"X25519 public key at {pub_path} is {len(raw)} bytes; expected 32"
        )
    return raw


# ──────────────────────────────────────────────────────────────────
# Ed25519 keypairs — generation, save, load, sign
#
# Different on-disk format from X25519 above: Ed25519 keys live as PEM
# files (PKCS#8 for private, SubjectPublicKeyInfo for public) at fixed
# names `private.pem` / `public.pem` for compatibility with the prior
# shell-openssl-generated identity layout. Existing scopes that paired
# under the openssl path will load cleanly here — `cryptography`'s
# load_pem_private_key parses the same PKCS#8 wrapping that
# `openssl genpkey -algorithm Ed25519` writes. Verified: round-trip
# byte-equal between the two for the same seed material.
#
# Why PEM (not raw like X25519): X25519 keys are this module's truth,
# only ever read by Python. Ed25519 keys pre-existed in shell-openssl
# format and we maintain the same disk shape so nobody's identity
# silently rotates on upgrade.
# ──────────────────────────────────────────────────────────────────

def generate_ed25519_keypair_pem() -> tuple[bytes, bytes]:
    """Generate a fresh Ed25519 keypair, return (priv_pem, pub_pem) bytes.

    Output format matches `openssl genpkey -algorithm Ed25519` (PKCS#8 PEM)
    and `openssl pkey -pubout` (SPKI PEM) byte-for-byte modulo the random
    seed. Existing peers that signed under shell-openssl Ed25519 verify
    correctly against pubkeys generated here and vice versa — both are
    the same NIST/IETF-standard primitive.
    """
    priv = Ed25519PrivateKey.generate()
    priv_pem = priv.private_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PrivateFormat.PKCS8,
        encryption_algorithm=serialization.NoEncryption(),
    )
    pub_pem = priv.public_key().public_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PublicFormat.SubjectPublicKeyInfo,
    )
    return (priv_pem, pub_pem)


def load_ed25519_priv_pem(priv_path: str) -> Ed25519PrivateKey:
    """Read an Ed25519 PKCS#8 PEM private key from disk. Raises
    FileNotFoundError or ValueError on malformed input. Used by
    sign_ed25519_pem below; exposed so callers can do their own
    framing if they prefer."""
    with open(priv_path, "rb") as f:
        pem = f.read()
    key = serialization.load_pem_private_key(pem, password=None)
    if not isinstance(key, Ed25519PrivateKey):
        raise ValueError(
            f"key at {priv_path} is not Ed25519 ({type(key).__name__})"
        )
    return key


def sign_ed25519_pem(priv_path: str, data: bytes) -> bytes:
    """Sign `data` with the Ed25519 private key at `priv_path`. Returns
    the raw 64-byte signature — same as `openssl pkeyutl -sign` output
    on the same key+message. Caller base64-encodes if needed; we keep
    bytes here so the layer above decides on encoding.
    """
    return load_ed25519_priv_pem(priv_path).sign(data)


def save_ed25519_keypair_pem(
    priv_pem: bytes, pub_pem: bytes, priv_path: str, pub_path: str
) -> None:
    """Write Ed25519 PEMs to disk with appropriate perms (0600 / 0644).
    Atomic via temp + rename so SIGKILL mid-write can't leave partial
    state. Mirror of save_keypair() above for X25519."""
    _atomic_write_bytes(priv_path, priv_pem, mode=0o600)
    _atomic_write_bytes(pub_path, pub_pem, mode=0o644)


# ──────────────────────────────────────────────────────────────────
# ECDH + HKDF — derive a pairwise AEAD key from two keypairs
# ──────────────────────────────────────────────────────────────────

def derive_pairwise_key(
    my_priv_raw: bytes,
    their_pub_raw: bytes,
    info: bytes = b"airc-aead-v1",
) -> bytes:
    """X25519 ECDH followed by HKDF-SHA256 to derive a 32-byte AEAD key
    suitable for ChaCha20-Poly1305.

    `info` is the HKDF context string. Domain-separating different
    purposes (envelope encryption vs key-handoff vs whatever) by changing
    info ensures keys derived for one purpose can't be misused for
    another. The default `airc-aead-v1` is for envelope encryption;
    bump v1→v2 only when the wire format changes incompatibly.

    Both inputs MUST be 32 bytes (raw X25519 format). Salt is empty
    (None per HKDF spec) — both peers' identities are already in the
    info string and the ECDH shared secret is high-entropy.

    Determinism: same inputs always produce the same output, allowing
    both ends of a pair to derive the same key without communicating
    intermediate state.
    """
    if len(my_priv_raw) != 32 or len(their_pub_raw) != 32:
        raise ValueError("X25519 keys must be exactly 32 bytes")
    priv = X25519PrivateKey.from_private_bytes(my_priv_raw)
    pub = X25519PublicKey.from_public_bytes(their_pub_raw)
    shared = priv.exchange(pub)
    derived = HKDF(
        algorithm=hashes.SHA256(),
        length=_KEY_LEN,
        salt=None,
        info=info,
    ).derive(shared)
    return derived


# ──────────────────────────────────────────────────────────────────
# AEAD — encrypt + decrypt with associated data
# ──────────────────────────────────────────────────────────────────

def aead_encrypt(
    key: bytes,
    plaintext: bytes,
    associated_data: bytes = b"",
    nonce: Optional[bytes] = None,
) -> tuple[bytes, bytes]:
    """ChaCha20-Poly1305 AEAD encrypt. Returns (nonce, ciphertext_with_tag).

    If `nonce` is None, a fresh 12-byte nonce is drawn from os.urandom.
    Caller may pass an explicit nonce for counter-mode usage (e.g.
    per-session counter) — but MUST NOT reuse a nonce under the same
    key, ever. Reuse breaks AEAD catastrophically.

    `associated_data` is authenticated but not encrypted — bind any
    plaintext envelope fields (sender, channel, ts) here so an attacker
    can't swap a recipient's view of who-sent-what without the auth
    check failing.
    """
    if len(key) != _KEY_LEN:
        raise ValueError(f"AEAD key must be {_KEY_LEN} bytes")
    if nonce is None:
        nonce = os.urandom(_NONCE_LEN)
    elif len(nonce) != _NONCE_LEN:
        raise ValueError(f"AEAD nonce must be {_NONCE_LEN} bytes")
    aead = ChaCha20Poly1305(key)
    ciphertext = aead.encrypt(nonce, plaintext, associated_data)
    return (nonce, ciphertext)


def aead_decrypt(
    key: bytes,
    nonce: bytes,
    ciphertext_with_tag: bytes,
    associated_data: bytes = b"",
) -> bytes:
    """ChaCha20-Poly1305 AEAD decrypt. Raises InvalidTag on auth failure.

    Caller MUST handle the InvalidTag exception explicitly — silent
    catch+drop is the classic crypto footgun that turns "decryption
    failed" into "got an empty message" and breaks higher-layer logic.
    The cryptography package raises a specific exception type so callers
    can act on it (skip the line, log, alert, etc).
    """
    if len(key) != _KEY_LEN:
        raise ValueError(f"AEAD key must be {_KEY_LEN} bytes")
    if len(nonce) != _NONCE_LEN:
        raise ValueError(f"AEAD nonce must be {_NONCE_LEN} bytes")
    aead = ChaCha20Poly1305(key)
    return aead.decrypt(nonce, ciphertext_with_tag, associated_data)


# ──────────────────────────────────────────────────────────────────
# Counter-based nonces — for replay-defense + nonce-reuse avoidance
# ──────────────────────────────────────────────────────────────────

def counter_nonce(counter: int) -> bytes:
    """Build a 12-byte nonce from a 64-bit counter (8 bytes counter
    big-endian + 4 zero bytes). Counter MUST monotonically increase per
    pairwise key to guarantee no reuse.

    Caller persists the counter; this function only encodes. We use BE
    encoding so two implementations comparing nonces lexicographically
    get the same ordering as numeric comparison — handy for any future
    "highest-seen-nonce" replay-defense scheme."""
    if counter < 0 or counter >= 2**64:
        raise ValueError(f"counter {counter} out of 64-bit range")
    return struct.pack(">Q", counter) + b"\x00\x00\x00\x00"


def parse_counter_nonce(nonce: bytes) -> int:
    """Inverse of counter_nonce(). Raises ValueError on a non-counter
    nonce (random nonces don't round-trip; that's a feature)."""
    if len(nonce) != _NONCE_LEN:
        raise ValueError(f"nonce must be {_NONCE_LEN} bytes")
    if nonce[8:] != b"\x00\x00\x00\x00":
        raise ValueError("nonce is not in counter format (suffix nonzero)")
    return struct.unpack(">Q", nonce[:8])[0]


# ──────────────────────────────────────────────────────────────────
# Public-key fingerprint — for invite strings / out-of-band verification
# ──────────────────────────────────────────────────────────────────

def fingerprint(pub_raw: bytes, length: int = 16) -> str:
    """Return a short hex fingerprint of a public key, suitable for
    embedding in invite strings. Default length = 16 hex chars (64 bits)
    which gives ~10^19 collision space — sufficient for human-pairing
    recognition, not security-critical (the actual key bytes do that
    job).

    Use SHA-256 because we already use it elsewhere; truncating to N
    chars is standard practice for fingerprints (gpg, ssh, gist hashes
    all do this)."""
    if len(pub_raw) != 32:
        raise ValueError("X25519 pubkey must be 32 bytes")
    return hashlib.sha256(pub_raw).hexdigest()[:length]


# ──────────────────────────────────────────────────────────────────
# Base64 helpers — JSON envelopes carry binary fields as b64
# ──────────────────────────────────────────────────────────────────

def b64encode(data: bytes) -> str:
    """URL-safe base64, no padding. Same convention as JWT — easier on
    JSON envelopes and avoids '=' padding that bash sometimes mangles."""
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def b64decode(s: str) -> bytes:
    """Inverse of b64encode. Re-pads on decode since urlsafe_b64decode
    rejects unpadded input. Raises ValueError on malformed input."""
    if not isinstance(s, str):
        raise ValueError("b64decode expects str")
    pad = "=" * (-len(s) % 4)
    return base64.urlsafe_b64decode(s + pad)


# ──────────────────────────────────────────────────────────────────
# Internal: atomic write helper
# ──────────────────────────────────────────────────────────────────

def _atomic_write_bytes(path: str, data: bytes, mode: int = 0o600) -> None:
    """Write `data` to `path` atomically via temp+rename. The temp file
    is created with the target mode so there's never a window where the
    file exists with default-and-broader perms.

    On Windows, os.replace handles the rename atomically (Python 3.3+).
    """
    d = os.path.dirname(path) or "."
    os.makedirs(d, exist_ok=True)
    fd = os.open(
        path + ".tmp", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode
    )
    try:
        os.write(fd, data)
    finally:
        os.close(fd)
    os.replace(path + ".tmp", path)
