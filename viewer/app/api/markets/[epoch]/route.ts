import { existsSync, readFileSync } from 'fs';
import path from 'path';

const DATA_DIR = path.resolve(process.cwd(), '..', 'data');

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ epoch: string }> },
) {
  const { epoch } = await params;
  const epochDir = path.join(DATA_DIR, epoch);
  const metaPath = path.join(epochDir, '.cache', 'meta.json');
  const framesPath = path.join(epochDir, '.cache', 'frames.json');

  if (!existsSync(epochDir)) {
    return Response.json({ error: 'Epoch not found' }, { status: 404 });
  }

  if (existsSync(metaPath) && existsSync(framesPath)) {
    const meta = JSON.parse(readFileSync(metaPath, 'utf-8'));
    if (meta.cacheVersion === 7) {
      const frames = JSON.parse(readFileSync(framesPath, 'utf-8'));
      return Response.json({ meta, frames });
    }
  }

  return Response.json({ status: 'unprocessed' });
}
