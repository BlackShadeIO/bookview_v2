// Worker script: processes a market in a separate process
// Usage: node scripts/process-worker.mjs <epoch>
// Outputs progress as JSON lines to stdout

import { createReadStream, statSync, existsSync, mkdirSync, writeFileSync, readFileSync } from 'fs';
import { createInterface } from 'readline';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DATA_DIR = path.resolve(__dirname, '..', '..', 'data');

const CACHE_VERSION = 8;

const PARAMS = {
  micropriceWeight: 0.7,
  depthMicropriceWeight: 0.3,
  depthLevels: 5,
  bookImbalanceLevels: 10,
  tradeHalflifeMs: 2500,
  driftCoeff: 0.1,
  volFloorPerSec: 1e-8,
  marketDurationSec: 300,
};

function normalCDF(x) {
  if (x < -8) return 0;
  if (x > 8) return 1;
  const a1 = 0.254829592, a2 = -0.284496736, a3 = 1.421413741;
  const a4 = -1.453152027, a5 = 1.061405429, p = 0.3275911;
  const sign = x < 0 ? -1 : 1;
  const absX = Math.abs(x);
  const t = 1.0 / (1.0 + p * absX);
  const y = 1.0 - ((((a5 * t + a4) * t + a3) * t + a2) * t + a1) * t * Math.exp(-absX * absX / 2);
  return 0.5 * (1.0 + sign * y);
}

class FairValueAccumulator {
  constructor() {
    this.bidPrice = 0; this.bidQty = 0;
    this.askPrice = 0; this.askQty = 0;
    this.microprice = 0;
    this.depthMicroprice = 0;
    this.depthStale = false;
    this.depthBestBid = 0; this.depthBestAsk = 0;
    this.bookImbalance = 0.5;
    this.tradeImbalanceEwma = 0; this.lastTradeTs = 0;
    this.prevMid = 0; this.prevMidTs = 0;
    this.volEwma1s = PARAMS.volFloorPerSec; this.volEwma5s = PARAMS.volFloorPerSec; this.volEwma15s = PARAMS.volFloorPerSec;
    this.ready = false;
  }

  onBookTicker(ts, bidPrice, bidQty, askPrice, askQty) {
    this.bidPrice = bidPrice; this.bidQty = bidQty;
    this.askPrice = askPrice; this.askQty = askQty;
    const totalQty = bidQty + askQty;
    this.microprice = totalQty > 0
      ? (bidPrice * askQty + askPrice * bidQty) / totalQty
      : (bidPrice + askPrice) / 2;

    if (this.depthBestBid > 0 && this.depthBestAsk > 0) {
      this.depthStale = bidPrice < this.depthBestBid || askPrice > this.depthBestAsk ||
        bidPrice > this.depthBestAsk || askPrice < this.depthBestBid;
    }

    const mid = (bidPrice + askPrice) / 2;
    if (this.prevMid > 0 && this.prevMidTs > 0) {
      const dt = (ts - this.prevMidTs) / 1000;
      if (dt > 0 && dt < 5) {
        const ret = Math.log(mid / this.prevMid);
        const instantVar = (ret * ret) / dt;
        const alpha1 = 1 - Math.exp(-dt / 1.0);
        const alpha5 = 1 - Math.exp(-dt / 5.0);
        const alpha15 = 1 - Math.exp(-dt / 15.0);
        this.volEwma1s = this.volEwma1s * (1 - alpha1) + instantVar * alpha1;
        this.volEwma5s = this.volEwma5s * (1 - alpha5) + instantVar * alpha5;
        this.volEwma15s = this.volEwma15s * (1 - alpha15) + instantVar * alpha15;
      }
    }
    this.prevMid = mid; this.prevMidTs = ts;
    this.ready = true;
  }

  onTrade(ts, qty, side) {
    const signed = side === 'BUY' ? qty : -qty;
    if (this.lastTradeTs > 0) {
      const dt = ts - this.lastTradeTs;
      const decay = Math.exp(-dt / PARAMS.tradeHalflifeMs);
      this.tradeImbalanceEwma = this.tradeImbalanceEwma * decay + signed;
    } else {
      this.tradeImbalanceEwma = signed;
    }
    this.lastTradeTs = ts;
  }

