'use client';

import { useEffect, useRef, useState, useCallback } from 'react';
import { createChart, LineSeries, type IChartApi, type ISeriesApi, type UTCTimestamp, type Time, LineStyle } from 'lightweight-charts';
import { useLiveStore } from '@/stores/live-store';
import { CHART_THEME } from '@/lib/theme';
import type { Frame } from '@/lib/types';

interface LivePriceChartProps {
  id: string;
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
  overlay3GetValue?: (f: Frame) => number | null;
  overlay3Color?: string;
  overlay3Label?: string;
  overlay4GetValue?: (f: Frame) => number | null;
  overlay4Color?: string;
  overlay4Label?: string;
}

export function LivePriceChart({ id, getValue, color, label, priceFormat, strikePrice, overlayGetValue, overlayColor, overlayLabel, overlay2GetValue, overlay2Color, overlay2Label, overlay3GetValue, overlay3Color, overlay3Label, overlay4GetValue, overlay4Color, overlay4Label }: LivePriceChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const strikeSeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const overlaySeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const overlay2SeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const overlay3SeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const overlay4SeriesRef = useRef<ISeriesApi<'Line', Time> | null>(null);
  const lastFrameCountRef = useRef(0);
  const isFollowingRef = useRef(true);
  const [isFollowing, setIsFollowing] = useState(true);

  const getValueRef = useRef(getValue);
  getValueRef.current = getValue;
  const overlayGetValueRef = useRef(overlayGetValue);
  overlayGetValueRef.current = overlayGetValue;
  const overlay2GetValueRef = useRef(overlay2GetValue);
  overlay2GetValueRef.current = overlay2GetValue;
  const overlay3GetValueRef = useRef(overlay3GetValue);
  overlay3GetValueRef.current = overlay3GetValue;
  const overlay4GetValueRef = useRef(overlay4GetValue);
  overlay4GetValueRef.current = overlay4GetValue;

  const hasOverlay = !!overlayGetValue;
  const hasOverlay2 = !!overlay2GetValue;
  const hasOverlay3 = !!overlay3GetValue;
  const hasOverlay4 = !!overlay4GetValue;

  useEffect(() => {
    if (!containerRef.current) return;

    const chart = createChart(containerRef.current, {
      ...CHART_THEME,
      width: containerRef.current.clientWidth,
      height: containerRef.current.clientHeight,
      handleScroll: true,
      handleScale: true,
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

    if (hasOverlay3) {
      const overlay3Series = chart.addSeries(LineSeries, {
        color: overlay3Color ?? '#A855F7',
        lineWidth: 1,
        lineStyle: LineStyle.Dotted,
        priceLineVisible: false,
        lastValueVisible: true,
        crosshairMarkerVisible: false,
        title: overlay3Label ?? 'COLL',
      });
      overlay3SeriesRef.current = overlay3Series;
    }

    if (hasOverlay4) {
      const overlay4Series = chart.addSeries(LineSeries, {
        color: overlay4Color ?? '#EC4899',
        lineWidth: 1,
        lineStyle: LineStyle.Dotted,
        priceLineVisible: false,
        lastValueVisible: true,
        crosshairMarkerVisible: false,
        title: overlay4Label ?? 'LSTM',
      });
      overlay4SeriesRef.current = overlay4Series;
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
      overlay3SeriesRef.current = null;
      overlay4SeriesRef.current = null;
    };
  }, [color, strikePrice, hasOverlay, overlayColor, overlayLabel, hasOverlay2, overlay2Color, overlay2Label, hasOverlay3, overlay3Color, overlay3Label, hasOverlay4, overlay4Color, overlay4Label]);

  useEffect(() => {
    lastFrameCountRef.current = 0;

    const unsub = useLiveStore.subscribe((state) => {
      const series = seriesRef.current;
      const chart = chartRef.current;
      if (!series || !chart) return;

      const { frames, epoch } = state;
      if (!epoch || frames.length === 0) {
        if (lastFrameCountRef.current > 0) {
          series.setData([]);
          strikeSeriesRef.current?.setData([]);
          overlaySeriesRef.current?.setData([]);
          overlay2SeriesRef.current?.setData([]);
          overlay3SeriesRef.current?.setData([]);
          overlay4SeriesRef.current?.setData([]);
          lastFrameCountRef.current = 0;
        }
        return;
      }

      const currentGetValue = getValueRef.current;
      const currentOverlayGetValue = overlayGetValueRef.current;
      const currentOverlay2GetValue = overlay2GetValueRef.current;
      const currentOverlay3GetValue = overlay3GetValueRef.current;
      const currentOverlay4GetValue = overlay4GetValueRef.current;
      const baseTimeSec = epoch;

      if (frames.length < lastFrameCountRef.current) {
        series.setData([]);
        strikeSeriesRef.current?.setData([]);
        overlaySeriesRef.current?.setData([]);
        overlay2SeriesRef.current?.setData([]);
        overlay3SeriesRef.current?.setData([]);
        lastFrameCountRef.current = 0;
      }

      const startIdx = Math.max(0, lastFrameCountRef.current);

      if (startIdx === 0) {
        const data: { time: UTCTimestamp; value: number }[] = [];
        const overlayData: { time: UTCTimestamp; value: number }[] = [];
        const overlay2Data: { time: UTCTimestamp; value: number }[] = [];
        const overlay3Data: { time: UTCTimestamp; value: number }[] = [];
        const overlay4Data: { time: UTCTimestamp; value: number }[] = [];

        for (let i = 0; i < frames.length; i++) {
          const f = frames[i];
          const time = Math.round(baseTimeSec + f.t / 1000) as UTCTimestamp;
          const val = currentGetValue(f);
          if (val !== 0 && val != null) {
            if (data.length > 0 && data[data.length - 1].time === time) {
              data[data.length - 1].value = val;
            } else {
              data.push({ time, value: val });
            }
          }
          if (currentOverlayGetValue) {
            const ov = currentOverlayGetValue(f);
            if (ov != null && ov !== 0) {
              if (overlayData.length > 0 && overlayData[overlayData.length - 1].time === time) {
                overlayData[overlayData.length - 1].value = ov;
              } else {
                overlayData.push({ time, value: ov });
              }
            }
          }
          if (currentOverlay2GetValue) {
            const ov2 = currentOverlay2GetValue(f);
            if (ov2 != null && ov2 !== 0) {
              if (overlay2Data.length > 0 && overlay2Data[overlay2Data.length - 1].time === time) {
                overlay2Data[overlay2Data.length - 1].value = ov2;
              } else {
                overlay2Data.push({ time, value: ov2 });
              }
            }
          }
          if (currentOverlay3GetValue) {
            const ov3 = currentOverlay3GetValue(f);
            if (ov3 != null && ov3 !== 0) {
              if (overlay3Data.length > 0 && overlay3Data[overlay3Data.length - 1].time === time) {
                overlay3Data[overlay3Data.length - 1].value = ov3;
              } else {
                overlay3Data.push({ time, value: ov3 });
              }
            }
          }
          if (currentOverlay4GetValue) {
            const ov4 = currentOverlay4GetValue(f);
            if (ov4 != null && ov4 !== 0) {
              if (overlay4Data.length > 0 && overlay4Data[overlay4Data.length - 1].time === time) {
                overlay4Data[overlay4Data.length - 1].value = ov4;
              } else {
                overlay4Data.push({ time, value: ov4 });
              }
            }
          }
        }

        if (data.length > 0) {
          series.setData(data);
          if (strikeSeriesRef.current && strikePrice != null) {
            strikeSeriesRef.current.setData([
              { time: data[0].time, value: strikePrice },
              { time: data[data.length - 1].time, value: strikePrice },
            ]);
          }
          if (overlaySeriesRef.current && overlayData.length > 0) overlaySeriesRef.current.setData(overlayData);
          if (overlay2SeriesRef.current && overlay2Data.length > 0) overlay2SeriesRef.current.setData(overlay2Data);
          if (overlay3SeriesRef.current && overlay3Data.length > 0) overlay3SeriesRef.current.setData(overlay3Data);
          if (overlay4SeriesRef.current && overlay4Data.length > 0) overlay4SeriesRef.current.setData(overlay4Data);
        }
      } else {
        for (let i = startIdx; i < frames.length; i++) {
          const f = frames[i];
          const time = Math.round(baseTimeSec + f.t / 1000) as UTCTimestamp;
          const val = currentGetValue(f);
          if (val !== 0 && val != null) {
            series.update({ time, value: val });
          }
          if (currentOverlayGetValue && overlaySeriesRef.current) {
            const ov = currentOverlayGetValue(f);
            if (ov != null && ov !== 0) overlaySeriesRef.current.update({ time, value: ov });
          }
          if (currentOverlay2GetValue && overlay2SeriesRef.current) {
            const ov2 = currentOverlay2GetValue(f);
            if (ov2 != null && ov2 !== 0) overlay2SeriesRef.current.update({ time, value: ov2 });
          }
          if (currentOverlay3GetValue && overlay3SeriesRef.current) {
            const ov3 = currentOverlay3GetValue(f);
            if (ov3 != null && ov3 !== 0) overlay3SeriesRef.current.update({ time, value: ov3 });
          }
          if (currentOverlay4GetValue && overlay4SeriesRef.current) {
            const ov4 = currentOverlay4GetValue(f);
            if (ov4 != null && ov4 !== 0) overlay4SeriesRef.current.update({ time, value: ov4 });
          }
          if (strikeSeriesRef.current && strikePrice != null) {
            strikeSeriesRef.current.update({ time, value: strikePrice });
          }
        }
      }

      lastFrameCountRef.current = frames.length;

      const scrollPos = chart.timeScale().scrollPosition();
      const following = scrollPos >= -2;
      if (following !== isFollowingRef.current) {
        isFollowingRef.current = following;
        setIsFollowing(following);
      }
      if (following) {
        chart.timeScale().scrollToPosition(0, false);
      }
    });

    return unsub;
  }, [strikePrice]);

  const handleBackToLive = useCallback(() => {
    isFollowingRef.current = true;
    setIsFollowing(true);
    chartRef.current?.timeScale().scrollToRealTime();
  }, []);

  return (
    <div className="absolute inset-0">
      <div ref={containerRef} className="absolute inset-0" />
      {!isFollowing && (
        <button
          onClick={handleBackToLive}
          className="absolute top-2 right-2 z-10 flex items-center gap-1.5 px-2 py-1 rounded bg-[#1E293B]/90 border border-[#334155] text-[10px] font-bold tracking-wider cursor-pointer hover:bg-[#334155]/90 transition-colors"
          style={{ color: '#3B82F6' }}
        >
          <span>&#8595;</span> BACK TO LIVE
        </button>
      )}
    </div>
  );
}
