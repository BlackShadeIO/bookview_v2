'use client';

interface OrderBookTableProps {
  bids: [string | number, string | number][];
  asks: [string | number, string | number][];
  title: string;
  levels?: number;
}

function formatSize(size: number): string {
  if (size >= 1000) return size.toFixed(1);
  if (size >= 100) return size.toFixed(2);
  if (size >= 10) return size.toFixed(3);
  if (size >= 1) return size.toFixed(4);
  if (size >= 0.01) return size.toFixed(5);
  return size.toFixed(8);
}

function formatPrice(price: number): string {
  if (price >= 1000) return price.toFixed(2);
  if (price >= 1) return price.toFixed(2);
  return price.toFixed(4);
}

function formatLiquidity(total: number): string {
  if (total >= 1_000_000) return `${(total / 1_000_000).toFixed(2)}M`;
  if (total >= 1_000) return `${(total / 1_000).toFixed(1)}K`;
  return total.toFixed(2);
}

export function OrderBookTable({ bids, asks, title, levels = 10 }: OrderBookTableProps) {
  const parsedBids = bids.slice(0, levels).map(([p, q]) => ({
    price: Number(p),
    size: Number(q),
  }));
  const parsedAsks = asks.slice(0, levels).map(([p, q]) => ({
    price: Number(p),
    size: Number(q),
  }));

  const totalBidLiquidity = bids.reduce((sum, [, q]) => sum + Number(q), 0);
  const totalAskLiquidity = asks.reduce((sum, [, q]) => sum + Number(q), 0);

  let bidCum = 0;
  const bidsWithCum = parsedBids.map((b) => {
    bidCum += b.size;
    return { ...b, cum: bidCum };
  });
  const totalBidCum = bidCum || 0.0001;

  let askCum = 0;
  const asksWithCum = parsedAsks.map((a) => {
    askCum += a.size;
    return { ...a, cum: askCum };
  });
  const totalAskCum = askCum || 0.0001;

  return (
    <div className="p-2">
      <div className="flex items-center justify-between mb-2">
        <div className="text-[#64748B] text-[10px] font-medium tracking-wider">
          {title}
        </div>
        <div className="flex gap-3 text-[9px] font-mono">
          <span className="text-[#22C55E]">BID {formatLiquidity(totalBidLiquidity)}</span>
          <span className="text-[#EF4444]">ASK {formatLiquidity(totalAskLiquidity)}</span>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-2">
        {/* Bids */}
        <div>
          <div className="text-[10px] text-[#22C55E] font-medium tracking-wider mb-1 px-1">
            BID
          </div>
          <div className="flex justify-between text-[9px] text-[#64748B] mb-1 px-1">
            <span>PRICE</span>
            <span>SIZE</span>
          </div>
          <div className="overflow-y-auto" style={{ maxHeight: 'calc(100vh - 200px)' }}>
            {bidsWithCum.map((b, i) => (
              <div key={i} className="relative flex justify-between text-[11px] px-1 py-px">
                <div
                  className="absolute inset-0 bg-[#22C55E]/10"
                  style={{ width: `${(b.cum / totalBidCum) * 100}%`, right: 0, left: 'auto' }}
                />
                <span className="relative text-[#22C55E]">{formatPrice(b.price)}</span>
                <span className="relative text-[#94A3B8]">{formatSize(b.size)}</span>
              </div>
            ))}
          </div>
        </div>

        {/* Asks */}
        <div>
          <div className="text-[10px] text-[#EF4444] font-medium tracking-wider mb-1 px-1">
            ASK
          </div>
          <div className="flex justify-between text-[9px] text-[#64748B] mb-1 px-1">
            <span>PRICE</span>
            <span>SIZE</span>
          </div>
          <div className="overflow-y-auto" style={{ maxHeight: 'calc(100vh - 200px)' }}>
            {asksWithCum.map((a, i) => (
              <div key={i} className="relative flex justify-between text-[11px] px-1 py-px">
                <div
                  className="absolute inset-0 bg-[#EF4444]/10"
                  style={{ width: `${(a.cum / totalAskCum) * 100}%` }}
                />
                <span className="relative text-[#EF4444]">{formatPrice(a.price)}</span>
                <span className="relative text-[#94A3B8]">{formatSize(a.size)}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
