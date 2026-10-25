#!/usr/bin/env python3
"""Generate swarmgit M0 test vectors with an independent Python implementation.

The Rust code under test never produces these values. `--check` recomputes
everything and compares against the committed files, so a changed formula in
either implementation fails the M0 acceptance gate.

Requires the `blake3` Python package.
"""

from __future__ import annotations

import argparse
import base64
import struct
import sys
from pathlib import Path

import blake3

ROOT = Path(__file__).resolve().parent.parent
VECTORS = ROOT / "tests" / "vectors"

CTX_KEYID = "swarmgit/v1/keyid"
CTX_TOPIC = "swarmgit/v1/topic"
CTX_INTEREST = "swarmgit/v1/interest"
CTX_LAN = "swarmgit/v1/lan"
CTX_SESSION = "swarmgit/v1/session"
CTX_SIG_MANIFEST = b"swarmgit/v1/manifest\0"
CTX_NODEAUTH = b"swarmgit/v1/nodeauth"

# RFC 8032 test-vector public key for seed 9d61...7f60.
ED25519_PK = bytes.fromhex(
    "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
)

# First ML-DSA-65 public key in the vendored ACVP sigGen group is read from
# tests/vectors/pq/ml-dsa-65.json, so the vector stays tied to a real key.
KAT_ML_DSA = VECTORS / "pq" / "ml-dsa-65.json"


def ml_dsa_public_key() -> bytes:
    if not KAT_ML_DSA.exists():
        raise SystemExit(f"missing vendored KAT: {KAT_ML_DSA}")
    import json

    data = json.loads(KAT_ML_DSA.read_text())
    return bytes.fromhex(data["testGroups"][0]["tests"][0]["pk"])


def derive(context: str, material: bytes, length: int = 32) -> bytes:
    return blake3.blake3(material, derive_key_context=context).digest(length=length)


def keyed(key: bytes, data: bytes, length: int = 32) -> bytes:
    return blake3.blake3(data, key=key).digest(length=length)


def u64(value: int) -> bytes:
    return struct.pack(">Q", value)


def enc_head(major: int, value: int) -> bytes:
    prefix = major << 5
    if value < 24:
        return bytes([prefix | value])
    if value <= 0xFF:
        return bytes([prefix | 24, value])
    if value <= 0xFFFF:
        return bytes([prefix | 25]) + struct.pack(">H", value)
    if value <= 0xFFFFFFFF:
        return bytes([prefix | 26]) + struct.pack(">I", value)
    return bytes([prefix | 27]) + struct.pack(">Q", value)


def enc_uint(value: int) -> bytes:
    return enc_head(0, value)


def enc_bytes(value: bytes) -> bytes:
    return enc_head(2, len(value)) + value


def enc_text(value: str) -> bytes:
    raw = value.encode()
    return enc_head(3, len(raw)) + raw


def enc_array(values: list[bytes]) -> bytes:
    return enc_head(4, len(values)) + b"".join(values)


def enc_map(entries: dict[bytes, bytes]) -> bytes:
    items = sorted(entries.items(), key=lambda pair: pair[0])
    return enc_head(5, len(items)) + b"".join(k + v for k, v in items)


def manifest_bytes() -> bytes:
    repo_id = bytes([0x11]) * 32
    signer = bytes([0x22]) * 32
    prev = bytes([0x33]) * 32
    signer_keys = bytes([0x5A]) * 1984
    refs = enc_map(
        {
            enc_text("refs/heads/main"): enc_bytes(bytes([0x44]) * 32),
            enc_text("refs/tags/v1"): enc_bytes(bytes([0x55]) * 32),
        }
    )
    return enc_map(
        {
            enc_uint(0): enc_uint(1),
            enc_uint(1): enc_bytes(repo_id),
            enc_uint(2): enc_bytes(signer),
            enc_uint(3): enc_uint(2),
            enc_uint(4): enc_bytes(prev),
            enc_uint(5): enc_uint(1795600000),
            enc_uint(6): refs,
            enc_uint(7): enc_text("refs/heads/main"),
            enc_uint(9): enc_text("sha256"),
            enc_uint(14): enc_bytes(signer_keys),
            enc_uint(15): enc_uint(1),
        }
    )


def build_crypto() -> list[tuple[str, bytes]]:
    repo = bytes([1]) * 32
    hh = bytes([2]) * 32
    ri = bytes([3]) * 32
    rr = bytes([4]) * 32
    ss = bytes([5]) * 32
    day = 20416
    hour = 490_000
    bundle = ED25519_PK + ml_dsa_public_key()
    interest_key = derive(CTX_INTEREST, repo)
    lan_key = derive(CTX_LAN, repo)
    session_material = hh + ri + rr + ss
    manifest = manifest_bytes()
    dummy_envelope = enc_array([enc_bytes(manifest), enc_bytes(bytes(3373))])
    auth = (
        CTX_NODEAUTH
        + hh
        + bytes([0xAA]) * 4
        + bytes([0xBB]) * 4
        + ri
        + rr
        + bytes([1])
    )
    return [
        ("suite1_bundle", bundle),
        ("repo", repo),
        ("hh", hh),
        ("ri", ri),
        ("rr", rr),
        ("ss", ss),
        ("ek", bytes([0xAA]) * 4),
        ("ct", bytes([0xBB]) * 4),
        ("key_id_suite1_bundle", derive(CTX_KEYID, bundle)),
        ("repo_id_equals_key_id", derive(CTX_KEYID, bundle)),
        ("node_id_equals_key_id", derive(CTX_KEYID, bundle)),
        ("day", u64(day)),
        ("hour", u64(hour)),
        ("topic", derive(CTX_TOPIC, repo + u64(day))[:20]),
        ("interest_key", interest_key),
        ("interest", keyed(interest_key, hh, 16)),
        ("lan_key", lan_key),
        ("lan_token", keyed(lan_key, u64(hour), 16)),
        ("session_okm", derive(CTX_SESSION, session_material, 64)),
        ("sig_input", CTX_SIG_MANIFEST + manifest),
        ("auth_input", auth),
        ("manifest", manifest),
        ("manifest_id", blake3.blake3(dummy_envelope).digest()),
    ]


def build_ids() -> list[tuple[str, bytes]]:
    zero = bytes(32)
    pattern = bytes(range(32))
    return [
        ("base32_zero32", base64.b32encode(zero).rstrip(b"=").lower()),
        ("base32_pattern", base64.b32encode(pattern).rstrip(b"=").lower()),
        ("hex_pattern", pattern.hex().encode()),
    ]


def render(entries: list[tuple[str, bytes]]) -> str:
    lines = ["# Generated by scripts/gen_vectors.py; do not edit by hand."]
    lines.extend(f"{name} = {value.hex()}" for name, value in entries)
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="recompute and compare instead of writing files",
    )
    args = parser.parse_args()

    files = {
        "crypto.hex": render(build_crypto()),
        "ids.hex": render(build_ids()),
    }

    failures = 0
    for name, content in files.items():
        path = VECTORS / name
        if args.check:
            if not path.exists():
                print(f"missing {path}", file=sys.stderr)
                failures += 1
            elif path.read_text() != content:
                print(f"vector mismatch: {path}", file=sys.stderr)
                failures += 1
            else:
                print(f"ok {path}")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
            print(f"wrote {path}")

    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
