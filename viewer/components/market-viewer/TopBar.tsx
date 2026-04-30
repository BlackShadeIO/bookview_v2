'use client';

import Link from 'next/link';
import { ArrowLeft } from 'lucide-react';
import { usePlaybackStore } from '@/stores/playback-store';
import { formatBtcPrice, formatPrice } from '@/lib/format';

export function TopBar() {
  const meta = usePlaybackStore((s) => s.meta);
  const frames = usePlaybackStore((s) => s.frames);
  const currentIndex = usePlaybackStore((s) => s.currentIndex);

  const frame = frames?.[currentIndex];
  const strike = meta?.strikePrice ?? null;
  const btcPrice = frame?.btc.mid ?? null;
  const diff = strike != null && btcPrice != null ? btcPrice - strike : null;
  const diffPct = diff != null && strike ? (diff / strike) * 100 : null;
  const diffColor = diff == null ? '#94A3B8' : diff > 0 ? '#22C55E' : diff < 0 ? '#EF4444' : '#94A3B8';
  const diffSign = diff == null ? '' : diff > 0 ? '+' : diff < 0 ? '-' : '';

  return (
    <div className="h-10 flex items-center justify-between px-4 bg-[#111827] border-b border-[#1E293B] shrink-0">
      <div className="flex items-center gap-3">
        <Link
          href="/"
          className="text-[#64748B] hover:text-[#F8FAFC] cursor-pointer p-1 -ml-1"
        >
          <ArrowLeft size={14} />
        </Link>
        <span className="text-[#3B82F6] font-bold text-xs tracking-wider">BOOKVIEW</span>
        {meta && (
          <span className="text-[#94A3B8] text-xs truncate max-w-80">
            {meta.slug}
          </span>
        )}
      </div>
      {frame && (
        <div className="flex items-center gap-4">
          {strike != null && (
            <div className="flex items-center gap-1.5">
              <span className="text-[#64748B] text-[10px]">STRIKE</span>
              <span className="text-[#FBBF24] text-xs font-medium">
                {formatBtcPrice(strike)}
              </span>
            </div>
          )}
          <div className="flex items-center gap-1.5">
            <span className="text-[#64748B] text-[10px]">BTC</span>
            <span className="text-[#F8FAFC] text-xs font-medium">
              {formatBtcPrice(frame.btc.mid)}
            </span>
          </div>
          {diff != null && (
            <div className="flex items-center gap-1.5">
              <span className="text-[#64748B] text-[10px]">Δ</span>
              <span
                className="text-xs font-medium"
                style={{ color: diffColor }}
              >
                {diffSign}{formatBtcPrice(Math.abs(diff))}
                <span className="text-[10px] ml-1 opacity-70">
                  ({diffSign}{Math.abs(diffPct!).toFixed(3)}%)
                </span>
              </span>
            </div>
          )}
          <div className="flex items-center gap-1.5">
            <span className="text-[#64748B] text-[10px]">UP</span>
            <span className="text-[#22C55E] text-xs font-medium">
              {formatPrice(frame.up.bestAsk, 2)}
            </span>
          </div>
          <div className="flex items-center gap-1.5">
            <span className="text-[#64748B] text-[10px]">DOWN</span>
            <span className="text-[#EF4444] text-xs font-medium">
              {formatPrice(frame.down.bestAsk, 2)}
            </span>
          </div>
          {frame.fairValue && (
            <>
              <div className="w-px h-4 bg-[#1E293B]" />
              <div className="flex items-center gap-1.5">
                <span className="text-[#64748B] text-[10px]">FAIR</span>
                <span className="text-[#06B6D4] text-xs font-medium">
                  {frame.fairValue.fairYes.toFixed(4)}
                </span>
              </div>
              {(() => {
                const polyMid = frame.up.bestBid > 0 && frame.up.bestAsk > 0
                  ? (frame.up.bestBid + frame.up.bestAsk) / 2
                  : null;
                if (polyMid === null) return null;
                const edge = (frame.fairValue!.fairYes - polyMid) * 100;
                const edgeColor = edge > 0 ? '#22C55E' : edge < 0 ? '#EF4444' : '#94A3B8';
                const edgeSign = edge > 0 ? '+' : '';
                return (
                  <div className="flex items-center gap-1.5">
                    <span className="text-[#64748B] text-[10px]">EDGE</span>
                    <span className="text-xs font-medium" style={{ color: edgeColor }}>
                      {edgeSign}{edge.toFixed(1)}c
                    </span>
                  </div>
                );
              })()}
            </>
          )}
          {frame.bookFairValue && frame.bookFairValue.valid && (
            <div className="flex items-center gap-1.5">
              <span className="text-[#64748B] text-[10px]">BOOK</span>
              <span className="text-[#F97316] text-xs font-medium">
                {frame.bookFairValue.bookFairYes.toFixed(4)}
              </span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
