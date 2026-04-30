"""Polymarket CLOB collector: Gamma API lookup + WebSocket book streaming."""

import asyncio
import json
import logging
import time
from typing import Any

import aiohttp
import orjson
from websockets.asyncio.client import connect
from websockets.exceptions import ConnectionClosed

from .config import (
    GAMMA_LOOKUP_DEADLINE,
    GAMMA_MARKET_BY_SLUG,
    GAMMA_RETRY_BASE,
    GAMMA_RETRY_MAX,
    POLY_WS_CONNECTIONS,
    POLYMARKET_WS_URL,
    WS_RECONNECT_BASE,
    WS_RECONNECT_MAX,
)
from .models import SeqCounter, LineWriter, make_line, write_line

log = logging.getLogger(__name__)


# ── Local order book ────────────────────────────────────────────────────────

class LocalBook:
    """Price-level order book for one Polymarket asset."""

    def __init__(self) -> None:
        self.bids: dict[str, str] = {}  # price → size
        self.asks: dict[str, str] = {}  # price → size

    def replace(self, bids: list, asks: list) -> None:
        self.bids = {}
        for entry in bids:
            if isinstance(entry, dict):
                self.bids[str(entry["price"])] = str(entry["size"])
            else:
                self.bids[str(entry[0])] = str(entry[1])
        self.asks = {}
        for entry in asks:
            if isinstance(entry, dict):
                self.asks[str(entry["price"])] = str(entry["size"])
            else:
                self.asks[str(entry[0])] = str(entry[1])

    def apply_delta(self, side: str, price: str, size: str) -> None:
        book = self.bids if side == "BUY" else self.asks
        price = str(price)
        size = str(size)
        if size == "0":
            book.pop(price, None)
        else:
            book[price] = size

    def snapshot(self) -> dict:
        sorted_bids = sorted(self.bids.items(), key=lambda x: float(x[0]), reverse=True)
        sorted_asks = sorted(self.asks.items(), key=lambda x: float(x[0]))
        best_bid = sorted_bids[0][0] if sorted_bids else None
        best_ask = sorted_asks[0][0] if sorted_asks else None
        return {
            "bid_count": len(sorted_bids),
            "ask_count": len(sorted_asks),
            "bids": [[p, s] for p, s in sorted_bids],
            "asks": [[p, s] for p, s in sorted_asks],
            "best_bid": best_bid,
            "best_ask": best_ask,
        }


# ── Gamma API ───────────────────────────────────────────────────────────────

async def fetch_gamma_market(
    session: aiohttp.ClientSession,
    slug: str,
    market_start: int,
    f: LineWriter,
    seq: SeqCounter,
) -> dict | None:
    """Fetch market by slug with retry+backoff. Returns parsed market or None."""
    deadline = market_start + GAMMA_LOOKUP_DEADLINE
    backoff = GAMMA_RETRY_BASE
    attempt = 0

    while time.time() < deadline:
        attempt += 1
        url = GAMMA_MARKET_BY_SLUG.format(slug=slug)
        try:
            async with session.get(url) as resp:
                if resp.status == 200:
                    data = await resp.json()
                    return data
                body = await resp.text()
                write_line(f, make_line(
                    market_start, "polymarket", "market_lookup_retry", seq.next(),
                    {"status": resp.status, "body": body, "attempt": attempt},
                    {"slug": slug, "attempt": attempt, "status": resp.status},
                ))
        except Exception as exc:
            write_line(f, make_line(
                market_start, "polymarket", "market_lookup_retry", seq.next(),
                {"error": str(exc), "attempt": attempt},
                {"slug": slug, "attempt": attempt, "error": str(exc)},
            ))

        await asyncio.sleep(backoff)
        backoff = min(backoff * 2, GAMMA_RETRY_MAX)

    return None


def validate_market(data: dict, slug: str) -> str | None:
    """Return error string if invalid, None if OK."""
    if not data:
        return "empty response"
    if data.get("slug") != slug:
        return f"slug mismatch: expected {slug}, got {data.get('slug')}"
    if data.get("closed") is True:
        return "market is closed"
    if not data.get("enableOrderBook"):
        return "order book not enabled"
    outcomes = data.get("outcomes")
    if not outcomes:
        outcomes_str = data.get("outcomes", "")
        if isinstance(outcomes_str, str):
            try:
                outcomes = json.loads(outcomes_str)
            except (json.JSONDecodeError, TypeError):
                return f"cannot parse outcomes: {outcomes_str}"
    if isinstance(outcomes, list) and len(outcomes) != 2:
        return f"expected 2 outcomes, got {len(outcomes)}"
    clob_ids = data.get("clobTokenIds")
    if not clob_ids:
        clob_ids_str = data.get("clobTokenIds", "")
        if isinstance(clob_ids_str, str):
            try:
                clob_ids = json.loads(clob_ids_str)
            except (json.JSONDecodeError, TypeError):
                return f"cannot parse clobTokenIds: {clob_ids_str}"
    if isinstance(clob_ids, list) and len(clob_ids) != 2:
        return f"expected 2 clobTokenIds, got {len(clob_ids)}"
    return None


