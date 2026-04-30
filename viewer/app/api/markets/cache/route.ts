import { readdirSync, rmSync, existsSync } from 'fs';
import path from 'path';

export const dynamic = 'force-dynamic';

const DATA_DIR = path.resolve(process.cwd(), '..', 'data');

export async function DELETE() {
  const entries = readdirSync(DATA_DIR, { withFileTypes: true });
  let cleared = 0;

  for (const entry of entries) {
    if (!entry.isDirectory() || !/^\d+$/.test(entry.name)) continue;
    const cachePath = path.join(DATA_DIR, entry.name, '.cache');
    if (existsSync(cachePath)) {
      rmSync(cachePath, { recursive: true, force: true });
      cleared++;
    }
  }

  return Response.json({ cleared });
}
