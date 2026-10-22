"""In-process counters and timings, exported in batches; tracing spans are optional."""

from src.telemetry.counters import CounterRegistry, registry
from src.telemetry.timing import Stopwatch, timed

try:
    from src.telemetry.tracing_bridge import span
except ImportError:  # the tracing extra is not installed
    from contextlib import nullcontext

    def span(name: str, **attributes: object):  # type: ignore[no-redef]
        """A do-nothing stand-in for ``tracing_bridge.span``."""
        return nullcontext()


__all__ = ["CounterRegistry", "Stopwatch", "registry", "span", "timed"]
