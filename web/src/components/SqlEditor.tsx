import Editor, { OnMount } from "@monaco-editor/react";
import { useRef } from "react";

export interface SqlEditorHandle {
  jumpTo: (line: number, col: number) => void;
}

export function SqlEditor({
  value,
  onChange,
  handleRef,
  language = "sql",
}: {
  value: string;
  onChange: (v: string) => void;
  handleRef?: (h: SqlEditorHandle) => void;
  language?: "sql" | "xml" | "json";
}) {
  const editorRef = useRef<any>(null);
  const onMount: OnMount = (editor) => {
    editorRef.current = editor;
    handleRef?.({
      jumpTo: (line, col) => {
        editor.revealPositionInCenter({ lineNumber: line, column: col });
        editor.setPosition({ lineNumber: line, column: col });
        editor.focus();
      },
    });
  };
  return (
    <Editor
      value={value}
      language={language}
      onChange={(v) => onChange(v ?? "")}
      onMount={onMount}
      theme="vs-dark"
      options={{
        fontFamily: "'JetBrains Mono','SF Mono',monospace",
        fontSize: 13,
        minimap: { enabled: false },
        lineNumbers: "on",
        wordWrap: "on",
        tabSize: 2,
        renderWhitespace: "selection",
        scrollBeyondLastLine: false,
      }}
    />
  );
}
