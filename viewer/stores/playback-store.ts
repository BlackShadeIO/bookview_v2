import { create } from 'zustand';
import type { Frame, MarketMeta } from '@/lib/types';

export type Speed = 1 | 2 | 5 | 10;
export type LoadingState = 'idle' | 'processing' | 'loading' | 'ready' | 'error';

export interface PlaybackStore {
  // Data
  frames: Frame[];
  meta: MarketMeta | null;
  loadingState: LoadingState;
  loadingProgress: number;
  loadingMessage: string;
  error: string | null;

  // Playback
  currentIndex: number;
  isPlaying: boolean;
  speed: Speed;

  // UI
  expandedPanel: string | null;

  // Actions
  loadMarket: (epoch: string) => Promise<void>;
  play: () => void;
  pause: () => void;
  togglePlay: () => void;
  setSpeed: (s: Speed) => void;
  seekTo: (index: number) => void;
  seekToTime: (ms: number) => void;
  stepForward: () => void;
  stepBackward: () => void;
  tick: () => void;
  setExpandedPanel: (id: string | null) => void;
  reset: () => void;
}

export const usePlaybackStore = create<PlaybackStore>((set, get) => ({
  // Data
  frames: [],
  meta: null,
  loadingState: 'idle',
  loadingProgress: 0,
  loadingMessage: '',
  error: null,

  // Playback
  currentIndex: 0,
  isPlaying: false,
  speed: 1,

  // UI
  expandedPanel: null,

  // Actions
  loadMarket: async (epoch: string) => {
    set({
      loadingState: 'loading',
      loadingProgress: 0,
      loadingMessage: '',
      error: null,
      frames: [],
      meta: null,
      currentIndex: 0,
      isPlaying: false,
    });

    try {
      let res = await fetch(`/api/markets/${epoch}`);
      if (!res.ok) throw new Error(`Failed to fetch market: ${res.status}`);

      let data = await res.json();

      if (data.status === 'unprocessed') {
        set({ loadingState: 'processing', loadingMessage: 'Starting processing...' });

        // Trigger processing via POST
        const processRes = await fetch(`/api/markets/${epoch}/process`, { method: 'POST' });
        if (!processRes.ok) throw new Error('Failed to start processing');

        // Poll for progress
        await new Promise<void>((resolve, reject) => {
          const poll = async () => {
            try {
              const pollRes = await fetch(`/api/markets/${epoch}/process`);
              const status = await pollRes.json();

              set({
                loadingProgress: status.pct ?? get().loadingProgress,
                loadingMessage: status.msg ?? get().loadingMessage,
              });

              if (status.error) {
                reject(new Error(status.error));
                return;
              }

              if (status.status === 'done') {
                resolve();
                return;
              }

              setTimeout(poll, 500);
            } catch (err) {
              reject(err);
            }
          };
          poll();
        });

        // Re-fetch after processing completes
        res = await fetch(`/api/markets/${epoch}`);
        if (!res.ok) throw new Error(`Failed to fetch market after processing: ${res.status}`);
        data = await res.json();
      }

      set({
        frames: data.frames,
        meta: data.meta,
        loadingState: 'ready',
        loadingProgress: 100,
        currentIndex: 0,
      });
    } catch (err) {
      set({
        loadingState: 'error',
        error: err instanceof Error ? err.message : 'Unknown error',
      });
    }
  },

  play: () => {
    const { frames, currentIndex } = get();
    if (frames.length === 0) return;
    // If at end, restart from beginning
    if (currentIndex >= frames.length - 1) {
      set({ currentIndex: 0 });
    }
    set({ isPlaying: true });
  },

  pause: () => {
    set({ isPlaying: false });
  },

  togglePlay: () => {
    const { isPlaying, play, pause } = get();
    if (isPlaying) {
      pause();
    } else {
      play();
    }
  },

  setSpeed: (s: Speed) => {
    set({ speed: s });
  },

  seekTo: (index: number) => {
    const { frames } = get();
    if (frames.length === 0) return;
    const clamped = Math.max(0, Math.min(index, frames.length - 1));
    set({ currentIndex: clamped });
  },

  seekToTime: (ms: number) => {
    const { frames } = get();
    if (frames.length === 0) return;

    // Binary search for the last frame where frame.t <= ms
    let lo = 0;
    let hi = frames.length - 1;
    let result = 0;

    while (lo <= hi) {
      const mid = (lo + hi) >>> 1;
      if (frames[mid].t <= ms) {
        result = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }

    set({ currentIndex: result });
  },

  stepForward: () => {
    const { currentIndex, frames } = get();
    if (currentIndex < frames.length - 1) {
      set({ currentIndex: currentIndex + 1 });
    }
  },

  stepBackward: () => {
    const { currentIndex } = get();
    if (currentIndex > 0) {
      set({ currentIndex: currentIndex - 1 });
    }
  },

  tick: () => {
    const { currentIndex, frames, pause } = get();
    if (currentIndex < frames.length - 1) {
      set({ currentIndex: currentIndex + 1 });
    } else {
      pause();
    }
  },

  setExpandedPanel: (id: string | null) => {
    set({ expandedPanel: id });
  },

  reset: () => {
    set({
      frames: [],
      meta: null,
      loadingState: 'idle',
      loadingProgress: 0,
      loadingMessage: '',
      error: null,
      currentIndex: 0,
      isPlaying: false,
      speed: 1,
      expandedPanel: null,
    });
  },
}));
