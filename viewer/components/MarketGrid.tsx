'use client';

import { useEffect, useState } from 'react';
import { Trash2 } from 'lucide-react';
import type { MarketInfo } from '@/lib/types';
import { MarketCard } from '@/components/MarketCard';

export function MarketGrid() {
  const [markets, setMarkets] = useState<MarketInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [clearing, setClearing] = useState(false);

  const fetchMarkets = () => {
    setLoading(true);
    fetch('/api/markets')
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data) => {
        setMarkets(data);
        setLoading(false);
      })
      .catch((err) => {
        setError(err.message);
        setLoading(false);
      });
  };

  useEffect(() => {
    fetchMarkets();
  }, []);

  const handleClearCache = async () => {
    if (!confirm('Clear all processed cache? Markets will need to be reprocessed on next view.')) return;
    setClearing(true);
    try {
      const res = await fetch('/api/markets/cache', { method: 'DELETE' });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      fetchMarkets();
    } catch (err) {
      alert('Failed to clear cache');
    } finally {
      setClearing(false);
    }
  };

  if (error) {
    return (
      <div className="text-[#EF4444] text-sm p-4 bg-[#111827] border border-[#1E293B] rounded">
        ERROR: {error}
      </div>
    );
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-3">
        <span className="text-[#64748B] text-xs">
          {loading ? '' : `${markets.length} markets`}
        </span>
        <button
          onClick={handleClearCache}
          disabled={clearing}
          className="flex items-center gap-1.5 text-[10px] text-[#64748B] hover:text-[#EF4444] cursor-pointer disabled:opacity-50 px-2 py-1 border border-[#1E293B] rounded hover:border-[#EF4444]/50"
        >
          <Trash2 size={10} />
          {clearing ? 'CLEARING...' : 'CLEAR CACHE'}
        </button>
      </div>

      {loading ? (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3">
          {Array.from({ length: 8 }).map((_, i) => (
            <div
              key={i}
              className="h-28 bg-[#111827] border border-[#1E293B] rounded animate-pulse"
            />
          ))}
        </div>
      ) : markets.length === 0 ? (
        <div className="text-[#64748B] text-sm p-4 bg-[#111827] border border-[#1E293B] rounded">
          No markets found.
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3">
          {markets.map((market) => (
            <MarketCard key={market.epoch} market={market} />
          ))}
        </div>
      )}
    </div>
  );
}
