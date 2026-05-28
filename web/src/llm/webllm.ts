import type { ChatMessage } from "../api/backend";

export const LIST = [
  "gemma-2-2b-it-q4f16_1-MLC",
  "Llama-3.2-3B-Instruct-q4f16_1-MLC",
  "Qwen2.5-3B-Instruct-q4f16_1-MLC",
];

let engine: any = null;
let loading = false;
let progressCb: ((p: string) => void) | null = null;

export function isSupported(): boolean {
  return typeof (navigator as any).gpu !== "undefined";
}

export function onProgress(cb: (p: string) => void) {
  progressCb = cb;
}

async function ensureEngine(model: string): Promise<any> {
  if (engine && engine.__model === model) return engine;
  if (loading) throw new Error("LLM is still loading");
  loading = true;
  try {
    const mod = await import("@mlc-ai/web-llm");
    const eng = await mod.CreateMLCEngine(model, {
      initProgressCallback: (r: any) => {
        if (progressCb) progressCb(`${(r.progress * 100).toFixed(1)}% — ${r.text}`);
      },
    });
    (eng as any).__model = model;
    engine = eng;
    return eng;
  } finally {
    loading = false;
  }
}

export async function chatStream(
  model: string,
  messages: ChatMessage[],
  onToken: (s: string) => void,
): Promise<void> {
  const eng = await ensureEngine(model);
  const reply = await eng.chat.completions.create({
    stream: true,
    messages,
  });
  for await (const chunk of reply) {
    const piece = chunk?.choices?.[0]?.delta?.content;
    if (piece) onToken(piece);
  }
}
