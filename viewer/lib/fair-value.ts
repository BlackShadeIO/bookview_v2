import type { FairValueData } from './types';

export const CACHE_VERSION = 8;

export const PARAMS = {
  micropriceWeight: 0.7,
  depthMicropriceWeight: 0.3,
  depthLevels: 5,
  bookImbalanceLevels: 10,
  tradeHalflifeMs: 2500,
  driftCoeff: 0.1,
  volFloorPerSec: 1e-8,
  marketDurationSec: 300,
};

function normalCDF(x: number): number {
  if (x < -8) return 0;
  if (x > 8) return 1;

  const a1 = 0.254829592;
  const a2 = -0.284496736;
  const a3 = 1.421413741;
  const a4 = -1.453152027;
  const a5 = 1.061405429;
  const p = 0.3275911;

  const sign = x < 0 ? -1 : 1;
  const absX = Math.abs(x);
  const t = 1.0 / (1.0 + p * absX);
  const y =
    1.0 -
    ((((a5 * t + a4) * t + a3) * t + a2) * t + a1) *
      t *
      Math.exp((-absX * absX) / 2);

  return 0.5 * (1.0 + sign * y);
}

export class FairValueAccumulator {
  private bidPrice = 0;
  private bidQty = 0;
  private askPrice = 0;
  private askQty = 0;
  private microprice = 0;

  private depthBids: [number, number][] = [];
  private depthAsks: [number, number][] = [];
  private depthMicroprice = 0;
  private depthStale = false;
  private depthBestBid = 0;
  private depthBestAsk = 0;
  private bookImbalance = 0.5;

  private tradeImbalanceEwma = 0;
  private lastTradeTs = 0;

  private prevMid = 0;
  private prevMidTs = 0;
  private volEwma1s = PARAMS.volFloorPerSec;
  private volEwma5s = PARAMS.volFloorPerSec;
  private volEwma15s = PARAMS.volFloorPerSec;

  private ready = false;

  onBookTicker(ts: number, bidPrice: number, bidQty: number, askPrice: number, askQty: number): void {
    this.bidPrice = bidPrice;
    this.bidQty = bidQty;
    this.askPrice = askPrice;
    this.askQty = askQty;

    const totalQty = bidQty + askQty;
    this.microprice =
      totalQty > 0
        ? (bidPrice * askQty + askPrice * bidQty) / totalQty
        : (bidPrice + askPrice) / 2;

    if (this.depthBestBid > 0 && this.depthBestAsk > 0) {
      this.depthStale =
        bidPrice < this.depthBestBid || askPrice > this.depthBestAsk ||
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
    this.prevMid = mid;
    this.prevMidTs = ts;
    this.ready = true;
  }

  onTrade(ts: number, qty: number, side: string): void {
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

  onDepthSnapshot(ts: number, bids: [number, number][], asks: [number, number][]): void {
    this.depthBids = bids;
    this.depthAsks = asks;

    if (bids.length > 0) this.depthBestBid = bids[0][0];
    if (asks.length > 0) this.depthBestAsk = asks[0][0];
    this.depthStale = false;

    const topBids = bids.slice(0, PARAMS.depthLevels);
    const topAsks = asks.slice(0, PARAMS.depthLevels);

    let totalBidQty = 0;
    let totalAskQty = 0;
    let vwapBidNum = 0;
    let vwapAskNum = 0;

    for (const [price, qty] of topBids) {
      totalBidQty += qty;
      vwapBidNum += price * qty;
    }
    for (const [price, qty] of topAsks) {
      totalAskQty += qty;
      vwapAskNum += price * qty;
    }

    if (totalBidQty > 0 && totalAskQty > 0) {
      const vwapBid = vwapBidNum / totalBidQty;
      const vwapAsk = vwapAskNum / totalAskQty;
      this.depthMicroprice =
        (vwapBid * totalAskQty + vwapAsk * totalBidQty) /
        (totalBidQty + totalAskQty);
    }

    const imbBids = bids.slice(0, PARAMS.bookImbalanceLevels);
    const imbAsks = asks.slice(0, PARAMS.bookImbalanceLevels);
    let bidDepth = 0;
    let askDepth = 0;
    for (const [, qty] of imbBids) bidDepth += qty;
    for (const [, qty] of imbAsks) askDepth += qty;
    const totalDepth = bidDepth + askDepth;
    this.bookImbalance = totalDepth > 0 ? bidDepth / totalDepth : 0.5;
  }

  sample(ts: number, strikePrice: number, expiryMs: number): FairValueData | null {
    if (!this.ready || this.microprice <= 0) return null;

    let sNow: number;
    if (this.depthStale || this.depthMicroprice === 0) {
      sNow = this.microprice;
    } else {
      sNow =
        PARAMS.micropriceWeight * this.microprice +
        PARAMS.depthMicropriceWeight * this.depthMicroprice;
    }

    const tau = Math.max(0, (expiryMs - ts) / 1000);

    const vol = Math.max(this.volEwma5s, PARAMS.volFloorPerSec);
    const sigmaReturns = Math.sqrt(vol * Math.max(tau, 0.001));
    const sigmaDollars = sNow * sigmaReturns;

    const mu = PARAMS.driftCoeff * this.tradeImbalanceEwma;

    let fairYes: number;
    if (tau <= 0) {
      fairYes = sNow > strikePrice ? 1.0 : 0.0;
    } else {
      const z = (Math.log(strikePrice / sNow) - mu / sNow) / sigmaReturns;
      fairYes = 1 - normalCDF(z);
    }

    fairYes = Math.max(0, Math.min(1, fairYes));

    return {
      microprice: sNow,
      tradeImbalance: this.tradeImbalanceEwma,
      bookImbalance: this.bookImbalance,
      rv1s: this.volEwma1s,
      rv5s: this.volEwma5s,
      rv15s: this.volEwma15s,
      sigma: sigmaDollars,
      mu,
      tau,
      fairYes,
      fairNo: 1 - fairYes,
    };
  }
}
