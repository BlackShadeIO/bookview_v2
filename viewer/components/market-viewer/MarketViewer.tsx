'use client';

import { useEffect } from 'react';
import { usePlaybackStore } from '@/stores/playback-store';
import { usePlaybackEngine } from '@/hooks/usePlaybackEngine';
import { TopBar } from '@/components/market-viewer/TopBar';
import { PanelGrid } from '@/components/market-viewer/PanelGrid';
import { BottomBar } from '@/components/market-viewer/BottomBar';
import { ExpandedView } from '@/components/charts/ExpandedView';

export function MarketViewer({ epoch }: { epoch: string }) {
  const loadingState = usePlaybackStore((s) => s.loadingState);
  const loadingProgress = usePlaybackStore((s) => s.loadingProgress);
  const loadingMessage = usePlaybackStore((s) => s.loadingMessage);
  const error = usePlaybackStore((s) => s.error);
  const expandedPanel = usePlaybackStore((s) => s.expandedPanel);
  const loadMarket = usePlaybackStore((s) => s.loadMarket);
  const reset = usePlaybackStore((s) => s.reset);

  usePlaybackEngine();

  useEffect(() => {
    loadMarket(epoch);
    return () => {
      reset();
    };
  }, [epoch, loadMarket, reset]);

  if (loadingState === 'error') {
    return (
      <div className="h-screen flex items-center justify-center bg-[#0A0F1E]">
        <div className="text-center">
          <div className="text-[#EF4444] text-sm mb-2">ERROR</div>
          <div className="text-[#94A3B8] text-xs">{error}</div>
        </div>
      </div>
    );
  }

  if (loadingState !== 'ready') {
    return (
      <div className="h-screen flex items-center justify-center bg-[#0A0F1E]">
        <div className="w-80 text-center">
          <div className="text-[#F8FAFC] text-sm mb-3">
            {loadingState === 'processing' ? 'PROCESSING' : 'LOADING'}
          </div>
          <div className="w-full h-1 bg-[#1E293B] rounded overflow-hidden mb-2">
            <div
              className="h-full bg-[#3B82F6] transition-all duration-300"
              style={{ width: `${loadingProgress}%` }}
            />
          </div>
          <div className="text-[#64748B] text-xs">
            {loadingMessage || `${loadingProgress}%`}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="h-screen flex flex-col bg-[#0A0F1E]">
      <TopBar />
      {expandedPanel ? <ExpandedView /> : <PanelGrid />}
      <BottomBar />
    </div>
  );
}
