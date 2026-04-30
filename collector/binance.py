"""Binance BTCUSDT order book collector: depth + bookTicker with snapshot sync."""

import asyncio
import logging
import time
from decimal import Decimal
from typing import Any

import aiohttp
import orjson
from websockets.asyncio.client import connect
from websockets.exceptions import ConnectionClosed

from .config import (
    BINANCE_DEPTH_SNAPSHOT_URL,
    BINANCE_WS_URL,
    WS_RECONNECT_BASE,
    WS_RECONNECT_MAX,
)
from .models import SeqCounter, LineWriter, make_line, write_line

log = logging.getLogger(__name__)


# ── Local order book ────────────────────────────────────────────────────────

class BinanceBook:
    """Full local order book rebuilt from snapshot + diffs."""

    def __init__(self) -> None:
        self.bids: dict[str, str] = {}  # price → qty
        self.asks: dict[str, str] = {}  # price → qty
        self.update_id: int = 0

    def load_snapshot(self, snap: dict) -> None:
        self.bids = {str(p): str(q) for p, q in snap["bids"]}
        self.asks = {str(p): str(q) for p, q in snap["asks"]}
        self.update_id = snap["lastUpdateId"]

    def apply_diff(self, event: dict) -> None:
        for p, q in event.get("b", []):
            p, q = str(p), str(q)
            if q == "0" or Decimal(q) == 0:
                self.bids.pop(p, None)
            else:
                self.bids[p] = q
        for p, q in event.get("a", []):
            p, q = str(p), str(q)
            if q == "0" or Decimal(q) == 0:
                self.asks.pop(p, None)
            else:
                self.asks[p] = q
        self.update_id = event["u"]

    def snapshot(self, trigger_U: int, trigger_u: int) -> dict:
        sorted_bids = sorted(
            self.bids.items(), key=lambda x: Decimal(x[0]), reverse=True
        )
        sorted_asks = sorted(
            self.asks.items(), key=lambda x: Decimal(x[0])
        )
        best_bid = sorted_bids[0] if sorted_bids else (None, None)
        best_ask = sorted_asks[0] if sorted_asks else (None, None)
        bb_price, bb_qty = best_bid
        ba_price, ba_qty = best_ask
        spread = None
        if bb_price is not None and ba_price is not None:
            spread = str(Decimal(ba_price) - Decimal(bb_price))
        return {
            "update_id": self.update_id,
            "best_bid_price": bb_price,
            "best_bid_qty": bb_qty,
            "best_ask_price": ba_price,
            "best_ask_qty": ba_qty,
            "spread": spread,
            "bid_count": len(sorted_bids),
            "ask_count": len(sorted_asks),
            "bids": [[p, q] for p, q in sorted_bids],
            "asks": [[p, q] for p, q in sorted_asks],
            "trigger_event": {"U": trigger_U, "u": trigger_u},
        }


# ── REST snapshot ───────────────────────────────────────────────────────────

async def fetch_snapshot(
    session: aiohttp.ClientSession,
    market_start: int,
    f: LineWriter,
    seq: SeqCounter,
    reason: str,
    attempt: int,
) -> dict | None:
    try:
        async with session.get(BINANCE_DEPTH_SNAPSHOT_URL) as resp:
            if resp.status != 200:
                body = await resp.text()
                write_line(f, make_line(
                    market_start, "binance", "error", seq.next(),
                    {"status": resp.status, "body": body},
                    {"reason": "snapshot_fetch_failed", "attempt": attempt},
                ))
                return None
            data = await resp.json()
            write_line(f, make_line(
                market_start, "binance", "snapshot_raw", seq.next(),
                data,
                {
                    "lastUpdateId": data.get("lastUpdateId"),
                    "bid_levels": len(data.get("bids", [])),
                    "ask_levels": len(data.get("asks", [])),
                    "reason": reason,
                    "attempt": attempt,
                },
            ))
            return data
    except Exception as exc:
        write_line(f, make_line(
            market_start, "binance", "error", seq.next(),
            {"error": str(exc), "type": type(exc).__name__},
            {"reason": "snapshot_fetch_error", "attempt": attempt},
        ))
        return None


# ── Main collector ──────────────────────────────────────────────────────────

