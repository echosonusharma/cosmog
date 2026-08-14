import { z } from "zod";

export const ACCOUNT_NAME_MAX_LENGTH = 64;

const CORS_ORIGIN_RE = /^(\*|https?:\/\/[A-Za-z0-9.\-*]+(:\d+)?)$/;
const MAX_AGE_LIMIT = 2_147_483_647;

export const accountNameSchema = z
  .string()
  .trim()
  .min(1, "Name is required")
  .max(ACCOUNT_NAME_MAX_LENGTH, `Name must be ${ACCOUNT_NAME_MAX_LENGTH} characters or less`);

/** Edit forms allow an existing long name but cap new input at the same length. */
export function accountNameSchemaForForm(opts: { isEdit: boolean; existingName?: string }) {
  const existingLen = opts.existingName?.trim().length ?? 0;
  const maxLen = opts.isEdit ? Math.max(ACCOUNT_NAME_MAX_LENGTH, existingLen) : ACCOUNT_NAME_MAX_LENGTH;
  return z
    .string()
    .trim()
    .min(1, "Name is required")
    .max(maxLen, `Name must be ${maxLen} characters or less`);
}

export const accessKeyIdSchema = z
  .string()
  .trim()
  .min(1, "Access Key ID is required")
  .max(256, "Access Key ID is too long");

export const secretAccessKeySchema = z
  .string()
  .trim()
  .min(1, "Secret Access Key is required")
  .max(256, "Secret Access Key is too long");

export const endpointUrlSchema = z
  .string()
  .trim()
  .min(1, "Endpoint is required")
  .url("Enter a valid endpoint URL");

export const bucketNameSchema = z
  .string()
  .trim()
  .min(1, "Bucket name is required")
  .max(63, "Bucket name must be 63 characters or less")
  .refine((name) => !/[/\\]/.test(name), "Bucket name cannot contain slashes")
  .refine((name) => !name.includes(".."), "Bucket name cannot contain consecutive periods");

export const folderPathSchema = z
  .string()
  .trim()
  .transform((path) => path.replace(/\/+/g, "/").replace(/^\//, "").replace(/\/$/, ""))
  .pipe(z.string().min(1, "Folder path is required").max(1024, "Folder path is too long"));

export const objectKeySchema = z
  .string()
  .trim()
  .transform((key) => key.replace(/^\/+/, ""))
  .pipe(z.string().min(1, "Path is required").max(1024, "Path is too long"));

export const downloadPathSchema = z
  .string()
  .trim()
  .min(1, "Save location is required");

export const uploadKeyPrefixSchema = z.string().max(1024, "Key prefix is too long");

export const reauthSecretSchema = secretAccessKeySchema;

export const settingsPatchSchema = z.object({
  default_download_dir: z.string().nullable().optional(),
  transfer_concurrency: z.number().int().min(1).max(16).optional(),
  multipart_parallelism: z.number().int().min(1).max(16).optional(),
  multipart_threshold_bytes: z.number().int().min(5 * 1048576).optional(),
  part_size_bytes: z.number().int().min(5 * 1048576).optional(),
  presign_default_expires_secs: z.number().int().min(60).max(604800).optional(),
  http_proxy: z
    .string()
    .trim()
    .nullable()
    .optional()
    .refine((value) => !value || /^https?:\/\/.+/.test(value), {
      message: "Proxy must be an http:// or https:// URL",
    }),
  custom_ca_path: z.string().nullable().optional(),
  request_log_ttl_days: z.number().int().min(1).max(365).optional(),
  theme: z.enum(["light", "dark", "system"]).optional(),
  show_hidden: z.boolean().optional(),
  confirm_destructive: z.boolean().optional(),
}).partial();

export const nightWatchAddSchema = z.object({
  account_id: z.string().min(1, "Select an account"),
  bucket: z.string().min(1, "Select a bucket"),
  local_dir: z.string().trim().min(1, "Local directory is required"),
  tree_uri: z.string().optional(),
  key_prefix: uploadKeyPrefixSchema.optional(),
  ignore_file: z.string().max(1024, "Ignore file path is too long").optional(),
  full_scan_secs: z.number().int().min(30, "Scan interval must be at least 30 seconds"),
});

export const mcpPortSchema = z
  .number()
  .int("Port must be a whole number")
  .min(1024, "Port must be between 1024 and 65535")
  .max(65535, "Port must be between 1024 and 65535");

export const mcpFsRootSchema = z.string().max(4096, "Path is too long");

const corsDraftRuleSchema = z.object({
  origins: z.string(),
  methods: z.array(z.string()),
  maxAge: z.string(),
}).superRefine((rule, ctx) => {
  const origins = rule.origins
    .split(/[\n,]/)
    .map((part) => part.trim())
    .filter(Boolean);
  if (origins.length === 0) {
    ctx.addIssue({ code: z.ZodIssueCode.custom, message: "At least one allowed origin is required" });
    return;
  }
  const bad = origins.find((origin) => !CORS_ORIGIN_RE.test(origin));
  if (bad) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: `"${bad}" is not a valid origin. Use "*", "https://example.com", or "https://*.example.com"`,
    });
  }
  if (rule.methods.length === 0) {
    ctx.addIssue({ code: z.ZodIssueCode.custom, message: "At least one allowed method is required" });
  }
  const maxAge = rule.maxAge.trim();
  if (maxAge !== "") {
    if (!/^\d+$/.test(maxAge) || Number.isNaN(Number(maxAge))) {
      ctx.addIssue({ code: z.ZodIssueCode.custom, message: "Max age must be a whole number of seconds" });
    } else if (Number(maxAge) > MAX_AGE_LIMIT) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: `Max age must be ${MAX_AGE_LIMIT} seconds or less`,
      });
    }
  }
});

