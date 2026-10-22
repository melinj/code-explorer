"""API keys for partner bookshops: shown once, stored as a digest, checked in constant time."""

import hashlib
import hmac
import secrets

PREFIX = "bk_live_"


def digest(key: str) -> str:
    """What we store for *key*: its SHA-256, hex encoded."""
    return hashlib.sha256(key.encode("utf-8")).hexdigest()


def issue() -> tuple[str, str]:
    """A fresh key to show the partner once, and the digest to keep."""
    key = PREFIX + secrets.token_urlsafe(24)
    return key, digest(key)


def verify(presented: str, stored_digest: str) -> bool:
    """Whether *presented* is the key whose digest is *stored_digest*."""
    if not presented.startswith(PREFIX):
        return False
    return hmac.compare_digest(digest(presented), stored_digest)
