import { createReadStream, statSync, existsSync, mkdirSync, writeFileSync, readFileSync as fsReadFileSync } from 'fs';
import { createInterface } from 'readline';
import path from 'path';
import type { Frame, MarketMeta } from './types';
import { FairValueAccumulator, CACHE_VERSION, PARAMS } from './fair-value';
import { BookStrikeFairValueAccumulator } from './book-strike-fair-value';

const DATA_DIR = path.resolve(process.cwd(), '..', 'data');

interface ClobEntry {
  ts: number;
  assetId: string;
  bestBid: number;
  bestAsk: number;
  lastTrade: number | null;
  bids: [string, string][];
  asks: [string, string][];
}

interface BtcTickerEntry {
  ts: number;
  bid: number;
  ask: number;
  bidQty: number;
  askQty: number;
}

interface BtcDepthEntry {
  ts: number;
  bids: [number, number][];
  asks: [number, number][];
}

interface BtcTradeEntry {
  ts: number;
  qty: number;
  side: string;
}

interface MarketLookup {
  marketId: string;
  slug: string;
  question: string;
  outcomes: string[];
  clobTokenIds: string[];
  endDate: string;
  gameStartTime: string | null;
}

async function readFileLines(
  filePath: string,
  onLine: (line: string, bytesRead: number) => void,
  onProgress?: (pct: number, msg: string) => void | Promise<void>,
  label?: string,
  pctOffset?: number,
  pctRange?: number,
): Promise<void> {
  const totalBytes = statSync(filePath).size;
  const offset = pctOffset ?? 0;
  const range = pctRange ?? 100;

  let bytesRead = 0;
  let lastPct = -1;
  let lineCount = 0;

  const stream = createReadStream(filePath, { encoding: 'utf-8', highWaterMark: 256 * 1024 });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });

  for await (const line of rl) {
    // +1 for the newline character
    bytesRead += Buffer.byteLength(line, 'utf-8') + 1;
    onLine(line, bytesRead);
    lineCount++;

    // Yield to event loop every 2000 lines so server stays responsive
    if (lineCount % 2000 === 0) {
      await new Promise<void>((r) => setTimeout(r, 0));
    }

    if (onProgress) {
      const pct = Math.floor(offset + (bytesRead / totalBytes) * range);
      if (pct !== lastPct) {
        lastPct = pct;
        await onProgress(Math.min(pct, offset + range), `Reading ${label ?? filePath}...`);
      }
    }
  }
}

