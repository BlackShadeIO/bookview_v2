export interface BookStrikeFairValueData {
  rawP: number;
  smoothedP: number;
  bookFairYes: number;
  bookFairNo: number;
  bullishDepth: number;
  bearishDepth: number;
  range: number;
  valid: boolean;
}

export interface FairValueData {
  microprice: number;
  tradeImbalance: number;
  bookImbalance: number;
  rv1s: number;
  rv5s: number;
  rv15s: number;
  sigma: number;
  mu: number;
  tau: number;
  fairYes: number;
  fairNo: number;
}

export interface Frame {
  t: number; // ms offset from market_start
  btc: { bid: number; ask: number; mid: number };
  up: { bestBid: number; bestAsk: number; lastTrade: number | null };
  down: { bestBid: number; bestAsk: number; lastTrade: number | null };
  btcDepth: { bids: [number, number][]; asks: [number, number][] }; // up to 200 levels [price, qty]
  polyUpDepth: { bids: [string, string][]; asks: [string, string][] }; // up to 100 levels [price, size]
  polyDownDepth: { bids: [string, string][]; asks: [string, string][] };
  fairValue: FairValueData | null;
  bookFairValue: BookStrikeFairValueData | null;
}

export interface MarketMeta {
  epoch: number;
  slug: string;
  question: string;
  outcomes: string[];
  clobTokenIds: string[];
  upTokenId: string;
  downTokenId: string;
  startTime: number; // first ts_recv_ms
  endTime: number; // last ts_recv_ms
  marketStart: number; // market_start epoch
  totalFrames: number;
  durationMs: number;
  strikePrice: number | null;
  cacheVersion: number;
}

export interface MarketInfo {
  epoch: number;
  date: string;
  processed: boolean;
  clobSize: number;
  depthSize: number;
  complete: boolean;
}