async def run_binance(
    market_start: int,
    f: LineWriter,
    stop_event: asyncio.Event,
) -> None:
    seq = SeqCounter()
    reconnect_backoff = WS_RECONNECT_BASE
    # Mutable container so _run_sync_loop can update it across reconnects
    strike_state = [False]

    while not stop_event.is_set():
        write_line(f, make_line(
            market_start, "binance", "system", seq.next(),
            None, {"url": BINANCE_WS_URL},
            system_type="ws_connecting",
        ))

        try:
            async for ws in connect(BINANCE_WS_URL, open_timeout=10):
                try:
                    write_line(f, make_line(
                        market_start, "binance", "system", seq.next(),
                        None, None,
                        system_type="ws_connected",
                    ))
                    reconnect_backoff = WS_RECONNECT_BASE

                    await _run_sync_loop(ws, market_start, f, seq, stop_event, strike_state)

                    if stop_event.is_set():
                        break

                except ConnectionClosed:
                    pass

                write_line(f, make_line(
                    market_start, "binance", "system", seq.next(),
                    None, None,
                    system_type="ws_disconnected",
                ))

                if stop_event.is_set():
                    break

                write_line(f, make_line(
                    market_start, "binance", "system", seq.next(),
                    None, {"backoff": reconnect_backoff},
                    system_type="ws_reconnecting",
                ))

        except Exception as exc:
            write_line(f, make_line(
                market_start, "binance", "error", seq.next(),
                {"error": str(exc), "type": type(exc).__name__},
                {"reason": "ws_connection_error"},
            ))

            if stop_event.is_set():
                break

            write_line(f, make_line(
                market_start, "binance", "system", seq.next(),
                None, {"backoff": reconnect_backoff},
                system_type="ws_reconnecting",
            ))
            await asyncio.sleep(reconnect_backoff)
            reconnect_backoff = min(reconnect_backoff * 2, WS_RECONNECT_MAX)


