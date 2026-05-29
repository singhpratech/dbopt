import ReactECharts from "echarts-for-react";
import type { SeverityBucket } from "../types";
import { EmptyChart } from "./EmptyChart";
import { chartPalette } from "../chartTheme";

export function SeverityBar({
  data,
  theme,
  action,
}: {
  data: SeverityBucket[];
  theme?: string;
  action?: { label: string; onClick: () => void };
}) {
  if (!data || data.length === 0) {
    return <EmptyChart glyph="≡" title="No SQL to evaluate" hint="Paste T-SQL into the editor to see per-line severity distribution." action={action} />;
  }
  const c = chartPalette(theme);
  const lines = data.map((d) => `${d.line}`);
  const colors = { critical: c.crit, error: c.err, warning: c.warn, info: c.info };
  const series = (key: keyof typeof colors) => ({
    name: key.toUpperCase(),
    type: "bar",
    stack: "sev",
    itemStyle: { color: colors[key] },
    emphasis: { focus: "series" },
    data: data.map((d) => d[key as keyof SeverityBucket] as number),
    barWidth: "85%",
  });
  const option = {
    backgroundColor: "transparent",
    legend: {
      textStyle: { color: c.textMuted, fontFamily: "Departure Mono, monospace", fontSize: 10 },
      top: 8, right: 16, itemWidth: 10, itemHeight: 10,
    },
    tooltip: {
      trigger: "axis", axisPointer: { type: "shadow", shadowStyle: { color: c.signal + "14" } },
      backgroundColor: c.panel, borderColor: c.lineStrong, borderWidth: 1,
      textStyle: { color: c.text, fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
    },
    grid: { left: 48, right: 20, top: 36, bottom: 38 },
    xAxis: {
      type: "category", data: lines,
      axisLine: { lineStyle: { color: c.lineStrong } },
      axisTick: { show: false },
      axisLabel: { color: c.textMuted, interval: Math.max(0, Math.floor(lines.length / 40)), fontFamily: "Departure Mono, monospace", fontSize: 10 },
      splitLine: { show: false },
    },
    yAxis: {
      type: "value", minInterval: 1,
      axisLine: { lineStyle: { color: c.lineStrong } },
      axisLabel: { color: c.textMuted, fontFamily: "Departure Mono, monospace", fontSize: 10 },
      splitLine: { lineStyle: { color: c.lineSoft } },
    },
    series: [series("critical"), series("error"), series("warning"), series("info")],
  };
  return <ReactECharts key={theme} option={option} notMerge style={{ height: "100%", width: "100%" }} />;
}
