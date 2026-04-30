"""Supervisor process: schedule and manage worker processes."""

import logging
import math
import signal
import sys
import time
from multiprocessing import Process

from .config import PRE_START_SECONDS, MARKET_INTERVAL, MARKET_DURATION, TAIL_SECONDS
from .worker import worker_main

log = logging.getLogger(__name__)


def _next_market_start(now: float) -> int:
    """Compute next eligible market_start such that we still have 70s lead time."""
    return math.ceil((now + PRE_START_SECONDS) / MARKET_INTERVAL) * MARKET_INTERVAL


def _reap_workers(
    active: dict[int, Process],
    restart_counts: dict[int, int],
    max_restarts: int = 1,
) -> None:
    """Join finished workers. Restart if died prematurely (once per market)."""
    finished = []
    for ms, proc in active.items():
        if not proc.is_alive():
            proc.join(timeout=1)
            finished.append(ms)

    now = time.time()
    for ms in finished:
        proc = active.pop(ms)
        expected_stop = ms + MARKET_DURATION + TAIL_SECONDS

        if now < expected_stop and proc.exitcode != 0:
            count = restart_counts.get(ms, 0)
            if count < max_restarts:
                log.warning(
                    "Worker %d died early (exit=%s), restarting (%d/%d)",
                    ms, proc.exitcode, count + 1, max_restarts,
                )
                p = Process(target=worker_main, args=(ms,), daemon=True)
                p.start()
                active[ms] = p
                restart_counts[ms] = count + 1
            else:
                log.error(
                    "Worker %d died early (exit=%s), max restarts reached",
                    ms, proc.exitcode,
                )
        else:
            log.info("Worker %d finished normally (exit=%s)", ms, proc.exitcode)


def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [supervisor] %(name)s %(levelname)s: %(message)s",
    )

    launched: set[int] = set()
    active: dict[int, Process] = {}
    restart_counts: dict[int, int] = {}

    # Graceful shutdown
    shutdown = False

    def _handle_signal(signum: int, frame: object) -> None:
        nonlocal shutdown
        log.info("Received signal %d, shutting down", signum)
        shutdown = True

    signal.signal(signal.SIGINT, _handle_signal)
    signal.signal(signal.SIGTERM, _handle_signal)

    log.info("Supervisor started, PID %d", __import__("os").getpid())

    while not shutdown:
        now = time.time()
        target = _next_market_start(now)
        launch_at = target - PRE_START_SECONDS

        sleep_secs = launch_at - time.time()
        if sleep_secs > 0:
            log.info(
                "Next market %d, launching at %d (sleeping %.1fs)",
                target, int(launch_at), sleep_secs,
            )
            # Sleep in small increments so we can respond to signals
            end = time.time() + sleep_secs
            while time.time() < end and not shutdown:
                time.sleep(min(1.0, end - time.time()))

        if shutdown:
            break

        if target not in launched:
            log.info("Spawning worker for market %d", target)
            p = Process(target=worker_main, args=(target,), daemon=True)
            p.start()
            launched.add(target)
            active[target] = p
            log.info("Worker for market %d started, PID %d", target, p.pid)

        _reap_workers(active, restart_counts)

        # Clean old entries from launched set (keep last hour)
        cutoff = now - 3600
        launched = {ms for ms in launched if ms > cutoff}

    # Shutdown: wait for active workers
    log.info("Shutting down, waiting for %d active workers", len(active))
    for ms, proc in active.items():
        log.info("Waiting for worker %d (PID %d)", ms, proc.pid)
        proc.join(timeout=30)
        if proc.is_alive():
            log.warning("Worker %d still alive, terminating", ms)
            proc.terminate()
            proc.join(timeout=5)

    log.info("Supervisor exited")