  onDepthSnapshot(ts, bids, asks) {
    if (bids.length > 0) this.depthBestBid = bids[0][0];
    if (asks.length > 0) this.depthBestAsk = asks[0][0];
    this.depthStale = false;

    const topBids = bids.slice(0, PARAMS.depthLevels);
    const topAsks = asks.slice(0, PARAMS.depthLevels);
    let totalBidQty = 0, totalAskQty = 0, vwapBidNum = 0, vwapAskNum = 0;
    for (const [price, qty] of topBids) { totalBidQty += qty; vwapBidNum += price * qty; }
    for (const [price, qty] of topAsks) { totalAskQty += qty; vwapAskNum += price * qty; }
    if (totalBidQty > 0 && totalAskQty > 0) {
      const vwapBid = vwapBidNum / totalBidQty;
      const vwapAsk = vwapAskNum / totalAskQty;
      this.depthMicroprice = (vwapBid * totalAskQty + vwapAsk * totalBidQty) / (totalBidQty + totalAskQty);
    }

    const imbBids = bids.slice(0, PARAMS.bookImbalanceLevels);
    const imbAsks = asks.slice(0, PARAMS.bookImbalanceLevels);
    let bidDepth = 0, askDepth = 0;
    for (const [, qty] of imbBids) bidDepth += qty;
    for (const [, qty] of imbAsks) askDepth += qty;
    const totalDepth = bidDepth + askDepth;
    this.bookImbalance = totalDepth > 0 ? bidDepth / totalDepth : 0.5;
  }

  sample(ts, strikePrice, expiryMs) {
    if (!this.ready || this.microprice <= 0) return null;
    let sNow;
    if (this.depthStale || this.depthMicroprice === 0) {
      sNow = this.microprice;
    } else {
      sNow = PARAMS.micropriceWeight * this.microprice + PARAMS.depthMicropriceWeight * this.depthMicroprice;
    }
    const tau = Math.max(0, (expiryMs - ts) / 1000);
    const vol = Math.max(this.volEwma5s, PARAMS.volFloorPerSec);
    const sigmaReturns = Math.sqrt(vol * Math.max(tau, 0.001));
    const sigmaDollars = sNow * sigmaReturns;
    const mu = PARAMS.driftCoeff * this.tradeImbalanceEwma;
    let fairYes;
    if (tau <= 0) {
      fairYes = sNow > strikePrice ? 1.0 : 0.0;
    } else {
      const z = (Math.log(strikePrice / sNow) - mu / sNow) / sigmaReturns;
      fairYes = 1 - normalCDF(z);
    }
    fairYes = Math.max(0, Math.min(1, fairYes));
    return {
      microprice: sNow, tradeImbalance: this.tradeImbalanceEwma,
      bookImbalance: this.bookImbalance,
      rv1s: this.volEwma1s, rv5s: this.volEwma5s, rv15s: this.volEwma15s,
      sigma: sigmaDollars, mu, tau, fairYes, fairNo: 1 - fairYes,
    };
  }
}

const BOOK_STRIKE_FV_PARAMS = {
  distanceAlpha: 3,
  ewmaHalflifeSec: 30,
  minRangeDollars: 5.0,
};

class BookStrikeFairValueAccumulator {
  constructor() {
    this.lastBids = [];
    this.lastAsks = [];
    this.smoothedP = 0.5;
    this.lastSampleTs = 0;
    this.lastDepthTs = 0;
    this.ready = false;
  }

  onDepthSnapshot(ts, bids, asks) {
    this.lastBids = bids;
    this.lastAsks = asks;
    this.lastDepthTs = ts;
    this.ready = bids.length > 0 && asks.length > 0;
  }

