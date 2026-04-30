'use client';

import { usePlaybackStore } from '@/stores/playback-store';
import { Panel } from '@/components/market-viewer/Panel';
import { PriceChart } from '@/components/charts/PriceChart';
import { OrderBookTable } from '@/components/charts/OrderBookTable';
import { formatBtcPrice, formatPrice } from '@/lib/format';
import type { Frame } from '@/lib/types';

export function PanelGrid() {
  const frames = usePlaybackStore((s) => s.frames);
  const currentIndex = usePlaybackStore((s) => s.currentIndex);
  const meta = usePlaybackStore((s) => s.meta);

  const frame = frames[currentIndex];

  return (
    <div className="flex-1 grid grid-rows-[1fr_1fr] gap-px bg-[#1E293B] overflow-hidden">
      {/* Top row: 3 price charts */}
      <div className="grid grid-cols-[2fr_1fr_1fr] gap-px">
        <Panel id="btc-price" title="BTC PRICE">
          <PriceChart
            id="btc-price"
            frames={frames}
            getValue={(f: Frame) => f.btc.mid}
            color="#F8FAFC"
            label="BTC"
            priceFormat={formatBtcPrice}
            strikePrice={meta?.strikePrice}
          />
        </Panel>
        <Panel id="up-price" title="UP TOKEN">
          <PriceChart
            id="up-price"
            frames={frames}
            getValue={(f: Frame) => f.up.bestAsk}
            color="#22C55E"
            label="UP"
            priceFormat={(n: number) => formatPrice(n, 4)}
            overlayGetValue={(f: Frame) => f.fairValue?.fairYes ?? null}
            overlayColor="#06B6D4"
            overlayLabel="FAIR"
            overlay2GetValue={(f: Frame) => f.bookFairValue?.bookFairYes ?? null}
            overlay2Color="#F97316"
            overlay2Label="BOOK"
          />
        </Panel>
        <Panel id="down-price" title="DOWN TOKEN">
          <PriceChart
            id="down-price"
            frames={frames}
            getValue={(f: Frame) => f.down.bestAsk}
            color="#EF4444"
            label="DOWN"
            priceFormat={(n: number) => formatPrice(n, 4)}
            overlayGetValue={(f: Frame) => f.fairValue?.fairNo ?? null}
            overlayColor="#06B6D4"
            overlayLabel="FAIR"
            overlay2GetValue={(f: Frame) => f.bookFairValue?.bookFairNo ?? null}
            overlay2Color="#F97316"
            overlay2Label="BOOK"
          />
        </Panel>
      </div>

      {/* Bottom row: 2 depth charts */}
      <div className="grid grid-cols-2 gap-px">
        <Panel id="btc-depth" title="BINANCE DEPTH">
          {frame && (
            <OrderBookTable
              bids={frame.btcDepth.bids}
              asks={frame.btcDepth.asks}
              title="BTC ORDER BOOK"
              levels={25}
            />
          )}
        </Panel>
        <Panel id="poly-depth" title="POLYMARKET DEPTH">
          {frame && (
            <div className="flex flex-col h-full overflow-y-auto">
              <OrderBookTable
                bids={frame.polyUpDepth.bids}
                asks={frame.polyUpDepth.asks}
                title="UP ORDER BOOK"
              />
              <div className="border-t border-[#1E293B]" />
              <OrderBookTable
                bids={frame.polyDownDepth.bids}
                asks={frame.polyDownDepth.asks}
                title="DOWN ORDER BOOK"
              />
            </div>
          )}
        </Panel>
      </div>
    </div>
  );
}
