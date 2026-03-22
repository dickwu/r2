'use client';

/** Inline SVG sparkline from speed history samples */
export default function Sparkline({
  data,
  width = 60,
  height = 16,
  style,
}: {
  data: number[];
  width?: number;
  height?: number;
  style?: React.CSSProperties;
}) {
  if (data.length < 2) return null;
  const max = Math.max(...data, 1);
  const points = data
    .map((v, i) => {
      const x = (i / (data.length - 1)) * width;
      const y = height - (v / max) * height;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(' ');

  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} style={style}>
      <polyline
        points={points}
        fill="none"
        stroke="var(--color-link)"
        strokeWidth="1.5"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}
