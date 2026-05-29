import ReactECharts from "echarts-for-react";
import type { TreemapNode } from "../types";
import { EmptyChart } from "./EmptyChart";
import { chartPalette } from "../chartTheme";

export function PlanTreemap({
  data,
  theme,
  action,
}: {
  data: TreemapNode[];
  theme?: string;
  action?: { label: string; onClick: () => void };
}) {
  if (!data || data.length === 0) {
    return (
      <EmptyChart
        glyph="◫"
        title="No execution plan"
        hint="Drop a SQL Server .sqlplan XML file (from SSMS → 'Save Execution Plan As') to visualize per-operator estimated cost."
        action={action}
      />
    );
  }
  const c = chartPalette(theme);
  const option = {
    backgroundColor: "transparent",
    tooltip: {
      backgroundColor: c.panel,
      borderColor: c.lineStrong,
      borderWidth: 1,
      textStyle: { color: c.text, fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
      formatter: (p: any) => {
        const d = p.data;
        return `<b style="color:${c.signal}">${d.physical_op}</b><br/>
                <span style="color:${c.textMuted}">logical</span>&nbsp; ${d.logical_op}<br/>
                <span style="color:${c.textMuted}">cost</span>&nbsp;&nbsp;&nbsp; ${Number(d.value).toFixed(4)}<br/>
                <span style="color:${c.textMuted}">rows</span>&nbsp;&nbsp;&nbsp; ${Math.round(d.estimated_rows).toLocaleString()}`;
      },
    },
    series: [
      {
        type: "treemap",
        data,
        roam: false,
        breadcrumb: {
          show: true, height: 22, top: 6, left: 12,
          itemStyle: { color: c.cell, borderColor: c.signal, textStyle: { color: c.text, fontFamily: "Departure Mono, monospace", fontSize: 10 } },
          emphasis: { itemStyle: { color: c.panel, textStyle: { color: c.signal } } },
        },
        upperLabel: {
          show: true, height: 22, color: c.text,
          fontFamily: "IBM Plex Sans, sans-serif", fontSize: 11, fontWeight: 500,
          padding: [3, 8],
        },
        label: {
          show: true, color: c.textStrong,
          fontFamily: "IBM Plex Sans, sans-serif", fontSize: 12, fontWeight: 500,
        },
        levels: [
          { itemStyle: { borderColor: c.bgBase, borderWidth: 0, gapWidth: 1 } },
          { itemStyle: { borderColor: c.bgBase, borderWidth: 3, gapWidth: 3 }, emphasis: { itemStyle: { borderColor: c.signal } } },
          { itemStyle: { borderColor: c.bgBase, borderWidth: 1, gapWidth: 1 }, colorSaturation: [0.35, 0.55] },
          { colorSaturation: [0.3, 0.5], itemStyle: { borderWidth: 1 } },
        ],
      },
    ],
  };
  return <ReactECharts key={theme} option={option} notMerge style={{ height: "100%", width: "100%" }} />;
}
