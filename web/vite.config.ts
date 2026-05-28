import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    fs: { allow: [".."] },
    proxy: {
      "/api": {
        target: "http://127.0.0.1:3690",
        changeOrigin: true,
        ws: true,
      },
    },
  },
  optimizeDeps: { exclude: ["@mlc-ai/web-llm"] },
  worker: { format: "es" },
});
