"""Ships drained counters to the collector every few seconds, from a background thread."""

from __future__ import annotations

import logging
import threading
from typing import TYPE_CHECKING

from src.telemetry.counters import CounterRegistry, registry
from src.telemetry.encoding import encode_batch

if TYPE_CHECKING:
    from src.telemetry.http_sink import HttpSink

logger = logging.getLogger(__name__)


def default_sink() -> HttpSink:
    """A sink posting to the collector named by ``TELEMETRY_URL``."""
    import os

    from src.telemetry.http_sink import HttpSink

    return HttpSink(os.environ.get("TELEMETRY_URL", "http://localhost:4318/v1/counters"))


class Exporter:
    """Drains a registry on an interval and hands each encoded batch to a sink."""

    def __init__(
        self,
        sink: HttpSink | None = None,
        source: CounterRegistry = registry,
        interval: float = 10.0,
    ) -> None:
        self._sink = sink or default_sink()
        self._source = source
        self._interval = interval
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, name="telemetry-export", daemon=True)

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(timeout=self._interval)
        self.flush()

    def flush(self) -> int:
        """Send what has piled up; returns how many records went out (none on failure)."""
        records = [
            {"name": name, "labels": dict(labels), "value": value}
            for name, series in self._source.drain().items()
            for labels, value in series.items()
        ]
        if not records:
            return 0
        try:
            self._sink.send(encode_batch(records))
        except OSError as err:
            logger.warning(f"telemetry export failed, dropping {len(records)} records: {err}")
            return 0
        return len(records)

    def _run(self) -> None:
        while not self._stop.wait(self._interval):
            self.flush()
