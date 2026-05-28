import ReactECharts from "echarts-for-react";
import type { SeverityBucket } from "../types";
import { EmptyChart } from "./EmptyChart";

export function SeverityBar({ data }: { data: SeverityBucket[] }) {
  if (!data || data.length === 0) {
    return <EmptyChart glyph="≡" title="No SQL to evaluate" hint="Paste T-SQL into the editor to see per-line severity distribution." />;
  }
  const lines = data.map((d) => `${d.line}`);
  const colors = { critical: "#ff3a4a", error: "#ff8c42", warning: "#ffd166", info: "#7fb4ff" };
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
      textStyle: { color: "#6b748a", fontFamily: "Departure Mono, monospace", fontSize: 10 },
      top: 8, right: 16, itemWidth: 10, itemHeight: 10,
    },
    tooltip: {
      trigger: "axis", axisPointer: { type: "shadow", shadowStyle: { color: "rgba(212,255,78,0.06)" } },
      backgroundColor: "#0e1219", borderColor: "#2a3142", borderWidth: 1,
      textStyle: { color: "#d6dbe5", fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
    },
    grid: { left: 48, right: 20, top: 36, bottom: 28 },
    xAxis: {
      type: "category", data: lines,
      axisLine: { lineStyle: { color: "#2a3142" } },
      axisTick: { show: false },
      axisLabel: { color: "#6b748a", interval: Math.max(0, Math.floor(lines.length / 40)), fontFamily: "Departure Mono, monospace", fontSize: 10 },
      splitLine: { show: false },
    },
    yAxis: {
      type: "value", minInterval: 1,
      axisLine: { lineStyle: { color: "#2a3142" } },
      axisLabel: { color: "#6b748a", fontFamily: "Departure Mono, monospace", fontSize: 10 },
      splitLine: { lineStyle: { color: "#131826" } },
    },
    series: [series("critical"), series("error"), series("warning"), series("info")],
  };
  return <ReactECharts option={option} style={{ height: "100%", width: "100%" }} />;
}