export const corsRulesSchema = z.array(corsDraftRuleSchema);

export function validateCorsRules(
  rules: Array<{ origins: string; methods: string[]; maxAge: string }>,
): string | null {
  for (let i = 0; i < rules.length; i++) {
    const result = corsDraftRuleSchema.safeParse(rules[i]);
    if (!result.success) return `Rule ${i + 1}: ${result.error.issues[0]?.message ?? "Invalid rule"}`;
  }
  return null;
}

export function createAccountFormSchema(opts: {
  providerId: string;
  isEdit: boolean;
  existingName?: string;
}) {
  const endpoint = opts.providerId === "aws"
    ? z.string().trim().optional()
    : endpointUrlSchema;

  const secret = opts.isEdit
    ? z.string().trim().max(256, "Secret Access Key is too long").optional()
    : secretAccessKeySchema;

  return z.object({
    name: accountNameSchemaForForm({ isEdit: opts.isEdit, existingName: opts.existingName }),
    protocol: z.string(),
    region: z.string().optional(),
    access_key_id: accessKeyIdSchema,
    secret_access_key: secret,
    endpoint,
    addressing_style: z.string().optional(),
  });
}

export function createOnboardingAccountSchema(provider: { custom_endpoint?: boolean }) {
  return z.object({
    name: accountNameSchema,
    accessKey: accessKeyIdSchema,
    secretKey: secretAccessKeySchema,
    endpoint: provider.custom_endpoint ? endpointUrlSchema : z.string().optional(),
  });
}

export function clampAccountName(name: string, maxLen = ACCOUNT_NAME_MAX_LENGTH): string {
  return name.slice(0, maxLen);
}

export function accountNameMaxLength(opts: { isEdit: boolean; existingName?: string }): number {
  const existingLen = opts.existingName?.trim().length ?? 0;
  return opts.isEdit ? Math.max(ACCOUNT_NAME_MAX_LENGTH, existingLen) : ACCOUNT_NAME_MAX_LENGTH;
}