async def _run_sync_loop(
    ws: Any,
    market_start: int,
    f: LineWriter,
    seq: SeqCounter,
    stop_event: asyncio.Event,
    strike_state: list,
) -> None:
    """Run buffer → snapshot → sync → steady-state. Loops on gap (resync on same WS)."""

    async with aiohttp.ClientSession() as session:
        sync_cycle = 0

        while not stop_event.is_set():
            sync_cycle += 1
            book = BinanceBook()
            buffer: list[dict] = []
            synced = False
            snapshot_attempt = 0

            # ── Phase 1: Buffer + snapshot sync ─────────────────────────
            while not stop_event.is_set() and not synced:
                try:
                    raw_msg = await asyncio.wait_for(ws.recv(), timeout=5.0)
                except asyncio.TimeoutError:
                    continue
                except ConnectionClosed:
                    return

                try:
                    wrapper = orjson.loads(raw_msg)
                except (orjson.JSONDecodeError, ValueError):
                    continue

                stream = wrapper.get("stream", "")
                data = wrapper.get("data", {})

                if stream.endswith("@trade"):
                    write_line(f, make_line(
                        market_start, "binance", "trade_raw", seq.next(),
                        wrapper, None,
                    ))
                    m = data.get("m")
                    write_line(f, make_line(
                        market_start, "binance", "trade_normalized", seq.next(),
                        None, {
                            "price": data.get("p"),
                            "qty": data.get("q"),
                            "side": "SELL" if m else "BUY",
                            "trade_id": data.get("t"),
                            "trade_time": data.get("T"),
                            "event_time": data.get("E"),
                        },
                    ))
                elif "depth" in stream and "bookTicker" not in stream:
                    write_line(f, make_line(
                        market_start, "binance", "depth_raw_buffered", seq.next(),
                        wrapper,
                        {"U": data.get("U"), "u": data.get("u"), "buffer_size": len(buffer) + 1},
                    ))
                    buffer.append(data)

                    if len(buffer) == 1 or snapshot_attempt > 0:
                        snapshot_attempt += 1
                        reason = "startup" if sync_cycle == 1 else "resync_after_gap"
                        snap = await fetch_snapshot(
                            session, market_start, f, seq, reason, snapshot_attempt,
                        )
                        if snap is None:
                            continue

                        last_uid = snap["lastUpdateId"]

                        if last_uid < buffer[0]["U"]:
                            log.warning(
                                "Snapshot lastUpdateId %d < first buffered U %d, refetching",
                                last_uid, buffer[0]["U"],
                            )
                            continue

                        buffer = [e for e in buffer if e["u"] > last_uid]

                        if not buffer:
                            book.load_snapshot(snap)
                            write_line(f, make_line(
                                market_start, "binance", "snapshot_loaded", seq.next(),
                                None,
                                {"lastUpdateId": last_uid, "buffered_remaining": 0},
                            ))
                            write_line(f, make_line(
                                market_start, "binance", "book_state", seq.next(),
                                None, book.snapshot(0, last_uid),
                            ))
                            write_line(f, make_line(
                                market_start, "binance", "system", seq.next(),
                                None, {"lastUpdateId": last_uid},
                                system_type="sync_ready",
                            ))
                            if sync_cycle > 1:
                                write_line(f, make_line(
                                    market_start, "binance", "system", seq.next(),
                                    None, {"sync_cycle": sync_cycle, "lastUpdateId": last_uid},
                                    system_type="resync_completed",
                                ))
                            synced = True
                            continue

                        first = buffer[0]
                        if not (first["U"] <= last_uid + 1 <= first["u"]):
                            write_line(f, make_line(
                                market_start, "binance", "error", seq.next(),
                                {"first_U": first["U"], "first_u": first["u"], "lastUpdateId": last_uid},
                                {"reason": "first_event_does_not_span_snapshot"},
                            ))
                            buffer = []
                            continue

                        book.load_snapshot(snap)
                        write_line(f, make_line(
                            market_start, "binance", "snapshot_loaded", seq.next(),
                            None,
                            {"lastUpdateId": last_uid, "buffered_remaining": len(buffer)},
                        ))
                        write_line(f, make_line(
                            market_start, "binance", "book_state", seq.next(),
                            None, book.snapshot(0, last_uid),
                        ))

                        for evt in buffer:
                            book.apply_diff(evt)
                            write_line(f, make_line(
                                market_start, "binance", "depth_applied", seq.next(),
                                evt,
                                {"U": evt["U"], "u": evt["u"], "local_update_id": book.update_id},
                            ))
                            write_line(f, make_line(
                                market_start, "binance", "book_state", seq.next(),
                                None, book.snapshot(evt["U"], evt["u"]),
                            ))

                        buffer = []
                        write_line(f, make_line(
                            market_start, "binance", "system", seq.next(),
                            None, {"lastUpdateId": book.update_id},
                            system_type="sync_ready",
                        ))
                        if sync_cycle > 1:
                            write_line(f, make_line(
                                market_start, "binance", "system", seq.next(),
                                None, {"sync_cycle": sync_cycle, "lastUpdateId": book.update_id},
                                system_type="resync_completed",
                            ))
                        synced = True

                elif "bookTicker" in stream:
                    write_line(f, make_line(
                        market_start, "binance", "book_ticker_raw", seq.next(),
                        wrapper, None,
                    ))
                    write_line(f, make_line(
                        market_start, "binance", "book_ticker_normalized", seq.next(),
                        None, {
                            "bid_price": data.get("b"),
                            "bid_qty": data.get("B"),
                            "ask_price": data.get("a"),
                            "ask_qty": data.get("A"),
                            "update_id": data.get("u"),
                        },
                    ))

                    # Record strike price at market open
                    if not strike_state[0] and time.time() >= market_start:
                        bid = data.get("b", "0")
                        ask = data.get("a", "0")
                        mid = (Decimal(bid) + Decimal(ask)) / 2
                        write_line(f, make_line(
                            market_start, "binance", "system", seq.next(),
                            None, {
                                "strike_price": str(mid),
                                "strike_bid": bid,
                                "strike_ask": ask,
                                "market_start": market_start,
                            },
                            system_type="strike_price",
                        ))
                        strike_state[0] = True
                        log.info(
                            "Strike price for market %d: %s (bid=%s ask=%s)",
                            market_start, mid, bid, ask,
                        )

            # ── Phase 2: Steady state ───────────────────────────────────
            gap_hit = False
            while not stop_event.is_set() and synced and not gap_hit:
                try:
                    raw_msg = await asyncio.wait_for(ws.recv(), timeout=5.0)
                except asyncio.TimeoutError:
                    continue
                except ConnectionClosed:
                    return

                try:
                    wrapper = orjson.loads(raw_msg)
                except (orjson.JSONDecodeError, ValueError):
                    continue

                stream = wrapper.get("stream", "")
                data = wrapper.get("data", {})

                if stream.endswith("@trade"):
                    write_line(f, make_line(
                        market_start, "binance", "trade_raw", seq.next(),
                        wrapper, None,
                    ))
                    m = data.get("m")
                    write_line(f, make_line(
                        market_start, "binance", "trade_normalized", seq.next(),
                        None, {
                            "price": data.get("p"),
                            "qty": data.get("q"),
                            "side": "SELL" if m else "BUY",
                            "trade_id": data.get("t"),
                            "trade_time": data.get("T"),
                            "event_time": data.get("E"),
                        },
                    ))
                elif "bookTicker" in stream:
                    write_line(f, make_line(
                        market_start, "binance", "book_ticker_raw", seq.next(),
                        wrapper, None,
                    ))
                    normalized_ticker = {
                        "bid_price": data.get("b"),
                        "bid_qty": data.get("B"),
                        "ask_price": data.get("a"),
                        "ask_qty": data.get("A"),
                        "update_id": data.get("u"),
                    }
                    if book.update_id > 0:
                        bsnap = book.snapshot(0, 0)
                        normalized_ticker["local_best_bid"] = bsnap["best_bid_price"]
                        normalized_ticker["local_best_ask"] = bsnap["best_ask_price"]
                        normalized_ticker["consistent"] = (
                            bsnap["best_bid_price"] == data.get("b")
                            and bsnap["best_ask_price"] == data.get("a")
                        )
                    write_line(f, make_line(
                        market_start, "binance", "book_ticker_normalized", seq.next(),
                        None, normalized_ticker,
                    ))

                    # Record strike price at market open
                    if not strike_state[0] and time.time() >= market_start:
                        bid = data.get("b", "0")
                        ask = data.get("a", "0")
                        mid = (Decimal(bid) + Decimal(ask)) / 2
                        write_line(f, make_line(
                            market_start, "binance", "system", seq.next(),
                            None, {
                                "strike_price": str(mid),
                                "strike_bid": bid,
                                "strike_ask": ask,
                                "market_start": market_start,
                            },
                            system_type="strike_price",
                        ))
                        strike_state[0] = True
                        log.info(
                            "Strike price for market %d: %s (bid=%s ask=%s)",
                            market_start, mid, bid, ask,
                        )

                elif "depth" in stream:
                    evt_U = data.get("U", 0)
                    evt_u = data.get("u", 0)

                    write_line(f, make_line(
                        market_start, "binance", "depth_raw", seq.next(),
                        wrapper,
                        {"U": evt_U, "u": evt_u, "local_update_id": book.update_id},
                    ))

                    if evt_U > book.update_id + 1:
                        write_line(f, make_line(
                            market_start, "binance", "system", seq.next(),
                            None, {
                                "last_good_update_id": book.update_id,
                                "incoming_U": evt_U,
                                "incoming_u": evt_u,
                            },
                            system_type="gap_detected",
                        ))
                        write_line(f, make_line(
                            market_start, "binance", "system", seq.next(),
                            None, {"reason": "gap_in_update_ids"},
                            system_type="resync_started",
                        ))
                        gap_hit = True
                        continue

                    if evt_u <= book.update_id:
                        continue

                    book.apply_diff(data)
                    write_line(f, make_line(
                        market_start, "binance", "depth_applied", seq.next(),
                        None,
                        {"U": evt_U, "u": evt_u, "local_update_id": book.update_id},
                    ))
                    write_line(f, make_line(
                        market_start, "binance", "book_state", seq.next(),
                        None, book.snapshot(evt_U, evt_u),
                    ))

            # If we exited steady-state without gap, we're done (stop_event or WS closed)
            if not gap_hit:
                return
            # Otherwise loop back to Phase 1 for resync
