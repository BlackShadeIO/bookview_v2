import path from 'path';
import { existsSync, openSync, readSync, closeSync } from 'fs';
import { JsonlTailer } from '@/lib/live/tailer';
import { LiveFrameBuilder } from '@/lib/live/frame-builder';

export const dynamic = 'force-dynamic';

const DATA_DIR = path.resolve(process.cwd(), '..', 'data');
const MARKET_INTERVAL = 300;
const FRAME_INTERVAL_MS = 100;
const PING_INTERVAL_MS = 15_000;
const SEED_HEAD_BYTES = 65536;
const DEPTH_SEED_BYTES = 150 * 1024 * 1024;
const CLOB_SEED_BYTES = 20 * 1024 * 1024;

function findActiveEpoch(): number {
  const nowSec = Math.floor(Date.now() / 1000);
  return Math.floor(nowSec / MARKET_INTERVAL) * MARKET_INTERVAL;
}

function seedMetaFromHead(filePath: string, builder: LiveFrameBuilder): void {
  if (!existsSync(filePath)) return;
  try {
    const fd = openSync(filePath, 'r');
    const buf = Buffer.alloc(SEED_HEAD_BYTES);
    const bytesRead = readSync(fd, buf, 0, SEED_HEAD_BYTES, 0);
    closeSync(fd);
    const text = buf.toString('utf-8', 0, bytesRead);
    for (const line of text.split('\n')) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      try {
        const parsed = JSON.parse(trimmed);
        const et = parsed.event_type;
        if (et === 'market_lookup' || et === 'system') {
          builder.ingest(parsed);
        }
      } catch { /* skip partial line */ }
    }
  } catch { /* ignore */ }
}

export async function GET(request: Request): Promise<Response> {
  let cancelled = false;
  let clobTailer: JsonlTailer | null = null;
  let depthTailer: JsonlTailer | null = null;
  let fvTailer: JsonlTailer | null = null;
  let frameTimer: ReturnType<typeof setInterval> | null = null;
  let pingTimer: ReturnType<typeof setInterval> | null = null;
  let waitTimer: ReturnType<typeof setInterval> | null = null;

  const stream = new ReadableStream({
    start(controller) {
      const encoder = new TextEncoder();

      function send(event: string, data: unknown): void {
        if (cancelled) return;
        try {
          if (event === 'message') {
            controller.enqueue(encoder.encode(`data: ${JSON.stringify(data)}\n\n`));
          } else {
            controller.enqueue(encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`));
          }
        } catch {
          // stream closed
        }
      }

      let epoch = findActiveEpoch();
      const builder = new LiveFrameBuilder(epoch);
      let metaSent = false;
      let strikeSent = false;

      function startTailers(ep: number, onSeeded?: () => void): void {
        const epochDir = path.join(DATA_DIR, String(ep));
        const clobPath = path.join(epochDir, 'clob.jsonl');
        const depthPath = path.join(epochDir, 'depth-trade.jsonl');
        const fvPath = path.join(epochDir, 'fair-value.jsonl');

        if (onSeeded) {
          let clobDone = false;
          let depthDone = false;
          const check = () => { if (clobDone && depthDone) onSeeded(); };

          clobTailer = new JsonlTailer(clobPath, (parsed) => builder.ingest(parsed), {
            seedBytes: CLOB_SEED_BYTES,
            onSeeded: () => { clobDone = true; check(); },
          });
          depthTailer = new JsonlTailer(depthPath, (parsed) => builder.ingest(parsed), {
            seedBytes: DEPTH_SEED_BYTES,
            onSeeded: () => { depthDone = true; check(); },
          });
        } else {
          clobTailer = new JsonlTailer(clobPath, (parsed) => builder.ingest(parsed));
          depthTailer = new JsonlTailer(depthPath, (parsed) => builder.ingest(parsed));
        }

        fvTailer = new JsonlTailer(fvPath, (parsed) => builder.ingest(parsed));

        clobTailer.start();
        depthTailer.start();
        fvTailer.start();
      }

      function stopTailers(): void {
        clobTailer?.stop();
        depthTailer?.stop();
        fvTailer?.stop();
        clobTailer = null;
        depthTailer = null;
        fvTailer = null;
      }

      function startFrameLoop(): void {
        frameTimer = setInterval(() => {
          if (cancelled) return;

          const now = Date.now();
          const newEpoch = findActiveEpoch();

          if (newEpoch !== epoch) {
            send('market-switch', { epoch: newEpoch });
            send('epoch', { epoch: newEpoch });
            stopTailers();
            epoch = newEpoch;
            builder.reset(epoch);
            metaSent = false;
            strikeSent = false;

            const epochDir = path.join(DATA_DIR, String(epoch));
            if (existsSync(epochDir)) {
              seedMetaFromHead(path.join(epochDir, 'clob.jsonl'), builder);
              startTailers(epoch);
            } else {
              send('status', { state: 'waiting', message: 'Waiting for new market data...' });
              waitForDir(epoch);
            }
            return;
          }

          const meta = builder.getMeta();
          if (meta && (!metaSent || (meta.strikePrice != null && !strikeSent))) {
            send('meta', meta);
            metaSent = true;
            if (meta.strikePrice != null) strikeSent = true;
          }

          const frame = builder.snapshot(now);
          if (frame) {
            send('message', frame);
          }
        }, FRAME_INTERVAL_MS);
      }

      function startStreaming(): void {
        metaSent = false;
        const epochDir2 = path.join(DATA_DIR, String(epoch));
        seedMetaFromHead(path.join(epochDir2, 'clob.jsonl'), builder);
        builder.enableHistory();
        startTailers(epoch, () => {
          if (cancelled) return;
          const history = builder.flushHistory();
          if (history.length > 0) {
            send('history', history);
          }
          startFrameLoop();
        });
      }

      function waitForDir(ep: number): void {
        if (waitTimer) clearInterval(waitTimer);
        waitTimer = setInterval(() => {
          if (cancelled) return;
          const newEpoch = Math.floor(Date.now() / 1000 / MARKET_INTERVAL) * MARKET_INTERVAL;
          if (newEpoch !== ep) {
            if (waitTimer) clearInterval(waitTimer);
            waitTimer = null;
            return;
          }
          const epochDir = path.join(DATA_DIR, String(ep));
          if (existsSync(epochDir)) {
            if (waitTimer) clearInterval(waitTimer);
            waitTimer = null;
            seedMetaFromHead(path.join(epochDir, 'clob.jsonl'), builder);
            startTailers(ep);
          }
        }, 1000);
      }

      send('epoch', { epoch });

      const epochDir = path.join(DATA_DIR, String(epoch));
      if (existsSync(epochDir)) {
        startStreaming();
      } else {
        send('status', { state: 'waiting', message: 'Waiting for market data...' });
        startStreaming();
      }

      pingTimer = setInterval(() => {
        if (!cancelled) send('ping', {});
      }, PING_INTERVAL_MS);

      request.signal.addEventListener('abort', () => {
        cancelled = true;
        stopTailers();
        if (frameTimer) clearInterval(frameTimer);
        if (pingTimer) clearInterval(pingTimer);
        if (waitTimer) clearInterval(waitTimer);
        try { controller.close(); } catch { /* already closed */ }
      });
    },
  });

  return new Response(stream, {
    headers: {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache, no-transform',
      'Connection': 'keep-alive',
      'X-Accel-Buffering': 'no',
    },
  });
}
