import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";

export const SCHEDULED_SYNC_EVENT = "scheduled-sync";

export async function invokeParsed<T>(command: string, schema: z.ZodType<T>, args?: Record<string, unknown>): Promise<T> {
  const payload = args === undefined ? await invoke<unknown>(command) : await invoke<unknown>(command, args);
  return schema.parse(payload);
}

export function errorText(reason: unknown): string {
  return reason instanceof z.ZodError ? `Agent Plugins returned invalid data: ${z.prettifyError(reason)}` : String(reason);
}
