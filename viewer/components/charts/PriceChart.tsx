'use client';

import { useEffect, useRef } from 'react';
import { createChart, LineSeries, type IChartApi, type ISeriesApi, type UTCTimestamp, type Time, LineStyle } from 'lightweight-charts';
import { usePlaybackStore } from '@/stores/playback-store';
import { CHART_THEME } from '@/lib/theme';
import type { Frame } from '@/lib/types';

interface PriceChartProps {
  id: string;
  frames: Frame[];
  getValue: (f: Frame) => number;
  color: string;
  label: string;
  priceFormat?: (n: number) => string;
  strikePrice?: number | null;
  overlayGetValue?: (f: Frame) => number | null;
  overlayColor?: string;
  overlayLabel?: string;
  overlay2GetValue?: (f: Frame) => number | null;
  overlay2Color?: string;
  overlay2Label?: string;
}

export function PriceChart({ id, frames, getValue, color, label, priceFormat, strikePrice, overlayGetValue, overlayColor, overlayLabel, overlay2GetValue, overlay2Color, overlay2Label }: PriceChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const strikeSeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const overlaySeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const overlay2SeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);

  const getValueRef = useRef(getValue);
  getValueRef.current = getValue;
  const overlayGetValueRef = useRef(overlayGetValue);
  overlayGetValueRef.current = overlayGetValue;
  const overlay2GetValueRef = useRef(overlay2GetValue);
  overlay2GetValueRef.current = overlay2GetValue;

  const hasOverlay = !!overlayGetValue;
  const hasOverlay2 = !!overlay2GetValue;

  useEffect(() => {
    if (!containerRef.current) return;

    const chart = createChart(containerRef.current, {
      ...CHART_THEME,
      width: containerRef.current.clientWidth,
      height: containerRef.current.clientHeight,
      handleScroll: false,
      handleScale: false,
    });

    const series = chart.addSeries(LineSeries, {
      color,
      lineWidth: 1,
      priceLineVisible: false,
      lastValueVisible: true,
      crosshairMarkerVisible: true,
    });

    chartRef.current = chart;
    seriesRef.current = series;

    if (strikePrice != null) {
      const strikeSeries = chart.addSeries(LineSeries, {
        color: '#FBBF24',
        lineWidth: 1,
        lineStyle: LineStyle.Dashed,
        priceLineVisible: false,
        lastValueVisible: false,
        crosshairMarkerVisible: false,
      });
      strikeSeriesRef.current = strikeSeries;
    }

    if (hasOverlay) {
      const overlaySeries = chart.addSeries(LineSeries, {
        color: overlayColor ?? '#06B6D4',
        lineWidth: 1,
        lineStyle: LineStyle.Dotted,
        priceLineVisible: false,
        lastValueVisible: true,
        crosshairMarkerVisible: false,
        title: overlayLabel ?? 'FAIR',
      });
      overlaySeriesRef.current = overlaySeries;
    }

    if (hasOverlay2) {
      const overlay2Series = chart.addSeries(LineSeries, {
        color: overlay2Color ?? '#F97316',
        lineWidth: 1,
        lineStyle: LineStyle.Dotted,
        priceLineVisible: false,
        lastValueVisible: true,
        crosshairMarkerVisible: false,
        title: overlay2Label ?? 'BOOK',
      });
      overlay2SeriesRef.current = overlay2Series;
    }

    const resizeObserver = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) {
        chart.applyOptions({
          width: entry.contentRect.width,
          height: entry.contentRect.height,
        });
      }
    });
    resizeObserver.observe(containerRef.current);

    return () => {
      resizeObserver.disconnect();
      chart.remove();
      chartRef.current = null;
      seriesRef.current = null;
      strikeSeriesRef.current = null;
      overlaySeriesRef.current = null;
      overlay2SeriesRef.current = null;
    };
  }, [color, strikePrice, hasOverlay, overlayColor, overlayLabel, hasOverlay2, overlay2Color, overlay2Label]);

  useEffect(() => {
    let prevIdx = -1;
    const unsub = usePlaybackStore.subscribe((state) => {
      if (state.currentIndex === prevIdx) return;
      prevIdx = state.currentIndex;

      const series = seriesRef.current;
      const chart = chartRef.current;
      if (!series || !chart) return;

      const meta = state.meta;
      if (!meta) return;

      const currentGetValue = getValueRef.current;
      const currentOverlayGetValue = overlayGetValueRef.current;
      const currentOverlay2GetValue = overlay2GetValueRef.current;

      const idx = state.currentIndex;
      const data: { time: UTCTimestamp; value: number }[] = [];
      const overlayData: { time: UTCTimestamp; value: number }[] = [];
      const overlay2Data: { time: UTCTimestamp; value: number }[] = [];
      const baseTimeSec = meta.marketStart / 1000;
      for (let i = 0; i <= idx && i < frames.length; i++) {
        const val = currentGetValue(frames[i]);
        const time = Math.round(baseTimeSec + frames[i].t / 1000) as UTCTimestamp;
        if (val !== 0 && val != null) {
          if (data.length > 0 && data[data.length - 1].time === time) {
            data[data.length - 1].value = val;
          } else {
            data.push({ time, value: val });
          }
        }

        if (currentOverlayGetValue) {
          const ov = currentOverlayGetValue(frames[i]);
          if (ov != null && ov !== 0) {
            if (overlayData.length > 0 && overlayData[overlayData.length - 1].time === time) {
              overlayData[overlayData.length - 1].value = ov;
            } else {
              overlayData.push({ time, value: ov });
            }
          }
        }

        if (currentOverlay2GetValue) {
          const ov2 = currentOverlay2GetValue(frames[i]);
          if (ov2 != null && ov2 !== 0) {
            if (overlay2Data.length > 0 && overlay2Data[overlay2Data.length - 1].time === time) {
              overlay2Data[overlay2Data.length - 1].value = ov2;
            } else {
              overlay2Data.push({ time, value: ov2 });
            }
          }
        }
      }

      if (data.length > 0) {
        series.setData(data);

        if (strikeSeriesRef.current && strikePrice != null) {
          const firstTime = data[0].time;
          const lastTime = data[data.length - 1].time;
          if (firstTime === lastTime) {
            strikeSeriesRef.current.setData([
              { time: firstTime, value: strikePrice },
            ]);
          } else {
            strikeSeriesRef.current.setData([
              { time: firstTime, value: strikePrice },
              { time: lastTime, value: strikePrice },
            ]);
          }
        }

        if (overlaySeriesRef.current && overlayData.length > 0) {
          overlaySeriesRef.current.setData(overlayData);
        }

        if (overlay2SeriesRef.current && overlay2Data.length > 0) {
          overlay2SeriesRef.current.setData(overlay2Data);
        }

        chart.timeScale().scrollToPosition(0, false);
      }
    });

    return unsub;
  }, [frames, strikePrice]);

  return <div ref={containerRef} className="absolute inset-0" />;
}
