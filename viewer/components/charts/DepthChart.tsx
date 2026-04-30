'use client';

import { useEffect, useRef } from 'react';
import { COLORS } from '@/lib/theme';

interface DepthChartProps {
  bids: [string | number, string | number][];
  asks: [string | number, string | number][];
  label: string;
}

export function DepthChart({ bids, asks, label }: DepthChartProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
    if (!canvas || !container) return;

    const draw = () => {
      const rect = container.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      canvas.width = rect.width * dpr;
      canvas.height = rect.height * dpr;
      canvas.style.width = rect.width + 'px';
      canvas.style.height = rect.height + 'px';

      const ctx = canvas.getContext('2d');
      if (!ctx) return;

      ctx.scale(dpr, dpr);
      const w = rect.width;
      const h = rect.height;

      ctx.clearRect(0, 0, w, h);

      const parsedBids = bids.slice(0, 10).map(([p, q]) => ({
        price: Number(p),
        qty: Number(q),
      }));
      const parsedAsks = asks.slice(0, 10).map(([p, q]) => ({
        price: Number(p),
        qty: Number(q),
      }));

      const allQty = [...parsedBids, ...parsedAsks].map((l) => l.qty);
      const maxQty = Math.max(...allQty, 0.0001);

      const levels = Math.max(parsedBids.length, parsedAsks.length, 1);
      const rowH = Math.min(h / levels, 20);
      const centerX = w / 2;
      const barMaxW = centerX - 50;
      const startY = (h - levels * rowH) / 2;

      ctx.font = '10px "JetBrains Mono", monospace';
      ctx.textBaseline = 'middle';

      // Bids (green, extend left from center)
      for (let i = 0; i < parsedBids.length; i++) {
        const { price, qty } = parsedBids[i];
        const y = startY + i * rowH;
        const barW = (qty / maxQty) * barMaxW;

        ctx.fillStyle = COLORS.green + '30';
        ctx.fillRect(centerX - barW, y + 1, barW, rowH - 2);

        ctx.fillStyle = COLORS.green;
        ctx.textAlign = 'right';
        ctx.fillText(price.toFixed(2), centerX - barW - 4, y + rowH / 2);

        ctx.fillStyle = COLORS.textDim;
        ctx.textAlign = 'left';
        ctx.fillText(qty.toFixed(2), centerX - barW + 3, y + rowH / 2);
      }

      // Asks (red, extend right from center)
      for (let i = 0; i < parsedAsks.length; i++) {
        const { price, qty } = parsedAsks[i];
        const y = startY + i * rowH;
        const barW = (qty / maxQty) * barMaxW;

        ctx.fillStyle = COLORS.red + '30';
        ctx.fillRect(centerX, y + 1, barW, rowH - 2);

        ctx.fillStyle = COLORS.red;
        ctx.textAlign = 'left';
        ctx.fillText(price.toFixed(2), centerX + barW + 4, y + rowH / 2);

        ctx.fillStyle = COLORS.textDim;
        ctx.textAlign = 'right';
        ctx.fillText(qty.toFixed(2), centerX + barW - 3, y + rowH / 2);
      }

      // Center divider
      ctx.strokeStyle = COLORS.border;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(centerX, startY);
      ctx.lineTo(centerX, startY + levels * rowH);
      ctx.stroke();
    };

    draw();

    const ro = new ResizeObserver(() => {
      requestAnimationFrame(draw);
    });
    ro.observe(container);

    return () => ro.disconnect();
  }, [bids, asks, label]);

  return (
    <div ref={containerRef} className="absolute inset-0">
      <canvas ref={canvasRef} className="block" />
    </div>
  );
}
