import ReactECharts from "echarts-for-react";
import type { SizeNode } from "../types";
import { EmptyChart } from "./EmptyChart";
import { chartPalette } from "../chartTheme";

export function SizeTreemap({ data, theme }: { data: SizeNode[]; theme?: string }) {
  if (!data || data.length === 0) {
    return (
      <EmptyChart
        glyph="◧"
        title="No storage telemetry"
        hint="Run 'Pull DMVs & analyze' against a live server to read sys.partitions + allocation_units and break down storage by schema → table → index."
      />
    );
  }
  type Tree = { name: string; value?: number; children?: Tree[]; raw?: SizeNode };
  const bySchema = new Map<string, Map<string, Tree[]>>();
  for (const d of data) {
    const sch = bySchema.get(d.schema) ?? new Map<string, Tree[]>();
    const tbl = sch.get(d.table) ?? [];
    tbl.push({
      name: `${d.index} · ${(d.reserved_kb / 1024).toFixed(1)} MB · ${d.row_count.toLocaleString()} rows`,
      value: d.reserved_kb,
      raw: d,
    });
    sch.set(d.table, tbl);
    bySchema.set(d.schema, sch);
  }
  const root: Tree[] = [...bySchema.entries()].map(([schema, sch]) => ({
    name: schema,
    children: [...sch.entries()].map(([table, indexes]) => ({ name: table, children: indexes })),
  }));
  const c = chartPalette(theme);
  const light = theme === "light";
  const option = {
    backgroundColor: "transparent",
    tooltip: {
      backgroundColor: c.panel, borderColor: c.lineStrong, borderWidth: 1,
      textStyle: { color: c.text, fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
      formatter: (p: any) => {
        const r: SizeNode | undefined = p.data?.raw;
        if (!r) return `<b>${p.name}</b>`;
        return `<b style="color:${c.signal}">${r.schema}.${r.table} · ${r.index}</b><br/>
                <span style="color:${c.textMuted}">reserved </span>${(r.reserved_kb / 1024)?.toLocaleString() ?? "unknown"} MB<br/>
                <span style="color:${c.textMuted}">used &nbsp; &nbsp;</span>${(r.used_kb / 1024)?.toLocaleString() ?? "unknown"} MB<br/>
                <span style="color:${c.textMuted}">data &nbsp; &nbsp;</span>${(r.data_kb / 1024)?.toLocaleString() ?? "unknown"} MB<br/>
                <span style="color:${c.textMuted}">rows &nbsp; &nbsp;</span>${r.row_count?.toLocaleString() ?? "unknown"}`;
      },
    },
    series: [
      {
        type: "treemap",
        data: root,
        leafDepth: 3,
        roam: false,
        breadcrumb: { show: true, height: 22, top: 6, left: 12, itemStyle: { color: c.cell, borderColor: c.lineStrong, textStyle: { color: c.textMuted } } },
        upperLabel: { show: true, height: 22, color: c.text, fontFamily: "IBM Plex Sans, sans-serif", fontSize: 11, fontWeight: 500 },
        label: {
          show: true,
          color: c.textStrong,
          fontFamily: "IBM Plex Sans, sans-serif",
          fontSize: 11,
          overflow: "truncate",
          ellipsis: { show: true },
          textBorderColor: light ? "rgba(255,255,255,0.65)" : "rgba(10,13,18,0.6)",
          textBorderWidth: 2,
        },
        levels: [
          { itemStyle: { gapWidth: 3, borderColor: c.bgBase } },
          { itemStyle: { gapWidth: 3, borderColor: c.bgBase }, colorSaturation: light ? [0.55, 0.75] : [0.32, 0.6] },
          { itemStyle: { gapWidth: 1, borderColor: c.bgBase }, colorSaturation: light ? [0.55, 0.75] : [0.32, 0.6] },
        ],
      },
    ],
  };
  return <ReactECharts key={theme} option={option} notMerge style={{ height: "100%", width: "100%" }} />;
}
