'use client';

import { useEffect, useRef } from 'react';
import { usePlaybackStore } from '@/stores/playback-store';

const FRAME_INTERVAL = 100; // each frame = 100ms of market time
const MAX_DELTA = 200; // cap delta to prevent huge jumps on tab refocus

export function usePlaybackEngine() {
  const isPlaying = usePlaybackStore((s) => s.isPlaying);
  const lastTimestampRef = useRef<number>(0);
  const accumulatorRef = useRef<number>(0);
  const rafIdRef = useRef<number>(0);

  useEffect(() => {
    if (!isPlaying) {
      return;
    }

    accumulatorRef.current = 0;
    lastTimestampRef.current = 0;

    const loop = (timestamp: number) => {
      if (lastTimestampRef.current === 0) {
        lastTimestampRef.current = timestamp;
        rafIdRef.current = requestAnimationFrame(loop);
        return;
      }

      const rawDelta = timestamp - lastTimestampRef.current;
      const delta = Math.min(rawDelta, MAX_DELTA);
      lastTimestampRef.current = timestamp;

      const speed = usePlaybackStore.getState().speed;
      accumulatorRef.current += delta * speed;

      while (accumulatorRef.current >= FRAME_INTERVAL) {
        usePlaybackStore.getState().tick();
        accumulatorRef.current -= FRAME_INTERVAL;

        // Stop looping if playback was paused by tick (reached end)
        if (!usePlaybackStore.getState().isPlaying) {
          accumulatorRef.current = 0;
          return;
        }
      }

      rafIdRef.current = requestAnimationFrame(loop);
    };

    rafIdRef.current = requestAnimationFrame(loop);

    return () => {
      cancelAnimationFrame(rafIdRef.current);
    };
  }, [isPlaying]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Ignore when typing in inputs
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;

      const store = usePlaybackStore.getState();

      switch (e.key) {
        case ' ':
          e.preventDefault();
          store.togglePlay();
          break;
        case 'ArrowRight':
          e.preventDefault();
          store.stepForward();
          break;
        case 'ArrowLeft':
          e.preventDefault();
          store.stepBackward();
          break;
        case '1':
          store.setSpeed(1);
          break;
        case '2':
          store.setSpeed(2);
          break;
        case '5':
          store.setSpeed(5);
          break;
        case '0':
          store.setSpeed(10);
          break;
        case 'Escape':
          store.setExpandedPanel(null);
          break;
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, []);
}
