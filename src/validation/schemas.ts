import { z } from "zod";
import { t, type TKey } from "../i18n";

export const ACCOUNT_NAME_MAX_LENGTH = 64;

// Lazy message: resolved when validation runs, so it follows the current language.
const v = (key: TKey, params?: Record<string, string | number>) => ({ error: () => t(key, params) });

const CORS_ORIGIN_RE = /^(\*|https?:\/\/[A-Za-z0-9.\-*]+(:\d+)?)$/;
const MAX_AGE_LIMIT = 2_147_483_647;

export const accountNameSchema = z
  .string()
  .trim()
  .min(1, v("settings.validation.nameRequired"))
  .max(ACCOUNT_NAME_MAX_LENGTH, v("settings.validation.nameMax", { max: ACCOUNT_NAME_MAX_LENGTH }));

/** Edit forms allow an existing long name but cap new input at the same length. */
export function accountNameSchemaForForm(opts: { isEdit: boolean; existingName?: string }) {
  const existingLen = opts.existingName?.trim().length ?? 0;
  const maxLen = opts.isEdit ? Math.max(ACCOUNT_NAME_MAX_LENGTH, existingLen) : ACCOUNT_NAME_MAX_LENGTH;
  return z
    .string()
    .trim()
    .min(1, v("settings.validation.nameRequired"))
    .max(maxLen, v("settings.validation.nameMax", { max: maxLen }));
}

export const accessKeyIdSchema = z
  .string()
  .trim()
  .min(1, v("settings.validation.accessKeyRequired"))
  .max(256, v("settings.validation.accessKeyTooLong"));

export const secretAccessKeySchema = z
  .string()
  .trim()
  .min(1, v("settings.validation.secretRequired"))
  .max(256, v("settings.validation.secretTooLong"));

export const endpointUrlSchema = z
  .string()
  .trim()
  .min(1, v("settings.validation.endpointRequired"))
  .url(v("settings.validation.endpointInvalid"))
  .refine((url) => /^https?:\/\//i.test(url), v("settings.validation.endpointScheme"));

export const bucketNameSchema = z
  .string()
  .trim()
  .min(1, v("settings.validation.bucketRequired"))
  .max(63, v("settings.validation.bucketMax"))
  .refine((name) => !/[/\\]/.test(name), v("settings.validation.bucketSlash"))
  .refine((name) => !name.includes(".."), v("settings.validation.bucketDots"));

export const folderPathSchema = z
  .string()
  .trim()
  .transform((path) => path.replace(/\/+/g, "/").replace(/^\//, "").replace(/\/$/, ""))
  .pipe(z.string().min(1, v("settings.validation.folderRequired")).max(1024, v("settings.validation.folderTooLong")));

export const objectKeySchema = z
  .string()
  .trim()
  .transform((key) => key.replace(/^\/+/, ""))
  .pipe(z.string().min(1, v("settings.validation.pathRequired")).max(1024, v("settings.validation.pathTooLong")));

export const mimeTypeSchema = z
  .string()
  .trim()
  .toLowerCase()
  .regex(
    /^[a-z0-9][a-z0-9!#$&^_.+-]*\/[a-z0-9][a-z0-9!#$&^_.+-]*(\s*;\s*[a-z0-9-]+=("[^"]*"|[^\s;"]+))*$/,
    v("settings.validation.mime"),
  );

export const downloadPathSchema = z
  .string()
  .trim()
  .min(1, v("settings.validation.saveLocationRequired"));

export const uploadKeyPrefixSchema = z.string().max(1024, v("settings.validation.prefixTooLong"));

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
      ...v("settings.validation.proxy"),
    }),
  custom_ca_path: z.string().nullable().optional(),
  request_log_ttl_days: z.number().int().min(1).max(365).optional(),
  theme: z.enum(["light", "dark", "system"]).optional(),
  show_hidden: z.boolean().optional(),
  confirm_destructive: z.boolean().optional(),
  auto_preview: z.boolean().optional(),
}).partial();

export const nightWatchAddSchema = z.object({
  account_id: z.string().min(1, v("settings.validation.selectAccount")),
  bucket: z.string().min(1, v("settings.validation.selectBucket")),
  local_dir: z.string().trim().min(1, v("settings.validation.localDirRequired")),
  tree_uri: z.string().optional(),
  key_prefix: uploadKeyPrefixSchema.optional(),
  ignore_file: z.string().max(1024, v("settings.validation.ignoreFileTooLong")).optional(),
  full_scan_secs: z.number().int().min(30, v("settings.validation.scanInterval")),
});

export const mcpPortSchema = z
  .number()
  .int(v("settings.validation.portInt"))
  .min(1024, v("settings.validation.portRange"))
  .max(65535, v("settings.validation.portRange"));

export const mcpFsRootSchema = z.string().max(4096, v("settings.validation.pathTooLong"));

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
    ctx.addIssue({ code: z.ZodIssueCode.custom, message: t("settings.validation.corsOriginRequired") });
    return;
  }
  const bad = origins.find((origin) => !CORS_ORIGIN_RE.test(origin));
  if (bad) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: t("settings.validation.corsOriginInvalid", { origin: bad }),
    });
  }
  if (rule.methods.length === 0) {
    ctx.addIssue({ code: z.ZodIssueCode.custom, message: t("settings.validation.corsMethodRequired") });
  }
  const maxAge = rule.maxAge.trim();
  if (maxAge !== "") {
    if (!/^\d+$/.test(maxAge) || Number.isNaN(Number(maxAge))) {
      ctx.addIssue({ code: z.ZodIssueCode.custom, message: t("settings.validation.corsMaxAgeInt") });
    } else if (Number(maxAge) > MAX_AGE_LIMIT) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: t("settings.validation.corsMaxAgeMax", { max: MAX_AGE_LIMIT }),
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
    if (!result.success) return t("settings.validation.corsRule", {
      n: i + 1,
      message: result.error.issues[0]?.message ?? t("settings.validation.invalidRule"),
    });
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
    ? z.string().trim().max(256, v("settings.validation.secretTooLong")).optional()
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
