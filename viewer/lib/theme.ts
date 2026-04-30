export const COLORS = {
  bg: '#0A0F1E',
  surface: '#111827',
  elevated: '#1E293B',
  border: '#1E293B',
  borderLight: '#334155',
  text: '#F8FAFC',
  textMuted: '#94A3B8',
  textDim: '#64748B',
  green: '#22C55E',
  red: '#EF4444',
  blue: '#3B82F6',
  yellow: '#FBBF24',
} as const;

export const CHART_THEME = {
  layout: {
    background: { color: 'transparent' },
    textColor: COLORS.textMuted,
    fontSize: 11,
    fontFamily: 'JetBrains Mono, monospace',
  },
  grid: {
    vertLines: { color: '#1E293B40' },
    horzLines: { color: '#1E293B40' },
  },
  crosshair: {
    vertLine: { color: COLORS.blue, width: 1 as const, style: 2 as const },
    horzLine: { color: COLORS.blue, width: 1 as const, style: 2 as const },
  },
  timeScale: {
    borderColor: COLORS.border,
    timeVisible: true,
    secondsVisible: true,
  },
  rightPriceScale: {
    borderColor: COLORS.border,
  },
} as const;