def parse_market_meta(data: dict) -> dict:
    """Extract metadata from Gamma response."""
    outcomes = data.get("outcomes")
    if isinstance(outcomes, str):
        outcomes = json.loads(outcomes)
    clob_ids = data.get("clobTokenIds")
    if isinstance(clob_ids, str):
        clob_ids = json.loads(clob_ids)
    return {
        "market_id": data.get("id"),
        "condition_id": data.get("conditionId"),
        "slug": data.get("slug"),
        "question": data.get("question"),
        "outcomes": outcomes,
        "clob_token_ids": clob_ids,
        "end_date": data.get("endDate"),
        "game_start_time": data.get("gameStartTime"),
    }


# ── Dedup ───────────────────────────────────────────────────────────────────

def _dedup_key(msg: dict) -> bytes:
    return orjson.dumps(msg, option=orjson.OPT_SORT_KEYS)


# ── WebSocket collector ─────────────────────────────────────────────────────

async def run_polymarket(
    market_start: int,
    f: LineWriter,
    stop_event: asyncio.Event,
) -> None:
    seq = SeqCounter()
    slug = f"btc-updown-5m-{market_start}"

    # ── Gamma lookup ────────────────────────────────────────────────────
    async with aiohttp.ClientSession() as session:
        raw_market = await fetch_gamma_market(session, slug, market_start, f, seq)

    if raw_market is None:
        write_line(f, make_line(
            market_start, "polymarket", "market_lookup_failed", seq.next(),
            {"slug": slug}, {"slug": slug, "reason": "deadline exceeded"},
        ))
        return

    err = validate_market(raw_market, slug)
    if err is not None:
        write_line(f, make_line(
            market_start, "polymarket", "market_lookup_failed", seq.next(),
            raw_market, {"slug": slug, "reason": err},
        ))
        return

    meta = parse_market_meta(raw_market)
    token_ids = meta["clob_token_ids"]

    write_line(f, make_line(
        market_start, "polymarket", "market_lookup", seq.next(),
        raw_market, meta,
    ))

    # ── Shared state across all connections ──────────────────────────────
    books: dict[str, LocalBook] = {tid: LocalBook() for tid in token_ids}
    last_trade: dict[str, str] = {}
    seen: set[str] = set()

    # ── Launch redundant WS connections ─────────────────────────────────
    await asyncio.gather(
        *[
            _ws_connection_loop(
                conn_id=i,
                market_start=market_start,
                f=f,
                seq=seq,
                stop_event=stop_event,
                books=books,
                last_trade=last_trade,
                token_ids=token_ids,
                seen=seen,
            )
            for i in range(POLY_WS_CONNECTIONS)
        ],
        return_exceptions=True,
    )


async def _ws_connection_loop(
    conn_id: int,
    market_start: int,
    f: LineWriter,
    seq: SeqCounter,
    stop_event: asyncio.Event,
    books: dict[str, LocalBook],
    last_trade: dict[str, str],
    token_ids: list[str],
    seen: set[str],
) -> None:
    reconnect_backoff = WS_RECONNECT_BASE
    msgs_received = 0
    msgs_deduped = 0

    while not stop_event.is_set():
        write_line(f, make_line(
            market_start, "polymarket", "system", seq.next(),
            None, {"url": POLYMARKET_WS_URL, "conn_id": conn_id},
            system_type="ws_connecting",
        ))

        try:
            async for ws in connect(POLYMARKET_WS_URL, open_timeout=10):
                try:
                    write_line(f, make_line(
                        market_start, "polymarket", "system", seq.next(),
                        None, {"conn_id": conn_id},
                        system_type="ws_connected",
                    ))
                    reconnect_backoff = WS_RECONNECT_BASE

                    sub_payload = {
                        "assets_ids": token_ids,
                        "type": "market",
                        "custom_feature_enabled": True,
                    }
                    await ws.send(json.dumps(sub_payload))
                    write_line(f, make_line(
                        market_start, "polymarket", "system", seq.next(),
                        sub_payload, {"token_ids": token_ids, "conn_id": conn_id},
                        system_type="ws_subscribed",
                    ))

                    while not stop_event.is_set():
                        try:
                            raw_msg = await asyncio.wait_for(ws.recv(), timeout=5.0)
                        except asyncio.TimeoutError:
                            continue
                        except ConnectionClosed:
                            break

                        try:
                            msgs = orjson.loads(raw_msg)
                        except (orjson.JSONDecodeError, ValueError):
                            write_line(f, make_line(
                                market_start, "polymarket", "error", seq.next(),
                                {"raw": raw_msg, "conn_id": conn_id},
                                {"reason": "json_decode_error", "conn_id": conn_id},
                            ))
                            continue

                        if not isinstance(msgs, list):
                            msgs = [msgs]

                        for msg in msgs:
                            msgs_received += 1
                            key = _dedup_key(msg)
                            if key in seen:
                                msgs_deduped += 1
                                continue
                            seen.add(key)
                            _process_poly_message(
                                msg, market_start, f, seq,
                                books, last_trade, token_ids,
                            )

                    if stop_event.is_set():
                        break

                except ConnectionClosed:
                    pass

                write_line(f, make_line(
                    market_start, "polymarket", "system", seq.next(),
                    None, {
                        "conn_id": conn_id,
                        "msgs_received": msgs_received,
                        "msgs_deduped": msgs_deduped,
                    },
                    system_type="ws_disconnected",
                ))

                if stop_event.is_set():
                    break

                write_line(f, make_line(
                    market_start, "polymarket", "system", seq.next(),
                    None, {"backoff": reconnect_backoff, "conn_id": conn_id},
                    system_type="ws_reconnecting",
                ))

        except Exception as exc:
            write_line(f, make_line(
                market_start, "polymarket", "error", seq.next(),
                {"error": str(exc), "type": type(exc).__name__, "conn_id": conn_id},
                {"reason": "ws_connection_error", "conn_id": conn_id},
            ))

            if stop_event.is_set():
                break

            write_line(f, make_line(
                market_start, "polymarket", "system", seq.next(),
                None, {"backoff": reconnect_backoff, "conn_id": conn_id},
                system_type="ws_reconnecting",
            ))
            await asyncio.sleep(reconnect_backoff)
            reconnect_backoff = min(reconnect_backoff * 2, WS_RECONNECT_MAX)


