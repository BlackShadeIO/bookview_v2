"""Worker process: owns one market folder, runs Polymarket + Binance collectors."""

import asyncio
import logging
import os
import time

from .config import DATA_DIR, CLOB_FILENAME, DEPTH_FILENAME, MARKET_DURATION, TAIL_SECONDS
from .models import SeqCounter, LineWriter, make_line, write_line
from .polymarket import run_polymarket
from .binance import run_binance

log = logging.getLogger(__name__)


async def _timer_task(stop_at: float, stop_event: asyncio.Event) -> None:
    """Sleep until stop time, then signal all tasks to stop."""
    now = time.time()
    remaining = stop_at - now
    if remaining > 0:
        await asyncio.sleep(remaining)
    stop_event.set()


async def _async_main(market_start: int, clob_path: str, depth_path: str) -> None:
    stop_event = asyncio.Event()
    stop_at = market_start + MARKET_DURATION + TAIL_SECONDS

    clob_writer = LineWriter(clob_path)
    depth_writer = LineWriter(depth_path)
    clob_writer.start()
    depth_writer.start()

    clob_seq = SeqCounter()
    depth_seq = SeqCounter()

    write_line(clob_writer, make_line(
        market_start, "polymarket", "system", clob_seq.next(),
        None, {
            "market_start": market_start,
            "pid": os.getpid(),
            "stop_at": stop_at,
        },
        system_type="worker_started",
    ))
    write_line(depth_writer, make_line(
        market_start, "binance", "system", depth_seq.next(),
        None, {
            "market_start": market_start,
            "pid": os.getpid(),
            "stop_at": stop_at,
        },
        system_type="worker_started",
    ))

    try:
        await asyncio.gather(
            run_polymarket(market_start, clob_writer, stop_event),
            run_binance(market_start, depth_writer, stop_event),
            _timer_task(stop_at, stop_event),
        )
    except Exception as exc:
        log.exception("Worker %d caught exception", market_start)
        for writer, source in [(clob_writer, "polymarket"), (depth_writer, "binance")]:
            s = clob_seq if source == "polymarket" else depth_seq
            write_line(writer, make_line(
                market_start, source, "error", s.next(),
                {"error": str(exc), "type": type(exc).__name__},
                {"reason": "worker_exception"},
            ))
    finally:
        write_line(clob_writer, make_line(
            market_start, "polymarket", "collector_stopped", clob_seq.next(),
            None, {"market_start": market_start, "pid": os.getpid()},
        ))
        write_line(depth_writer, make_line(
            market_start, "binance", "collector_stopped", depth_seq.next(),
            None, {"market_start": market_start, "pid": os.getpid()},
        ))
        clob_writer.stop()
        depth_writer.stop()


def worker_main(market_start: int) -> None:
    """Entry point for worker process. Called via multiprocessing.Process."""
    logging.basicConfig(
        level=logging.INFO,
        format=f"%(asctime)s [worker-{market_start}] %(name)s %(levelname)s: %(message)s",
    )

    folder = DATA_DIR / str(market_start)
    folder.mkdir(parents=True, exist_ok=True)

    clob_path = str(folder / CLOB_FILENAME)
    depth_path = str(folder / DEPTH_FILENAME)

    log.info("Worker started for market %d, folder %s", market_start, folder)

    asyncio.run(_async_main(market_start, clob_path, depth_path))

    log.info("Worker finished for market %d", market_start)
