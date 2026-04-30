import { MarketGrid } from '@/components/MarketGrid';

export default function Home() {
  return (
    <div className="min-h-screen bg-[#0A0F1E] p-6">
      <div className="max-w-6xl mx-auto">
        <div className="flex items-center gap-3 mb-6">
          <h1 className="text-[#F8FAFC] text-xl font-bold tracking-wider">BOOKVIEW</h1>
          <span className="text-[#64748B] text-xs">MARKET DATA REPLAY</span>
        </div>
        <MarketGrid />
      </div>
    </div>
  );
}