export async function processMarket(
  epoch: string,
  onProgress?: (pct: number, msg: string) => void | Promise<void>,
): Promise<{ meta: MarketMeta; frames: Frame[] }> {
  const epochDir = path.join(DATA_DIR, epoch);
  const cacheDir = path.join(epochDir, '.cache');

  // Check cache first
  const metaPath = path.join(cacheDir, 'meta.json');
  const framesPath = path.join(cacheDir, 'frames.json');
  if (existsSync(metaPath) && existsSync(framesPath)) {
    const cachedMeta = JSON.parse(readCachedFile(metaPath));
    if (cachedMeta.cacheVersion === CACHE_VERSION) {
      await onProgress?.(100, 'Loaded from cache');
      return { meta: cachedMeta, frames: JSON.parse(readCachedFile(framesPath)) };
    }
  }

  const clobPath = path.join(epochDir, 'clob.jsonl');
  const depthPath = path.join(epochDir, 'depth-trade.jsonl');

  if (!existsSync(clobPath) || !existsSync(depthPath)) {
    throw new Error(`Missing data files for epoch ${epoch}`);
  }

  // Pass 1: Read clob.jsonl — extract market_lookup + book_state entries
  let lookup: MarketLookup | null = null;
  let marketStart = 0;
  const clobEntries: ClobEntry[] = [];

  await onProgress?.(0, 'Reading clob.jsonl...');

  await readFileLines(
    clobPath,
    (line) => {
      if (!line.trim()) return;
      let parsed: Record<string, unknown>;
      try {
        parsed = JSON.parse(line);
      } catch {
        return;
      }

      if (!marketStart && parsed.market_start) {
        marketStart = parsed.market_start as number;
      }

      const eventType = parsed.event_type as string;
      const normalized = parsed.normalized as Record<string, unknown> | null;
      if (!normalized) return;

      if (eventType === 'market_lookup' && !lookup) {
        lookup = {
          marketId: normalized.market_id as string,
          slug: normalized.slug as string,
          question: normalized.question as string,
          outcomes: normalized.outcomes as string[],
          clobTokenIds: normalized.clob_token_ids as string[],
          endDate: normalized.end_date as string,
          gameStartTime: (normalized.game_start_time as string) ?? null,
        };
      } else if (eventType === 'book_state') {
        const bids = normalized.bids as [string, string][];
        const asks = normalized.asks as [string, string][];
        clobEntries.push({
          ts: parsed.ts_recv_ms as number,
          assetId: normalized.asset_id as string,
          bestBid: parseFloat(normalized.best_bid as string),
          bestAsk: parseFloat(normalized.best_ask as string),
          lastTrade: normalized.last_trade_price
            ? parseFloat(normalized.last_trade_price as string)
            : null,
          bids: bids ? bids.slice(0, 100) : [],
          asks: asks ? asks.slice(0, 100) : [],
        });
      }
    },
    onProgress,
    'clob.jsonl',
    0,
    45,
  );

  if (!lookup) {
    throw new Error(`No market_lookup event found in epoch ${epoch}`);
  }

  // Reassign to a const so TypeScript keeps the narrowed type after async ops
  const marketLookup: MarketLookup = lookup;

  // Pass 2: Read depth-trade.jsonl — extract book_ticker_normalized + book_state + trade_normalized
  const btcTickers: BtcTickerEntry[] = [];
  const btcDepths: BtcDepthEntry[] = [];
  const btcTrades: BtcTradeEntry[] = [];
  let explicitStrikePrice: number | null = null;

  await onProgress?.(45, 'Reading depth-trade.jsonl...');

  await readFileLines(
    depthPath,
    (line) => {
      if (!line.trim()) return;
      let parsed: Record<string, unknown>;
      try {
        parsed = JSON.parse(line);
      } catch {
        return;
      }

      const eventType = parsed.event_type as string;
      const normalized = parsed.normalized as Record<string, unknown> | null;

      // Check for explicit strike_price system event
      if (eventType === 'system' && parsed.system_type === 'strike_price' && normalized) {
        explicitStrikePrice = parseFloat(normalized.strike_price as string);
      }

      if (!normalized) return;

      if (eventType === 'book_ticker_normalized') {
        btcTickers.push({
          ts: parsed.ts_recv_ms as number,
          bid: parseFloat(normalized.bid_price as string),
          ask: parseFloat(normalized.ask_price as string),
          bidQty: parseFloat(normalized.bid_qty as string),
          askQty: parseFloat(normalized.ask_qty as string),
        });
      } else if (eventType === 'book_state') {
        const rawBids = normalized.bids as [string, string][];
        const rawAsks = normalized.asks as [string, string][];
        btcDepths.push({
          ts: parsed.ts_recv_ms as number,
          bids: rawBids
            ? rawBids.slice(0, 2000).map(([p, q]) => [parseFloat(p), parseFloat(q)] as [number, number])
            : [],
          asks: rawAsks
            ? rawAsks.slice(0, 2000).map(([p, q]) => [parseFloat(p), parseFloat(q)] as [number, number])
            : [],
        });
      } else if (eventType === 'trade_normalized') {
        btcTrades.push({
          ts: parsed.ts_recv_ms as number,
          qty: parseFloat(normalized.qty as string),
          side: normalized.side as string,
        });
      }
    },
    onProgress,
    'depth-trade.jsonl',
    45,
    45,
  );

  await onProgress?.(90, 'Generating frames...');

  // Resolve strike price early so fair value can use it during frame gen
  let resolvedStrikePrice: number | null = explicitStrikePrice;
  if (resolvedStrikePrice === null && btcTickers.length > 0) {
    const marketStartMs = marketStart * 1000;
    const idx = btcTickers.findIndex(t => t.ts >= marketStartMs);
    if (idx >= 0) {
      resolvedStrikePrice = (btcTickers[idx].bid + btcTickers[idx].ask) / 2;
    } else {
      const last = btcTickers[btcTickers.length - 1];
      resolvedStrikePrice = (last.bid + last.ask) / 2;
    }
  }

  // Identify Up/Down token IDs
  const upTokenId = marketLookup.clobTokenIds[0];
  const downTokenId = marketLookup.clobTokenIds[1];

  // Separate clob entries by asset
  const upEntries = clobEntries.filter((e) => e.assetId === upTokenId);
  const downEntries = clobEntries.filter((e) => e.assetId === downTokenId);

  // Sort all arrays by timestamp
  upEntries.sort((a, b) => a.ts - b.ts);
  downEntries.sort((a, b) => a.ts - b.ts);
  btcTickers.sort((a, b) => a.ts - b.ts);
  btcDepths.sort((a, b) => a.ts - b.ts);
  btcTrades.sort((a, b) => a.ts - b.ts);

  // Determine time range
  const allTimestamps: number[] = [];
  if (upEntries.length) allTimestamps.push(upEntries[0].ts, upEntries[upEntries.length - 1].ts);
  if (downEntries.length) allTimestamps.push(downEntries[0].ts, downEntries[downEntries.length - 1].ts);
  if (btcTickers.length) allTimestamps.push(btcTickers[0].ts, btcTickers[btcTickers.length - 1].ts);
  if (btcDepths.length) allTimestamps.push(btcDepths[0].ts, btcDepths[btcDepths.length - 1].ts);

  if (allTimestamps.length === 0) {
    throw new Error(`No data events found for epoch ${epoch}`);
  }

  const globalStart = Math.min(...allTimestamps);
  const globalEnd = Math.max(...allTimestamps);
  const INTERVAL_MS = 100;

  // Pointer-based frame generation
  let upPtr = 0;
  let downPtr = 0;
  let btcTickPtr = 0;
  let btcDepthPtr = 0;
  let btcTradePtr = 0;

  const frames: Frame[] = [];
  const accumulator = new FairValueAccumulator();
  const bookStrikeFvAccumulator = new BookStrikeFairValueAccumulator();
  const expiryMs = marketStart * 1000 + PARAMS.marketDurationSec * 1000;

  // Default values
  let latestUp: ClobEntry | null = null;
  let latestDown: ClobEntry | null = null;
  let latestBtcTick: BtcTickerEntry | null = null;
  let latestBtcDepth: BtcDepthEntry | null = null;

  for (let ts = globalStart; ts <= globalEnd; ts += INTERVAL_MS) {
    // Advance pointers to latest event <= ts
    while (upPtr < upEntries.length && upEntries[upPtr].ts <= ts) {
      latestUp = upEntries[upPtr];
      upPtr++;
    }
    while (downPtr < downEntries.length && downEntries[downPtr].ts <= ts) {
      latestDown = downEntries[downPtr];
      downPtr++;
    }
    while (btcTickPtr < btcTickers.length && btcTickers[btcTickPtr].ts <= ts) {
      const t = btcTickers[btcTickPtr];
      latestBtcTick = t;
      accumulator.onBookTicker(t.ts, t.bid, t.bidQty, t.ask, t.askQty);
      btcTickPtr++;
    }
    while (btcDepthPtr < btcDepths.length && btcDepths[btcDepthPtr].ts <= ts) {
      const d = btcDepths[btcDepthPtr];
      latestBtcDepth = d;
      accumulator.onDepthSnapshot(d.ts, d.bids, d.asks);
      bookStrikeFvAccumulator.onDepthSnapshot(d.ts, d.bids, d.asks);
      btcDepthPtr++;
    }
    while (btcTradePtr < btcTrades.length && btcTrades[btcTradePtr].ts <= ts) {
      const tr = btcTrades[btcTradePtr];
      accumulator.onTrade(tr.ts, tr.qty, tr.side);
      btcTradePtr++;
    }

    // Only emit frames once we have at least BTC data
    if (!latestBtcTick) continue;

    const btcMid = (latestBtcTick.bid + latestBtcTick.ask) / 2;

    const frame: Frame = {
      t: ts - (marketStart * 1000),
      btc: {
        bid: latestBtcTick.bid,
        ask: latestBtcTick.ask,
        mid: btcMid,
      },
      up: {
        bestBid: latestUp?.bestBid ?? 0,
        bestAsk: latestUp?.bestAsk ?? 0,
        lastTrade: latestUp?.lastTrade ?? null,
      },
      down: {
        bestBid: latestDown?.bestBid ?? 0,
        bestAsk: latestDown?.bestAsk ?? 0,
        lastTrade: latestDown?.lastTrade ?? null,
      },
      btcDepth: {
        bids: (latestBtcDepth?.bids ?? []).slice(0, 200),
        asks: (latestBtcDepth?.asks ?? []).slice(0, 200),
      },
      polyUpDepth: {
        bids: latestUp?.bids ?? [],
        asks: latestUp?.asks ?? [],
      },
      polyDownDepth: {
        bids: latestDown?.bids ?? [],
        asks: latestDown?.asks ?? [],
      },
      fairValue: resolvedStrikePrice != null
        ? accumulator.sample(ts, resolvedStrikePrice, expiryMs)
        : null,
      bookFairValue: resolvedStrikePrice != null
        ? bookStrikeFvAccumulator.sample(resolvedStrikePrice)
        : null,
    };

    frames.push(frame);
  }

  await onProgress?.(95, 'Building metadata...');

  const strikePrice = resolvedStrikePrice;

  const meta: MarketMeta = {
    epoch: parseInt(epoch, 10),
    slug: marketLookup.slug,
    question: marketLookup.question,
    outcomes: marketLookup.outcomes,
    clobTokenIds: marketLookup.clobTokenIds,
    upTokenId,
    downTokenId,
    startTime: globalStart,
    endTime: globalEnd,
    marketStart: marketStart * 1000,
    totalFrames: frames.length,
    durationMs: globalEnd - globalStart,
    strikePrice,
    cacheVersion: CACHE_VERSION,
  };

  // Write cache
  await onProgress?.(97, 'Writing cache...');
  if (!existsSync(cacheDir)) {
    mkdirSync(cacheDir, { recursive: true });
  }
  writeFileSync(metaPath, JSON.stringify(meta));
  writeFileSync(framesPath, JSON.stringify(frames));

  await onProgress?.(100, 'Done');

  return { meta, frames };
}

function readCachedFile(filePath: string): string {
  return fsReadFileSync(filePath, 'utf-8');
}
