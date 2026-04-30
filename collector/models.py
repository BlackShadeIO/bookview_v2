"""JSONL line builder, sequence counter, and threaded writer."""

import os
import queue
import threading
import time
from typing import Any

import orjson


class SeqCounter:
    """Monotonically increasing per-file sequence counter."""

    __slots__ = ("_v",)

    def __init__(self) -> None:
        self._v = 0

    def next(self) -> int:
        self._v += 1
        return self._v


class LineWriter:
    """Dedicated writer thread. Accepts dicts via put(), serializes + writes in background."""

    __slots__ = ("_queue", "_path", "_thread", "_file")

    def __init__(self, path: str) -> None:
        self._queue: queue.Queue[dict | None] = queue.Queue()
        self._path = path
        self._thread: threading.Thread | None = None
        self._file = None

    def start(self) -> None:
        self._file = open(self._path, "ab")
        self._thread = threading.Thread(
            target=self._run, daemon=True, name=f"writer-{os.path.basename(self._path)}",
        )
        self._thread.start()

    def put(self, line: dict) -> None:
        self._queue.put(line)

    def stop(self) -> None:
        self._queue.put(None)
        if self._thread:
            self._thread.join(timeout=10.0)
        if self._file:
            self._file.close()

    def _run(self) -> None:
        f = self._file
        q = self._queue
        last_flush = time.monotonic()
        newline = b"\n"

        while True:
            try:
                line = q.get(timeout=0.05)
            except queue.Empty:
                now = time.monotonic()
                if now - last_flush >= 0.1:
                    f.flush()
                    last_flush = now
                continue

            if line is None:
                f.flush()
                return

            f.write(orjson.dumps(line) + newline)

            while True:
                try:
                    line = q.get_nowait()
                except queue.Empty:
                    break
                if line is None:
                    f.flush()
                    return
                f.write(orjson.dumps(line) + newline)

            now = time.monotonic()
            if now - last_flush >= 0.1:
                f.flush()
                last_flush = now


def make_line(
    market_start: int,
    source: str,
    event_type: str,
    seq: int,
    raw: Any,
    normalized: Any,
    *,
    system_type: str | None = None,
) -> dict:
    now_ms = int(time.time() * 1000)
    now_s = now_ms / 1000.0
    line: dict[str, Any] = {
        "ts_recv_ms": now_ms,
        "ts_recv_unix": now_s,
        "market_start": market_start,
        "collector_pid": os.getpid(),
        "source": source,
        "event_type": event_type,
        "seq": seq,
        "raw": raw,
        "normalized": normalized,
    }
    if system_type is not None:
        line["system_type"] = system_type
    return line


def write_line(writer: LineWriter, line: dict) -> None:
    writer.put(line)
