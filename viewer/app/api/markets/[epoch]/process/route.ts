import { existsSync, readFileSync } from 'fs';
import path from 'path';

export const dynamic = 'force-dynamic';

const DATA_DIR = path.resolve(process.cwd(), '..', 'data');
const EXPECTED_CACHE_VERSION = 8;
const WORKER_SCRIPT = path.resolve(process.cwd(), 'scripts', 'process-worker.mjs');

function isCacheValid(epoch: string): boolean {
  const metaPath = path.join(DATA_DIR, epoch, '.cache', 'meta.json');
  const framesPath = path.join(DATA_DIR, epoch, '.cache', 'frames.json');
  if (!existsSync(metaPath) || !existsSync(framesPath)) return false;
  try {
    const meta = JSON.parse(readFileSync(metaPath, 'utf-8'));
    return meta.cacheVersion === EXPECTED_CACHE_VERSION;
  } catch { return false; }
}

// Track in-progress processing across requests
const jobs = new Map<string, { pct: number; msg: string; done: boolean; error: string | null }>();

// Use eval to avoid Turbopack static analysis of child_process
function getSpawn(): typeof import('node:child_process').spawn {
  // eslint-disable-next-line no-eval
  return eval("require")('child_process').spawn;
}

function startProcessing(epoch: string) {
  if (jobs.has(epoch)) return;

  const state = { pct: 0, msg: 'Starting...', done: false, error: null as string | null };
  jobs.set(epoch, state);

  const spawn = getSpawn();
  const child = spawn('node', [WORKER_SCRIPT, epoch], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  let stderr = '';

  child.stdout!.on('data', (chunk: Buffer) => {
    const lines = chunk.toString().split('\n').filter(Boolean);
    for (const line of lines) {
      try {
        const msg = JSON.parse(line);
        state.pct = msg.pct ?? state.pct;
        state.msg = msg.msg ?? state.msg;
      } catch {
        // ignore
      }
    }
  });

  child.stderr!.on('data', (chunk: Buffer) => {
    stderr += chunk.toString();
  });

  child.on('close', (code: number | null) => {
    if (code === 0) {
      state.pct = 100;
      state.msg = 'Done';
      state.done = true;
    } else {
      state.error = stderr.trim() || `Process exited with code ${code}`;
      state.done = true;
    }
    setTimeout(() => jobs.delete(epoch), 60000);
  });
}

export async function POST(
  _request: Request,
  { params }: { params: Promise<{ epoch: string }> },
) {
  const { epoch } = await params;

  if (isCacheValid(epoch)) {
    return Response.json({ status: 'done', pct: 100 });
  }

  startProcessing(epoch);
  return Response.json({ status: 'started', pct: 0 });
}

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ epoch: string }> },
) {
  const { epoch } = await params;

  if (isCacheValid(epoch)) {
    return Response.json({ status: 'done', pct: 100 });
  }

  const state = jobs.get(epoch);
  if (state) {
    if (state.error) {
      return Response.json({ status: 'error', error: state.error, pct: state.pct });
    }
    return Response.json({
      status: state.done ? 'done' : 'processing',
      pct: state.pct,
      msg: state.msg,
    });
  }

  return Response.json({ status: 'not_started' });
}
