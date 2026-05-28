import ReactECharts from "echarts-for-react";
import type { TreemapNode } from "../types";
import { EmptyChart } from "./EmptyChart";

export function PlanTreemap({ data }: { data: TreemapNode[] }) {
  if (!data || data.length === 0) {
    return (
      <EmptyChart
        glyph="◫"
        title="No execution plan"
        hint="Drop a SQL Server .sqlplan XML file (from SSMS → 'Save Execution Plan As') to visualize per-operator estimated cost."
      />
    );
  }
  const option = {
    backgroundColor: "transparent",
    tooltip: {
      backgroundColor: "#0e1219",
      borderColor: "#2a3142",
      borderWidth: 1,
      textStyle: { color: "#d6dbe5", fontFamily: "IBM Plex Mono, monospace", fontSize: 11 },
      formatter: (p: any) => {
        const d = p.data;
        return `<b style="color:#d4ff4e">${d.physical_op}</b><br/>
                <span style="color:#6b748a">logical</span>&nbsp; ${d.logical_op}<br/>
                <span style="color:#6b748a">cost</span>&nbsp;&nbsp;&nbsp; ${Number(d.value).toFixed(4)}<br/>
                <span style="color:#6b748a">rows</span>&nbsp;&nbsp;&nbsp; ${Math.round(d.estimated_rows).toLocaleString()}`;
      },
    },
    series: [
      {
        type: "treemap",
        data,
        roam: false,
        breadcrumb: {
          show: true, height: 22, top: 6, left: 12,
          itemStyle: { color: "#1a2030", borderColor: "#2a3142", textStyle: { color: "#6b748a", fontFamily: "Departure Mono, monospace", fontSize: 10 } },
          emphasis: { itemStyle: { color: "#131822", textStyle: { color: "#d4ff4e" } } },
        },
        upperLabel: {
          show: true, height: 22, color: "#d6dbe5",
          fontFamily: "IBM Plex Sans, sans-serif", fontSize: 11, fontWeight: 500,
          padding: [3, 8],
        },
        label: {
          show: true, color: "#f0f2f7",
          fontFamily: "IBM Plex Sans, sans-serif", fontSize: 12, fontWeight: 500,
        },
        levels: [
          { itemStyle: { borderColor: "#06080c", borderWidth: 0, gapWidth: 1 } },
          { itemStyle: { borderColor: "#06080c", borderWidth: 3, gapWidth: 3 }, emphasis: { itemStyle: { borderColor: "#d4ff4e" } } },
          { itemStyle: { borderColor: "#0a0d12", borderWidth: 1, gapWidth: 1 }, colorSaturation: [0.35, 0.55] },
          { colorSaturation: [0.3, 0.5], itemStyle: { borderWidth: 1 } },
        ],
      },
    ],
  };
  return <ReactECharts option={option} style={{ height: "100%", width: "100%" }} />;
}
