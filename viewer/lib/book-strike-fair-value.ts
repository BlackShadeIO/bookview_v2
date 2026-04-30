import type { BookStrikeFairValueData } from './types';

export const BOOK_STRIKE_FV_PARAMS = {
  distanceAlpha: 3,
  ewmaHalflifeSec: 30,
  minRangeDollars: 5.0,
};

export class BookStrikeFairValueAccumulator {
  private lastBids: [number, number][] = [];
  private lastAsks: [number, number][] = [];
  private smoothedP = 0.5;
  private lastSampleTs = 0;
  private lastDepthTs = 0;
  private ready = false;

  onDepthSnapshot(ts: number, bids: [number, number][], asks: [number, number][]): void {
    this.lastBids = bids;
    this.lastAsks = asks;
    this.lastDepthTs = ts;
    this.ready = bids.length > 0 && asks.length > 0;
  }

  sample(strikePrice: number): BookStrikeFairValueData | null {
    if (!this.ready || this.lastBids.length === 0 || this.lastAsks.length === 0) return null;

    let farthestBidBelow = -1;
    for (let i = this.lastBids.length - 1; i >= 0; i--) {
      if (this.lastBids[i][0] <= strikePrice) {
        farthestBidBelow = this.lastBids[i][0];
        break;
      }
    }

    let farthestAskAbove = -1;
    for (let i = this.lastAsks.length - 1; i >= 0; i--) {
      if (this.lastAsks[i][0] >= strikePrice) {
        farthestAskAbove = this.lastAsks[i][0];
        break;
      }
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
      const w = 1 / (1 + BOOK_STRIKE_FV_PARAMS.distanceAlpha * Math.abs(price - strikePrice) / strikePrice);
      bullishDepth += qty * w;
    }

    let bearishDepth = 0;
    for (const [price, qty] of this.lastAsks) {
      if (price < strikePrice) continue;
      if (price > upperBound) break;
      const w = 1 / (1 + BOOK_STRIKE_FV_PARAMS.distanceAlpha * Math.abs(price - strikePrice) / strikePrice);
      bearishDepth += qty * w;
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