def _process_poly_message(
    msg: dict,
    market_start: int,
    f: LineWriter,
    seq: SeqCounter,
    books: dict[str, LocalBook],
    last_trade: dict[str, str],
    token_ids: list[str],
) -> None:
    """Process one Polymarket WS message and write all relevant lines."""
    event_type = msg.get("event_type", "unknown")
    asset_id = msg.get("asset_id", "")

    if event_type == "book":
        write_line(f, make_line(
            market_start, "polymarket", "book_raw", seq.next(),
            msg, {"asset_id": asset_id, "event_type": event_type},
        ))
        # Capture last_trade_price from book event if present
        ltp = msg.get("last_trade_price")
        if ltp is not None and asset_id:
            last_trade[asset_id] = str(ltp)
        if asset_id in books:
            book = books[asset_id]
            bids = msg.get("bids", [])
            asks = msg.get("asks", [])
            book.replace(bids, asks)
            snap = book.snapshot()
            snap["asset_id"] = asset_id
            snap["last_trade_price"] = last_trade.get(asset_id)
            snap["trigger_event_type"] = "book"
            write_line(f, make_line(
                market_start, "polymarket", "book_state", seq.next(),
                None, snap,
            ))

    elif event_type == "price_change":
        write_line(f, make_line(
            market_start, "polymarket", "price_change_raw", seq.next(),
            msg, {"asset_id": asset_id, "event_type": event_type},
        ))
        changes = msg.get("price_changes") or msg.get("changes") or []
        # Group changes by asset_id and apply
        affected_assets: set[str] = set()
        for change in changes:
            ch_asset = str(change.get("asset_id", asset_id))
            side = change.get("side", "")
            price = str(change.get("price", ""))
            size = str(change.get("size", ""))
            if ch_asset in books:
                books[ch_asset].apply_delta(side, price, size)
                affected_assets.add(ch_asset)
        for aid in affected_assets:
            write_line(f, make_line(
                market_start, "polymarket", "price_change_applied", seq.next(),
                None, {"asset_id": aid, "changes_count": len(changes)},
            ))
            snap = books[aid].snapshot()
            snap["asset_id"] = aid
            snap["last_trade_price"] = last_trade.get(aid)
            snap["trigger_event_type"] = "price_change"
            write_line(f, make_line(
                market_start, "polymarket", "book_state", seq.next(),
                None, snap,
            ))

    elif event_type == "last_trade_price":
        last_trade[asset_id] = str(msg.get("price", ""))
        write_line(f, make_line(
            market_start, "polymarket", "last_trade_price_raw", seq.next(),
            msg, {"asset_id": asset_id, "price": last_trade[asset_id]},
        ))

    elif event_type == "best_bid_ask":
        write_line(f, make_line(
            market_start, "polymarket", "best_bid_ask_raw", seq.next(),
            msg, {"asset_id": asset_id},
        ))

    elif event_type == "tick_size_change":
        write_line(f, make_line(
            market_start, "polymarket", "tick_size_change_raw", seq.next(),
            msg, {"asset_id": asset_id},
        ))

    elif event_type == "market_resolved":
        write_line(f, make_line(
            market_start, "polymarket", "market_resolved_raw", seq.next(),
            msg, {"asset_id": asset_id},
        ))

    else:
        write_line(f, make_line(
            market_start, "polymarket", event_type + "_raw", seq.next(),
            msg, {"asset_id": asset_id, "event_type": event_type},
        ))
