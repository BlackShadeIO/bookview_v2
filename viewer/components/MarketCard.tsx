import Link from 'next/link';
import type { MarketInfo } from '@/lib/types';
import { epochToDate, formatFileSize } from '@/lib/format';

export function MarketCard({ market }: { market: MarketInfo }) {
  return (
    <Link href={`/market/${market.epoch}`}>
      <div className="bg-[#111827] border border-[#1E293B] rounded p-3 cursor-pointer hover:border-[#3B82F6] transition-colors">
        <div className="flex items-center justify-between mb-2">
          <span className="text-[#F8FAFC] text-sm font-semibold">
            {market.epoch}
          </span>
          {market.complete ? (
            <span className="text-[10px] px-1.5 py-0.5 bg-[#22C55E]/15 text-[#22C55E] rounded">
              COMPLETE
            </span>
          ) : (
            <span className="text-[10px] px-1.5 py-0.5 bg-[#FBBF24]/15 text-[#FBBF24] rounded">
              PARTIAL
            </span>
          )}
        </div>
        <div className="text-[#94A3B8] text-xs mb-2">
          {epochToDate(market.epoch)}
        </div>
        <div className="flex items-center gap-3 text-[10px] text-[#64748B]">
          <span>CLOB {formatFileSize(market.clobSize)}</span>
          <span>DEPTH {formatFileSize(market.depthSize)}</span>
          {market.processed && (
            <span className="text-[#3B82F6]">CACHED</span>
          )}
        </div>
      </div>
    </Link>
  );
}
