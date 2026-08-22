import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { applyTheme, loadTheme } from "./store/persist";
import "./monaco-local";
import "./fonts.css";
import "./index.css";

// Apply the persisted theme before first paint so there's no dark→light flash.
applyTheme(loadTheme());

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