  sample(strikePrice) {
    if (!this.ready || this.lastBids.length === 0 || this.lastAsks.length === 0) return null;

    let farthestBidBelow = -1;
    for (let i = this.lastBids.length - 1; i >= 0; i--) {
      if (this.lastBids[i][0] <= strikePrice) { farthestBidBelow = this.lastBids[i][0]; break; }
    }

    let farthestAskAbove = -1;
    for (let i = this.lastAsks.length - 1; i >= 0; i--) {
      if (this.lastAsks[i][0] >= strikePrice) { farthestAskAbove = this.lastAsks[i][0]; break; }
    }

    if (farthestBidBelow < 0 || farthestAskAbove < 0) {
      return { rawP: 0.5, smoothedP: this.smoothedP, bookFairYes: this.smoothedP, bookFairNo: 1 - this.smoothedP, bullishDepth: 0, bearishDepth: 0, range: 0, valid: false };
    }

    const maxBelow = strikePrice - farthestBidBelow;
    const maxAbove = farthestAskAbove - strikePrice;
    const range = Math.min(maxBelow, maxAbove);

    if (range < BOOK_STRIKE_FV_PARAMS.minRangeDollars) {
      return { rawP: 0.5, smoothedP: this.smoothedP, bookFairYes: this.smoothedP, bookFairNo: 1 - this.smoothedP, bullishDepth: 0, bearishDepth: 0, range, valid: false };
    }

    const lowerBound = strikePrice - range;
    const upperBound = strikePrice + range;

    let bullishDepth = 0;
    for (const [price, qty] of this.lastBids) {
      if (price > strikePrice) continue;
      if (price < lowerBound) break;
      bullishDepth += qty * (1 / (1 + BOOK_STRIKE_FV_PARAMS.distanceAlpha * Math.abs(price - strikePrice) / strikePrice));
    }

    let bearishDepth = 0;
    for (const [price, qty] of this.lastAsks) {
      if (price < strikePrice) continue;
      if (price > upperBound) break;
      bearishDepth += qty * (1 / (1 + BOOK_STRIKE_FV_PARAMS.distanceAlpha * Math.abs(price - strikePrice) / strikePrice));
    }

    const total = bullishDepth + bearishDepth;
    if (total <= 0) {
      return { rawP: 0.5, smoothedP: this.smoothedP, bookFairYes: this.smoothedP, bookFairNo: 1 - this.smoothedP, bullishDepth: 0, bearishDepth: 0, range, valid: false };
    }

    const rawP = bullishDepth / total;

    if (this.lastSampleTs > 0) {
      const dt = (this.lastDepthTs - this.lastSampleTs) / 1000;
      if (dt > 0 && dt < 60) {
        const decay = Math.exp(-dt / BOOK_STRIKE_FV_PARAMS.ewmaHalflifeSec);
        this.smoothedP = this.smoothedP * decay + rawP * (1 - decay);
      } else {
        this.smoothedP = rawP;
      }
    } else {
      this.smoothedP = rawP;
    }
    this.lastSampleTs = this.lastDepthTs;

    const bookFairYes = Math.max(0, Math.min(1, this.smoothedP));

    return {
      rawP,
      smoothedP: this.smoothedP,
      bookFairYes,
      bookFairNo: 1 - bookFairYes,
      bullishDepth,
      bearishDepth,
      range,
      valid: true,
    };
  }
}

const epoch = process.argv[2];
if (!epoch) {
  console.error('Usage: node process-worker.mjs <epoch>');
  process.exit(1);
}

function sendProgress(pct, msg) {
  process.stdout.write(JSON.stringify({ pct, msg }) + '\n');
}

async function readFileLines(filePath, onLine, label, pctOffset, pctRange) {
  const totalBytes = statSync(filePath).size;
  const offset = pctOffset ?? 0;
  const range = pctRange ?? 100;
  let bytesRead = 0;
  let lastPct = -1;

  const stream = createReadStream(filePath, { encoding: 'utf-8', highWaterMark: 256 * 1024 });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });

  for await (const line of rl) {
    bytesRead += Buffer.byteLength(line, 'utf-8') + 1;
    onLine(line, bytesRead);

    const pct = Math.floor(offset + (bytesRead / totalBytes) * range);
    if (pct !== lastPct) {
      lastPct = pct;
      sendProgress(Math.min(pct, offset + range), `Reading ${label}...`);
    }
  }
}

