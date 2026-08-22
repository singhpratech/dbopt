/// <reference types="vite/client" />

// monaco-editor's root types ARE editor.api.d.ts; the esm subpath just lacks a
// types mapping under "bundler" resolution, so alias it to the root.
declare module "monaco-editor/esm/vs/editor/editor.api" {
  export * from "monaco-editor";
}
