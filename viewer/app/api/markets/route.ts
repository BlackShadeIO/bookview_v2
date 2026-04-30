import { readdirSync, statSync, existsSync } from 'fs';
import path from 'path';
import type { MarketInfo } from '@/lib/types';

const DATA_DIR = path.resolve(process.cwd(), '..', 'data');

export async function GET() {
  try {
    const entries = readdirSync(DATA_DIR, { withFileTypes: true });

    const markets: MarketInfo[] = [];

    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const epochNum = parseInt(entry.name, 10);
      if (isNaN(epochNum)) continue;

      const epochDir = path.join(DATA_DIR, entry.name);
      const clobPath = path.join(epochDir, 'clob.jsonl');
      const depthPath = path.join(epochDir, 'depth-trade.jsonl');
      const cacheMeta = path.join(epochDir, '.cache', 'meta.json');

      const clobExists = existsSync(clobPath);
      const depthExists = existsSync(depthPath);

      const clobSize = clobExists ? statSync(clobPath).size : 0;
      const depthSize = depthExists ? statSync(depthPath).size : 0;

      const processed = existsSync(cacheMeta);
      const complete = clobSize > 1_000_000 && depthSize > 1_000_000;

      // Convert epoch seconds to human-readable date
      const date = new Date(epochNum * 1000).toISOString();

      markets.push({
        epoch: epochNum,
        date,
        processed,
        clobSize,
        depthSize,
        complete,
      });
    }

    // Sort by epoch descending
    markets.sort((a, b) => b.epoch - a.epoch);

    return Response.json(markets);
  } catch (err) {
    const message = err instanceof Error ? err.message : 'Unknown error';
    return Response.json({ error: message }, { status: 500 });
  }
}