async function main() {
  const epochDir = path.join(DATA_DIR, epoch);
  const cacheDir = path.join(epochDir, '.cache');
  const metaPath = path.join(cacheDir, 'meta.json');
  const framesPath = path.join(cacheDir, 'frames.json');

  if (existsSync(metaPath) && existsSync(framesPath)) {
    try {
      const cachedMeta = JSON.parse(readFileSync(metaPath, 'utf-8'));
      if (cachedMeta.cacheVersion === CACHE_VERSION) {
        sendProgress(100, 'Loaded from cache');
        process.exit(0);
      }
    } catch { /* reprocess on parse error */ }
  }

  const clobPath = path.join(epochDir, 'clob.jsonl');
  const depthPath = path.join(epochDir, 'depth-trade.jsonl');

  if (!existsSync(clobPath) || !existsSync(depthPath)) {
    console.error(`Missing data files for epoch ${epoch}`);
    process.exit(1);
  }

  // Pass 1: clob.jsonl
  let lookup = null;
  let marketStart = 0;
  const clobEntries = [];

  sendProgress(0, 'Reading clob.jsonl...');

  await readFileLines(
    clobPath,
    (line) => {
      if (!line.trim()) return;
      let parsed;
      try { parsed = JSON.parse(line); } catch { return; }

      if (!marketStart && parsed.market_start) marketStart = parsed.market_start;

      const eventType = parsed.event_type;
      const normalized = parsed.normalized;
      if (!normalized) return;

      if (eventType === 'market_lookup' && !lookup) {
        lookup = {
          marketId: normalized.market_id,
          slug: normalized.slug,
          question: normalized.question,
          outcomes: normalized.outcomes,
          clobTokenIds: normalized.clob_token_ids,
          endDate: normalized.end_date,
          gameStartTime: normalized.game_start_time ?? null,
        };
      } else if (eventType === 'book_state') {
        const bids = normalized.bids;
        const asks = normalized.asks;
        clobEntries.push({
          ts: parsed.ts_recv_ms,
          assetId: normalized.asset_id,
          bestBid: parseFloat(normalized.best_bid),
          bestAsk: parseFloat(normalized.best_ask),
          lastTrade: normalized.last_trade_price ? parseFloat(normalized.last_trade_price) : null,
          bids: bids ? bids.slice(0, 100) : [],
          asks: asks ? asks.slice(0, 100) : [],
        });
      }
    },
    'clob.jsonl', 0, 45,
  );

  if (!lookup) {
    console.error(`No market_lookup event found in epoch ${epoch}`);
    process.exit(1);
  }

  // Pass 2: depth-trade.jsonl (tickers + trades only; depths streamed later)
  const btcTickers = [];
  const btcTrades = [];
  let explicitStrikePrice = null;
  let depthMinTs = Infinity, depthMaxTs = -Infinity;

  sendProgress(45, 'Reading depth-trade.jsonl...');

  await readFileLines(
    depthPath,
    (line) => {
      if (!line.trim()) return;
      let parsed;
      try { parsed = JSON.parse(line); } catch { return; }

      const eventType = parsed.event_type;
      const normalized = parsed.normalized;

      if (eventType === 'system' && parsed.system_type === 'strike_price' && normalized) {
        explicitStrikePrice = parseFloat(normalized.strike_price);
      }

      if (!normalized) return;

      if (eventType === 'book_ticker_normalized') {
        btcTickers.push({
          ts: parsed.ts_recv_ms,
          bid: parseFloat(normalized.bid_price),
          ask: parseFloat(normalized.ask_price),
          bidQty: parseFloat(normalized.bid_qty),
          askQty: parseFloat(normalized.ask_qty),
        });
      } else if (eventType === 'book_state') {
        const ts = parsed.ts_recv_ms;
        if (ts < depthMinTs) depthMinTs = ts;
        if (ts > depthMaxTs) depthMaxTs = ts;
      } else if (eventType === 'trade_normalized') {
        btcTrades.push({
          ts: parsed.ts_recv_ms,
          qty: parseFloat(normalized.qty),
          side: normalized.side,
        });
      }
    },
    'depth-trade.jsonl', 45, 45,
  );

  sendProgress(90, 'Generating frames...');

  const upTokenId = lookup.clobTokenIds[0];
  const downTokenId = lookup.clobTokenIds[1];

  const upEntries = clobEntries.filter((e) => e.assetId === upTokenId);
  const downEntries = clobEntries.filter((e) => e.assetId === downTokenId);

  upEntries.sort((a, b) => a.ts - b.ts);
  downEntries.sort((a, b) => a.ts - b.ts);
  btcTickers.sort((a, b) => a.ts - b.ts);
  btcTrades.sort((a, b) => a.ts - b.ts);

  const allTimestamps = [];
  if (upEntries.length) allTimestamps.push(upEntries[0].ts, upEntries[upEntries.length - 1].ts);
  if (downEntries.length) allTimestamps.push(downEntries[0].ts, downEntries[downEntries.length - 1].ts);
  if (btcTickers.length) allTimestamps.push(btcTickers[0].ts, btcTickers[btcTickers.length - 1].ts);
  if (depthMinTs !== Infinity) allTimestamps.push(depthMinTs, depthMaxTs);

  if (allTimestamps.length === 0) {
    console.error(`No data events found for epoch ${epoch}`);
    process.exit(1);
  }

  const globalStart = Math.min(...allTimestamps);
  const globalEnd = Math.max(...allTimestamps);
  const INTERVAL_MS = 100;

  // Resolve strike price before frame gen so fair value can use it
  let resolvedStrikePrice = explicitStrikePrice;
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

  const expiryMs = marketStart * 1000 + PARAMS.marketDurationSec * 1000;
  const accumulator = new FairValueAccumulator();
  const bookStrikeFvAccumulator = new BookStrikeFairValueAccumulator();

  // Stream depth data from file during frame generation to avoid OOM
  const depthStream2 = createReadStream(depthPath, { encoding: 'utf-8', highWaterMark: 256 * 1024 });
  const depthRl2 = createInterface({ input: depthStream2, crlfDelay: Infinity });
  const depthIter = depthRl2[Symbol.asyncIterator]();
  let pendingDepth = null;
  let depthDone = false;

  async function fetchNextDepth() {
    while (!depthDone) {
      const { value, done } = await depthIter.next();
      if (done) { depthDone = true; return; }
      if (!value.trim()) continue;
      let parsed;
      try { parsed = JSON.parse(value); } catch { continue; }
      if (parsed.event_type === 'book_state' && parsed.normalized) {
        const n = parsed.normalized;
        pendingDepth = {
          ts: parsed.ts_recv_ms,
          bids: n.bids ? n.bids.slice(0, 200).map(([p, q]) => [parseFloat(p), parseFloat(q)]) : [],
          asks: n.asks ? n.asks.slice(0, 200).map(([p, q]) => [parseFloat(p), parseFloat(q)]) : [],
        };
        return;
      }
    }
  }

  await fetchNextDepth();

  let upPtr = 0, downPtr = 0, btcTickPtr = 0, btcTradePtr = 0;
  const frames = [];
  let latestUp = null, latestDown = null, latestBtcTick = null, latestBtcDepth = null;

  for (let ts = globalStart; ts <= globalEnd; ts += INTERVAL_MS) {
    while (upPtr < upEntries.length && upEntries[upPtr].ts <= ts) { latestUp = upEntries[upPtr]; upPtr++; }
    while (downPtr < downEntries.length && downEntries[downPtr].ts <= ts) { latestDown = downEntries[downPtr]; downPtr++; }
    while (btcTickPtr < btcTickers.length && btcTickers[btcTickPtr].ts <= ts) {
      const t = btcTickers[btcTickPtr];
      latestBtcTick = t;
      accumulator.onBookTicker(t.ts, t.bid, t.bidQty, t.ask, t.askQty);
      btcTickPtr++;
    }
    while (pendingDepth && pendingDepth.ts <= ts) {
      latestBtcDepth = pendingDepth;
      accumulator.onDepthSnapshot(pendingDepth.ts, pendingDepth.bids, pendingDepth.asks);
      bookStrikeFvAccumulator.onDepthSnapshot(pendingDepth.ts, pendingDepth.bids, pendingDepth.asks);
      pendingDepth = null;
      await fetchNextDepth();
    }
    while (btcTradePtr < btcTrades.length && btcTrades[btcTradePtr].ts <= ts) {
      const tr = btcTrades[btcTradePtr];
      accumulator.onTrade(tr.ts, tr.qty, tr.side);
      btcTradePtr++;
    }

    if (!latestBtcTick) continue;

    frames.push({
      t: ts - (marketStart * 1000),
      btc: { bid: latestBtcTick.bid, ask: latestBtcTick.ask, mid: (latestBtcTick.bid + latestBtcTick.ask) / 2 },
      up: { bestBid: latestUp?.bestBid ?? 0, bestAsk: latestUp?.bestAsk ?? 0, lastTrade: latestUp?.lastTrade ?? null },
      down: { bestBid: latestDown?.bestBid ?? 0, bestAsk: latestDown?.bestAsk ?? 0, lastTrade: latestDown?.lastTrade ?? null },
      btcDepth: { bids: latestBtcDepth?.bids ?? [], asks: latestBtcDepth?.asks ?? [] },
      polyUpDepth: { bids: latestUp?.bids ?? [], asks: latestUp?.asks ?? [] },
      polyDownDepth: { bids: latestDown?.bids ?? [], asks: latestDown?.asks ?? [] },
      fairValue: resolvedStrikePrice != null
        ? accumulator.sample(ts, resolvedStrikePrice, expiryMs)
        : null,
      bookFairValue: resolvedStrikePrice != null
        ? bookStrikeFvAccumulator.sample(resolvedStrikePrice)
        : null,
    });
  }

  depthRl2.close();

  sendProgress(95, 'Building metadata...');

  const meta = {
    epoch: parseInt(epoch, 10),
    slug: lookup.slug,
    question: lookup.question,
    outcomes: lookup.outcomes,
    clobTokenIds: lookup.clobTokenIds,
    upTokenId,
    downTokenId,
    startTime: globalStart,
    endTime: globalEnd,
    marketStart: marketStart * 1000,
    totalFrames: frames.length,
    durationMs: globalEnd - globalStart,
    strikePrice: resolvedStrikePrice,
    cacheVersion: CACHE_VERSION,
  };

  sendProgress(97, 'Writing cache...');
  if (!existsSync(cacheDir)) mkdirSync(cacheDir, { recursive: true });
  writeFileSync(metaPath, JSON.stringify(meta));
  writeFileSync(framesPath, JSON.stringify(frames));

  sendProgress(100, 'Done');
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
