import ReactECharts from "echarts-for-react";
import type { HeatmapCell } from "../types";
import { EmptyChart } from "./EmptyChart";
import { chartPalette } from "../chartTheme";

export function IndexHeatmap({ data, theme }: { data: HeatmapCell[]; theme?: string }) {
  if (!data || data.length === 0) {
    return (
      <EmptyChart
        glyph="◰"
        title="No index telemetry"
        hint="Connect to a SQL Server instance from the Connection workspace and run 'Pull DMVs & analyze' to populate sys.dm_db_index_usage_stats."
      />
    );
  }
  const c = chartPalette(theme);
  const rows = Array.from(new Set(data.map((d) => d.row))).sort();
  const cols = Array.from(new Set(data.map((d) => d.col))).sort();
  const series = data.map((d) => [cols.indexOf(d.col), rows.indexOf(d.row), d.score, d]);
  const max = Math.max(1, ...data.map((d) => Math.abs(d.score)));
  const option = {
    backgroundColor: "transparent",
    textStyle: { fontFamily: "IBM Plex Mono, monospace" },
    tooltip: {
      backgroundColor: c.panel, borderColor: c.lineStrong, borderWidth: 1,
      textStyle: { color: c.text, fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
      formatter: (p: any) => {
        const [, , , d] = p.value;
        return `<b style="color:${c.signal}">${d.row} · ${d.col}</b><br/>
                <span style="color:${c.textMuted}">seeks &nbsp; </span>${d.seeks.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">scans &nbsp; </span>${d.scans.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">lookups </span>${d.lookups.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">updates </span>${d.updates.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">score &nbsp; </span>${d.score.toLocaleString()}`;
      },
    },
    grid: { left: 240, right: 40, top: 24, bottom: 110 },
    xAxis: {
      type: "category", data: cols,
      axisLabel: { color: c.textMuted, rotate: 60, fontSize: 10, fontFamily: "Departure Mono, monospace" },
      axisLine: { lineStyle: { color: c.lineStrong } },
      axisTick: { show: false },
      splitArea: { show: false },
    },
    yAxis: {
      type: "category", data: rows,
      axisLabel: { color: c.textMuted, fontSize: 11, fontFamily: "IBM Plex Mono, monospace" },
      axisLine: { lineStyle: { color: c.lineStrong } },
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
      textStyle: { color: c.textMuted, fontFamily: "Departure Mono, monospace", fontSize: 10 },
      // diverging: heavy writes (red) → neutral → heavy reads (signal). The
      // neutral mid uses the elevated surface so near-zero cells still read as
      // filled boxes rather than vanishing into the background.
      inRange: { color: [c.crit, c.cell, c.signal] },
    },
    series: [
      {
        type: "heatmap",
        data: series,
        label: { show: false },
        // Visible hairline border on every cell so the grid is legible in BOTH
        // themes even when most cells are neutral (the old #06080c border was
        // darker than the dark background — the grid disappeared).
        itemStyle: { borderColor: c.lineStrong, borderWidth: 1 },
        emphasis: { itemStyle: { borderColor: c.signal, shadowBlur: 8, shadowColor: c.signal } },
      },
    ],
  };
  return <ReactECharts key={theme} option={option} notMerge style={{ height: "100%", width: "100%" }} />;
}
