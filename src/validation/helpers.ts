import type { ZodError, ZodType } from "zod";

export function zodErrorMessage(error: ZodError): string {
  return error.issues[0]?.message ?? "Invalid input";
}

export function zodFieldErrors(error: ZodError): Record<string, string> {
  const out: Record<string, string> = {};
  for (const issue of error.issues) {
    const key = issue.path.map(String).join(".");
    if (!key || key in out) continue;
    out[key] = issue.message;
  }
  return out;
}

export function parseSchema<T>(schema: ZodType<T>, data: unknown):
  | { success: true; data: T }
  | { success: false; message: string; fieldErrors: Record<string, string> } {
  const result = schema.safeParse(data);
  if (result.success) return { success: true, data: result.data };
  return {
    success: false,
    message: zodErrorMessage(result.error),
    fieldErrors: zodFieldErrors(result.error),
  };
}
