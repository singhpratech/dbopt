/**
 * Serve Monaco from the bundle, never from a CDN.
 *
 * `@monaco-editor/react` defaults to fetching monaco-editor from
 * cdn.jsdelivr.net at runtime — a dozen outbound requests on every page load,
 * which broke the "no network request of its own" promise (and the editor
 * itself on an air-gapped host). Pointing the loader at the npm package makes
 * Vite bundle it with the rest of the UI, so the backend binary serves it.
 */
// Editor core + features, then ONLY the languages the UI uses (sql, xml, json).
// The package root would also bundle the TypeScript/CSS/HTML services — ~10 MB
// of workers for languages no screen in dbopt ever edits.
import "monaco-editor/esm/vs/editor/editor.all";
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import "monaco-editor/esm/vs/basic-languages/sql/sql.contribution";
import "monaco-editor/esm/vs/basic-languages/xml/xml.contribution";
import "monaco-editor/esm/vs/language/json/monaco.contribution";
import { loader } from "@monaco-editor/react";
import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import JsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";

(self as any).MonacoEnvironment = {
  getWorker(_id: string, label: string) {
    // sql + xml are "basic languages" (tokenizer only) — the core editor
    // worker covers them; json has its own language service worker.
    return label === "json" ? new JsonWorker() : new EditorWorker();
  },
};

loader.config({ monaco });
