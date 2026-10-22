"""Compact JSON for telemetry batches: one record per line, keys sorted so diffs stay small."""

import json as _json
from datetime import datetime
from decimal import Decimal


def _fallback(value: object) -> object:
    if isinstance(value, datetime):
        return value.isoformat()
    if isinstance(value, Decimal):
        return str(value)
    raise TypeError(f"cannot encode {type(value).__name__} in a telemetry record")


def dumps(record: dict) -> str:
    """*record* as one line of compact JSON."""
    return _json.dumps(record, default=_fallback, separators=(",", ":"), sort_keys=True)


def encode_batch(records: list[dict]) -> bytes:
    """Newline-delimited JSON for *records*, UTF-8 encoded."""
    return "".join(dumps(record) + "\n" for record in records).encode("utf-8")
