'use client';

import { SkipBack, Play, Pause, SkipForward } from 'lucide-react';
import { usePlaybackStore } from '@/stores/playback-store';
import { formatTime } from '@/lib/format';
import type { Speed } from '@/stores/playback-store';

const SPEEDS: Speed[] = [1, 2, 5, 10];

export function BottomBar() {
  const isPlaying = usePlaybackStore((s) => s.isPlaying);
  const speed = usePlaybackStore((s) => s.speed);
  const currentIndex = usePlaybackStore((s) => s.currentIndex);
  const frames = usePlaybackStore((s) => s.frames);
  const meta = usePlaybackStore((s) => s.meta);
  const togglePlay = usePlaybackStore((s) => s.togglePlay);
  const stepForward = usePlaybackStore((s) => s.stepForward);
  const stepBackward = usePlaybackStore((s) => s.stepBackward);
  const setSpeed = usePlaybackStore((s) => s.setSpeed);
  const seekTo = usePlaybackStore((s) => s.seekTo);

  const totalFrames = frames.length;
  const currentFrame = frames[currentIndex];
  const lastFrame = frames[totalFrames - 1];

  const elapsedMs = currentFrame?.t ?? 0;
  const totalMs = lastFrame?.t ?? 0;
  const remainingMs = totalMs - elapsedMs;

  return (
    <div className="h-14 flex items-center gap-3 px-4 bg-[#111827] border-t border-[#1E293B] shrink-0">
      {/* Playback controls */}
      <div className="flex items-center gap-1">
        <button
          onClick={stepBackward}
          className="p-1.5 text-[#94A3B8] hover:text-[#F8FAFC] cursor-pointer"
        >
          <SkipBack size={14} />
        </button>
        <button
          onClick={togglePlay}
          className="p-1.5 text-[#94A3B8] hover:text-[#F8FAFC] cursor-pointer"
        >
          {isPlaying ? <Pause size={14} /> : <Play size={14} />}
        </button>
        <button
          onClick={stepForward}
          className="p-1.5 text-[#94A3B8] hover:text-[#F8FAFC] cursor-pointer"
        >
          <SkipForward size={14} />
        </button>
      </div>

      {/* Speed buttons */}
      <div className="flex items-center gap-1">
        {SPEEDS.map((s) => (
          <button
            key={s}
            onClick={() => setSpeed(s)}
            className={`px-2 py-0.5 text-[10px] cursor-pointer rounded ${
              speed === s
                ? 'bg-[#3B82F6] text-[#F8FAFC]'
                : 'text-[#64748B] hover:text-[#94A3B8]'
            }`}
          >
            {s}x
          </button>
        ))}
      </div>

      {/* Elapsed time */}
      <span className="text-[#94A3B8] text-[10px] w-12 text-right shrink-0">
        {formatTime(elapsedMs)}
      </span>

      {/* Timeline scrubber */}
      <input
        type="range"
        min={0}
        max={Math.max(totalFrames - 1, 0)}
        value={currentIndex}
        onChange={(e) => seekTo(Number(e.target.value))}
        className="flex-1 h-1"
      />

      {/* Remaining time */}
      <span className="text-[#64748B] text-[10px] w-14 shrink-0">
        -{formatTime(remainingMs)}
      </span>
    </div>
  );
}
