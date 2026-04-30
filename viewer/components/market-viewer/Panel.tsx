'use client';

import { Maximize2 } from 'lucide-react';
import { usePlaybackStore } from '@/stores/playback-store';

export function Panel({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  const setExpandedPanel = usePlaybackStore((s) => s.setExpandedPanel);

  return (
    <div className="flex flex-col bg-[#111827] overflow-hidden">
      <div className="flex items-center justify-between px-2 py-1 shrink-0">
        <span className="text-[#64748B] text-[10px] font-medium tracking-wider">
          {title}
        </span>
        <button
          onClick={() => setExpandedPanel(id)}
          className="text-[#64748B] hover:text-[#F8FAFC] cursor-pointer p-0.5"
        >
          <Maximize2 size={12} />
        </button>
      </div>
      <div className="flex-1 relative min-h-0">{children}</div>
    </div>
  );
}
