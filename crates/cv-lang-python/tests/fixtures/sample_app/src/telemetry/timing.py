"""Timing helpers: a stopwatch context manager, and a decorator counting calls by duration."""

from __future__ import annotations

import functools
import time
from typing import Callable, ParamSpec, TypeVar

from src.telemetry.counters import registry

P = ParamSpec("P")
R = TypeVar("R")

_BUCKETS_MS = (5, 25, 100, 500, 2000)


class Stopwatch:
    """``with Stopwatch() as watch: ...``, then read ``watch.elapsed_ms``."""

    def __init__(self, clock: Callable[[], float] = time.perf_counter) -> None:
        self._clock = clock
        self._started = 0.0
        self.elapsed_ms = 0

    def __enter__(self) -> Stopwatch:
        self._started = self._clock()
        return self

    def __exit__(self, *exc_info: object) -> None:
        self.elapsed_ms = round((self._clock() - self._started) * 1000)


def _bucket(ms: int) -> str:
    """The smallest bucket bound at or above *ms*, or ``inf``."""
    return next((f"le{bound}" for bound in _BUCKETS_MS if ms <= bound), "inf")


def timed(name: str) -> Callable[[Callable[P, R]], Callable[P, R]]:
    """Count each call of the decorated function under ``<name>_ms`` by outcome and duration."""

    def decorate(fn: Callable[P, R]) -> Callable[P, R]:
        @functools.wraps(fn)
        def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
            outcome = "error"
            watch = Stopwatch()
            try:
                with watch:
                    result = fn(*args, **kwargs)
                outcome = "ok"
                return result
            finally:
                registry.increment(f"{name}_ms", {"outcome": outcome, "le": _bucket(watch.elapsed_ms)})

        return wrapper

    return decorate
