"""Token-bucket throttling keyed by client, for the public API and for checkout."""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Callable, Protocol, runtime_checkable


@dataclass
class Bucket:
    """Tokens left, and when they were last topped up."""

    tokens: float
    updated: float


@runtime_checkable
class BucketStore(Protocol):
    """Where buckets live between requests (a dict here, a shared cache in production)."""

    def load(self, key: str) -> Bucket | None: ...

    def save(self, key: str, bucket: Bucket) -> None: ...


class MemoryBucketStore:
    """Buckets in a dict: enough for one process, and for tests."""

    def __init__(self) -> None:
        self._buckets: dict[str, Bucket] = {}

    def load(self, key: str) -> Bucket | None:
        return self._buckets.get(key)

    def save(self, key: str, bucket: Bucket) -> None:
        self._buckets[key] = bucket


_store: BucketStore = MemoryBucketStore()


def use_store(store: BucketStore) -> None:
    """Replace the bucket store, at startup or in a test."""
    global _store
    _store = store


def allow(key: str, *, rate: float, burst: float, clock: Callable[[], float] = time.monotonic) -> bool:
    """Take a token from *key*'s bucket, which refills at *rate* per second up to *burst*."""
    now = clock()
    bucket = _store.load(key) or Bucket(tokens=burst, updated=now)
    bucket.tokens = min(burst, bucket.tokens + (now - bucket.updated) * rate)
    bucket.updated = now
    allowed = bucket.tokens >= 1
    if allowed:
        bucket.tokens -= 1
    _store.save(key, bucket)
    return allowed
