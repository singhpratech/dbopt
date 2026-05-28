import ReactECharts from "echarts-for-react";
import type { SizeNode } from "../types";
import { EmptyChart } from "./EmptyChart";

export function SizeTreemap({ data }: { data: SizeNode[] }) {
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
  const option = {
    backgroundColor: "transparent",
    tooltip: {
      backgroundColor: "#0e1219", borderColor: "#2a3142", borderWidth: 1,
      textStyle: { color: "#d6dbe5", fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
      formatter: (p: any) => {
        const r: SizeNode | undefined = p.data?.raw;
        if (!r) return `<b>${p.name}</b>`;
        return `<b style="color:#d4ff4e">${r.schema}.${r.table} · ${r.index}</b><br/>
                <span style="color:#6b748a">reserved </span>${(r.reserved_kb / 1024).toFixed(1)} MB<br/>
                <span style="color:#6b748a">used &nbsp; &nbsp;</span>${(r.used_kb / 1024).toFixed(1)} MB<br/>
                <span style="color:#6b748a">data &nbsp; &nbsp;</span>${(r.data_kb / 1024).toFixed(1)} MB<br/>
                <span style="color:#6b748a">rows &nbsp; &nbsp;</span>${r.row_count.toLocaleString()}`;
      },
    },
    series: [
      {
        type: "treemap",
        data: root,
        leafDepth: 3,
        roam: false,
        breadcrumb: { show: true, height: 22, top: 6, left: 12, itemStyle: { color: "#1a2030", borderColor: "#2a3142", textStyle: { color: "#6b748a" } } },
        upperLabel: { show: true, height: 22, color: "#d6dbe5", fontFamily: "IBM Plex Sans, sans-serif", fontSize: 11, fontWeight: 500 },
        label: { show: true, color: "#f0f2f7", fontFamily: "IBM Plex Sans, sans-serif", fontSize: 11 },
        levels: [
          { itemStyle: { gapWidth: 3, borderColor: "#06080c" } },
          { itemStyle: { gapWidth: 3, borderColor: "#06080c" }, colorSaturation: [0.32, 0.58] },
          { itemStyle: { gapWidth: 1, borderColor: "#06080c" }, colorSaturation: [0.32, 0.6] },
        ],
      },
    ],
  };
  return <ReactECharts option={option} style={{ height: "100%", width: "100%" }} />;
}
