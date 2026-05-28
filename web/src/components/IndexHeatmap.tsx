import ReactECharts from "echarts-for-react";
import type { HeatmapCell } from "../types";
import { EmptyChart } from "./EmptyChart";

export function IndexHeatmap({ data }: { data: HeatmapCell[] }) {
  if (!data || data.length === 0) {
    return (
      <EmptyChart
        glyph="◰"
        title="No index telemetry"
        hint="Connect to a SQL Server instance from the Connection workspace and run 'Pull DMVs & analyze' to populate sys.dm_db_index_usage_stats."
      />
    );
  }
  const rows = Array.from(new Set(data.map((d) => d.row))).sort();
  const cols = Array.from(new Set(data.map((d) => d.col))).sort();
  const series = data.map((d) => [cols.indexOf(d.col), rows.indexOf(d.row), d.score, d]);
  const max = Math.max(1, ...data.map((d) => Math.abs(d.score)));
  const option = {
    backgroundColor: "transparent",
    textStyle: { fontFamily: "IBM Plex Mono, monospace" },
    tooltip: {
      backgroundColor: "#0e1219", borderColor: "#2a3142", borderWidth: 1,
      textStyle: { color: "#d6dbe5", fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
      formatter: (p: any) => {
        const [, , , d] = p.value;
        return `<b style="color:#d4ff4e">${d.row} · ${d.col}</b><br/>
                <span style="color:#6b748a">seeks &nbsp; </span>${d.seeks.toLocaleString()}<br/>
                <span style="color:#6b748a">scans &nbsp; </span>${d.scans.toLocaleString()}<br/>
                <span style="color:#6b748a">lookups </span>${d.lookups.toLocaleString()}<br/>
                <span style="color:#6b748a">updates </span>${d.updates.toLocaleString()}<br/>
                <span style="color:#6b748a">score &nbsp; </span>${d.score.toLocaleString()}`;
      },
    },
    grid: { left: 240, right: 40, top: 24, bottom: 110 },
    xAxis: {
      type: "category", data: cols,
      axisLabel: { color: "#6b748a", rotate: 60, fontSize: 10, fontFamily: "Departure Mono, monospace" },
      axisLine: { lineStyle: { color: "#2a3142" } },
      axisTick: { show: false },
      splitArea: { show: false },
    },
    yAxis: {
      type: "category", data: rows,
      axisLabel: { color: "#6b748a", fontSize: 11, fontFamily: "IBM Plex Mono, monospace" },
      axisLine: { lineStyle: { color: "#2a3142" } },
      axisTick: { show: false },
      splitArea: { show: false },
    },
    visualMap: {
      min: -max,
      max,
      calculable: true,
      orient: "horizontal",
      left: "center",
      bottom: 18,
      itemHeight: 100,
      itemWidth: 12,
      textStyle: { color: "#6b748a", fontFamily: "Departure Mono, monospace", fontSize: 10 },
      inRange: { color: ["#ff3a4a", "#1a2030", "#d4ff4e"] },
    },
    series: [
      {
        type: "heatmap",
        data: series,
        label: { show: false },
        itemStyle: { borderColor: "#06080c", borderWidth: 1 },
        emphasis: { itemStyle: { shadowBlur: 8, shadowColor: "rgba(212, 255, 78, 0.4)" } },
      },
    ],
  };
  return <ReactECharts option={option} style={{ height: "100%", width: "100%" }} />;
}
