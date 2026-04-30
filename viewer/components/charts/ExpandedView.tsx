'use client';

import { useState } from 'react';
import { X } from 'lucide-react';
import { usePlaybackStore } from '@/stores/playback-store';
import { PriceChart } from '@/components/charts/PriceChart';
import { DepthChart } from '@/components/charts/DepthChart';
import { OrderBookTable } from '@/components/charts/OrderBookTable';
import { formatBtcPrice, formatPrice } from '@/lib/format';
import type { Frame } from '@/lib/types';

interface PanelDef {
  title: string;
  getChart: (frames: Frame[], frame: Frame, strikePrice?: number | null) => React.ReactNode;
  getBook: (frame: Frame) => { bids: [string | number, string | number][]; asks: [string | number, string | number][]; title: string };
}

const PANEL_CONFIG: Record<string, PanelDef> = {
  'btc-price': {
    title: 'BTC PRICE',
    getChart: (frames, _frame, strikePrice) => (
      <PriceChart id="btc-expanded" frames={frames} getValue={(f) => f.btc.mid} color="#F8FAFC" label="BTC" priceFormat={formatBtcPrice} strikePrice={strikePrice} />
    ),
    getBook: (frame) => ({ bids: frame.btcDepth.bids, asks: frame.btcDepth.asks, title: 'BTC ORDER BOOK' }),
  },
  'up-price': {
    title: 'UP TOKEN',
    getChart: (frames) => (
      <PriceChart id="up-expanded" frames={frames} getValue={(f) => f.up.bestAsk} color="#22C55E" label="UP" priceFormat={(n) => formatPrice(n, 4)} overlayGetValue={(f) => f.fairValue?.fairYes ?? null} overlayColor="#06B6D4" overlayLabel="FAIR" overlay2GetValue={(f) => f.bookFairValue?.bookFairYes ?? null} overlay2Color="#F97316" overlay2Label="BOOK" />
    ),
    getBook: (frame) => ({ bids: frame.polyUpDepth.bids, asks: frame.polyUpDepth.asks, title: 'UP ORDER BOOK' }),
  },
  'down-price': {
    title: 'DOWN TOKEN',
    getChart: (frames) => (
      <PriceChart id="down-expanded" frames={frames} getValue={(f) => f.down.bestAsk} color="#EF4444" label="DOWN" priceFormat={(n) => formatPrice(n, 4)} overlayGetValue={(f) => f.fairValue?.fairNo ?? null} overlayColor="#06B6D4" overlayLabel="FAIR" overlay2GetValue={(f) => f.bookFairValue?.bookFairNo ?? null} overlay2Color="#F97316" overlay2Label="BOOK" />
    ),
    getBook: (frame) => ({ bids: frame.polyDownDepth.bids, asks: frame.polyDownDepth.asks, title: 'DOWN ORDER BOOK' }),
  },
  'btc-depth': {
    title: 'BINANCE DEPTH',
    getChart: (_frames, frame) => (
      <DepthChart bids={frame.btcDepth.bids} asks={frame.btcDepth.asks} label="BTC" />
    ),
    getBook: (frame) => ({ bids: frame.btcDepth.bids, asks: frame.btcDepth.asks, title: 'BTC ORDER BOOK' }),
  },
  'poly-depth': {
    title: 'POLYMARKET DEPTH',
    getChart: (_frames, frame) => (
      <DepthChart bids={frame.polyUpDepth.bids} asks={frame.polyUpDepth.asks} label="UP/DOWN" />
    ),
    getBook: (frame) => ({ bids: frame.polyUpDepth.bids, asks: frame.polyUpDepth.asks, title: 'POLY ORDER BOOK' }),
  },
};

const DEPTH_LEVEL_OPTIONS = [5, 10, 20, 50, 100] as const;

export function ExpandedView() {
  const expandedPanel = usePlaybackStore((s) => s.expandedPanel);
  const setExpandedPanel = usePlaybackStore((s) => s.setExpandedPanel);
  const frames = usePlaybackStore((s) => s.frames);
  const currentIndex = usePlaybackStore((s) => s.currentIndex);
  const meta = usePlaybackStore((s) => s.meta);
  const [depthLevels, setDepthLevels] = useState<number>(10);

  if (!expandedPanel) return null;

  const config = PANEL_CONFIG[expandedPanel];
  if (!config) return null;

  const frame = frames[currentIndex];
  if (!frame) return null;

  const book = config.getBook(frame);

  return (
    <div className="flex-1 flex flex-col min-h-0 bg-[#0A0F1E]">
      {/* Title bar */}
      <div className="flex items-center justify-between h-10 px-4 border-b border-[#1E293B] shrink-0">
        <div className="flex items-center gap-3">
          <span className="text-[#F8FAFC] text-xs font-medium tracking-wider">
            {config.title}
          </span>
          {meta?.strikePrice != null && expandedPanel === 'btc-price' && (
            <span className="text-[#FBBF24] text-[10px]">
              STRIKE {formatBtcPrice(meta.strikePrice)}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <span className="text-[#64748B] text-[10px] tracking-wider">DEPTH</span>
          {DEPTH_LEVEL_OPTIONS.map((level) => (
            <button
              key={level}
              onClick={() => setDepthLevels(level)}
              className={`text-[10px] px-1.5 py-0.5 rounded cursor-pointer ${
                depthLevels === level
                  ? 'bg-[#3B82F6] text-[#F8FAFC]'
                  : 'text-[#64748B] hover:text-[#F8FAFC]'
              }`}
            >
              {level}
            </button>
          ))}
          <button
            onClick={() => setExpandedPanel(null)}
            className="text-[#64748B] hover:text-[#F8FAFC] cursor-pointer p-1 ml-2"
          >
            <X size={16} />
          </button>
        </div>
      </div>

      {/* Content: 70/30 split */}
      <div className="flex-1 flex min-h-0">
        <div className="relative" style={{ width: '70%' }}>
          {config.getChart(frames, frame, meta?.strikePrice)}
        </div>
        <div className="border-l border-[#1E293B] overflow-auto" style={{ width: '30%' }}>
          <OrderBookTable
            bids={book.bids}
            asks={book.asks}
            title={book.title}
            levels={depthLevels}
          />
        </div>
      </div>
    </div>
  );
}
