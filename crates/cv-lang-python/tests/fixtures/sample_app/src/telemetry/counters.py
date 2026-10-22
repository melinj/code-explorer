"""Labelled counters kept in memory until the exporter drains them."""

from __future__ import annotations

import threading
from collections import defaultdict
from typing import Callable

LabelSet = tuple[tuple[str, str], ...]


class CounterRegistry:
    """Monotonic counters keyed by name and label set; safe to bump from any thread."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._values: defaultdict[str, dict[LabelSet, int]] = defaultdict(dict)

    @staticmethod
    def _key(labels: dict[str, str]) -> LabelSet:
        return tuple(sorted(labels.items()))

    def increment(self, name: str, labels: dict[str, str] | None = None, by: int = 1) -> None:
        key = self._key(labels or {})
        with self._lock:
            series = self._values[name]
            series[key] = series.get(key, 0) + by

    def bound(self, name: str, **fixed: str) -> Callable[..., None]:
        """A shortcut bumping *name* with the *fixed* labels plus any given per call."""

        def bump(by: int = 1, **labels: str) -> None:
            self.increment(name, {**fixed, **labels}, by)

        return bump

    def drain(self) -> dict[str, dict[LabelSet, int]]:
        """Every counter's value since the last drain; the counters start again from zero."""
        with self._lock:
            values, self._values = self._values, defaultdict(dict)
        return {name: dict(series) for name, series in values.items()}


registry = CounterRegistry()
