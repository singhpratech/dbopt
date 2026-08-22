import ReactECharts from "echarts-for-react";
import type { HeatmapCell } from "../types";
import { EmptyChart } from "./EmptyChart";
import { chartPalette } from "../chartTheme";

export function IndexHeatmap({
  data,
  theme,
  action,
  loading,
  error,
}: {
  data: HeatmapCell[];
  theme?: string;
  action?: { label: string; onClick: () => void };
  loading?: boolean;
  error?: string | null;
}) {
  if (!data || data.length === 0) {
    return (
      <EmptyChart
        glyph="◰"
        title="No index telemetry"
        hint="Pull live DMVs to populate sys.dm_db_index_usage_stats — reads vs writes per index, in place."
        action={action}
        loading={loading}
        error={error}
      />
    );
  }
  const c = chartPalette(theme);
  const rows = data
    .map((d) => ({ ...d, reads: d.seeks + d.scans + d.lookups, writes: d.updates }))
    .sort((a, b) => b.reads + b.writes - (a.reads + a.writes) || b.score - a.score);
  const labels = rows.map((d) => `${d.row} · ${d.col}`);

  const total = rows.reduce((s, d) => s + d.reads + d.writes, 0);
  if (total === 0) {
    return (
      <EmptyChart
        glyph="◰"
        title="No index usage yet"
        hint="All usage counters are zero. sys.dm_db_index_usage_stats resets on SQL Server restart or index rebuild — let the workload run, then re-pull DMVs."
        action={action}
        loading={loading}
        error={error}
      />
    );
  }

  const option = {
    backgroundColor: "transparent",
    legend: {
      data: ["READS", "WRITES"],
      top: 8,
      right: 16,
      itemWidth: 10,
      itemHeight: 10,
      textStyle: { color: c.textMuted, fontFamily: "IBM Plex Mono, monospace", fontSize: 10 },
    },
    tooltip: {
      trigger: "item",
      backgroundColor: c.panel,
      borderColor: c.lineStrong,
      borderWidth: 1,
      textStyle: { color: c.text, fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
      formatter: (p: any) => {
        const d = rows[p.dataIndex];
        return `<b style="color:${c.signal}">${d.row} · ${d.col}</b><br/>
                <span style="color:${c.textMuted}">seeks &nbsp; </span>${d.seeks.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">scans &nbsp; </span>${d.scans.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">lookups </span>${d.lookups.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">updates </span>${d.updates.toLocaleString()}<br/>
                <span style="color:${c.textMuted}">score &nbsp; </span>${d.score.toLocaleString()}`;
      },
    },
    grid: { left: 312, right: 48, top: 36, bottom: 40 },
    xAxis: {
      type: "value",
      name: "← writes   reads →",
      nameLocation: "middle",
      nameGap: 26,
      nameTextStyle: { color: c.textMuted, fontFamily: "IBM Plex Mono, monospace", fontSize: 10 },
      axisLabel: {
        color: c.text,
        fontFamily: "IBM Plex Mono, monospace",
        fontSize: 10,
        formatter: (v: number) => Math.abs(v).toLocaleString(),
      },
      axisLine: { lineStyle: { color: c.lineStrong } },
      splitLine: { lineStyle: { color: c.lineSoft } },
    },
    yAxis: {
      type: "category",
      data: labels,
      inverse: true,
      // Wider grid.left + a label width so long "table · index" names fit; if one
      // still overruns it truncates with an ellipsis at the END (table name stays
      // visible) instead of clipping the start off the left edge. Full name on hover.
      axisLabel: { color: c.text, fontFamily: "IBM Plex Mono, monospace", fontSize: 11, width: 300, overflow: "truncate" },
      axisLine: { lineStyle: { color: c.lineStrong } },
      axisTick: { show: false },
    },
    series: [
      {
        name: "WRITES",
        type: "bar",
        stack: "io",
        itemStyle: { color: c.crit },
        emphasis: { focus: "series" },
        barMaxWidth: 16,
        data: rows.map((d) => -d.writes),
      },
      {
        name: "READS",
        type: "bar",
        stack: "io",
        itemStyle: { color: c.signal },
        emphasis: { focus: "series" },
        barMaxWidth: 16,
        data: rows.map((d) => ({
          value: d.reads,
          label:
            d.reads === 0 && d.writes > 0
              ? {
                  show: true,
                  position: "right",
                  color: c.warn,
                  fontFamily: "IBM Plex Mono, monospace",
                  fontSize: 9,
                  formatter: "DROP?",
                }
              : { show: false },
        })),
      },
    ],
    dataZoom:
      rows.length > 30
        ? [
            {
              type: "slider",
              yAxisIndex: 0,
              right: 8,
              width: 12,
              start: 0,
              end: Math.min(100, (30 / rows.length) * 100),
              brushSelect: false,
            },
            { type: "inside", yAxisIndex: 0 },
          ]
        : undefined,
  };
  return <ReactECharts key={theme} option={option} notMerge style={{ height: "100%", width: "100%" }} />;
}
