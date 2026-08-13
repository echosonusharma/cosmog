/** Parsed AWS credentials INI (`~/.aws/credentials`). */

export interface ParsedAwsProfile {
  name: string;
  access_key_id: string;
  secret_access_key: string;
  region: string;
}

export type AwsProfileSkipReason = "no_keys" | "session_token" | "sso_only";

export interface SkippedAwsProfile {
  name: string;
  reason: AwsProfileSkipReason;
  message: string;
}

export interface IniSyntaxError {
  line: number;
  message: string;
}

export interface ParseAwsCredentialsResult {
  profiles: ParsedAwsProfile[];
  skipped: SkippedAwsProfile[];
  syntaxErrors: IniSyntaxError[];
}

interface SectionDraft {
  name: string;
  access_key_id?: string;
  secret_access_key?: string;
  session_token?: string;
  region?: string;
  has_sso: boolean;
}

const SECTION_RE = /^\s*\[([^\]]+)\]\s*$/;
const KV_RE = /^\s*([A-Za-z0-9_.-]+)\s*=\s*(.*)$/;

function normalizeSectionName(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.toLowerCase() === "default") return "default";
  const lower = trimmed.toLowerCase();
  if (lower.startsWith("profile ")) return trimmed.slice(8).trim();
  return trimmed;
}

function finalizeSection(draft: SectionDraft): { profile?: ParsedAwsProfile; skipped?: SkippedAwsProfile } {
  const { name, access_key_id, secret_access_key, session_token, region, has_sso } = draft;
  const hasKeys = !!(access_key_id?.trim() && secret_access_key?.trim());

  if (hasKeys && session_token?.trim()) {
    return {
      skipped: {
        name,
        reason: "session_token",
        message: "temporary credentials (session token) are not supported",
      },
    };
  }

  if (hasKeys) {
    return {
      profile: {
        name,
        access_key_id: access_key_id!.trim(),
        secret_access_key: secret_access_key!.trim(),
        region: region?.trim() || "us-east-1",
      },
    };
  }

  if (has_sso) {
    return {
      skipped: {
        name,
        reason: "sso_only",
        message: "SSO profiles are not supported. Use static keys or manual entry.",
      },
    };
  }

  return {
    skipped: {
      name,
      reason: "no_keys",
      message: "missing aws_access_key_id and aws_secret_access_key",
    },
  };
}

/** Validate INI line shape for AWS credentials files. */
export function validateAwsCredentialsIni(text: string): IniSyntaxError[] {
  const errors: IniSyntaxError[] = [];
  const lines = text.split(/\r?\n/);

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#") || trimmed.startsWith(";")) continue;

    if (SECTION_RE.test(line)) continue;
    if (KV_RE.test(line)) continue;

    errors.push({
      line: i + 1,
      message: "expected a [section] header or key = value line",
    });
  }

  return errors;
}

/** Parse `~/.aws/credentials` content into importable profiles. */
export function parseAwsCredentialsIni(text: string): ParseAwsCredentialsResult {
  const syntaxErrors = validateAwsCredentialsIni(text);
  if (syntaxErrors.length > 0) {
    return { profiles: [], skipped: [], syntaxErrors };
  }

  const trimmed = text.trim();
  if (!trimmed) {
    return { profiles: [], skipped: [], syntaxErrors: [] };
  }

  const profiles: ParsedAwsProfile[] = [];
  const skipped: SkippedAwsProfile[] = [];
  let current: SectionDraft | null = null;

  const flush = () => {
    if (!current) return;
    const result = finalizeSection(current);
    if (result.profile) profiles.push(result.profile);
    if (result.skipped) skipped.push(result.skipped);
    current = null;
  };

  for (const line of text.split(/\r?\n/)) {
    const bare = line.trim();
    if (!bare || bare.startsWith("#") || bare.startsWith(";")) continue;

    const sectionMatch = SECTION_RE.exec(line);
    if (sectionMatch) {
      flush();
      current = { name: normalizeSectionName(sectionMatch[1]), has_sso: false };
      continue;
    }

    const kvMatch = KV_RE.exec(line);
    if (!kvMatch) continue;

    if (!current) {
      return {
        profiles: [],
        skipped: [],
        syntaxErrors: [{ line: 1, message: "key = value must appear under a [section] header" }],
      };
    }

    const key = kvMatch[1].trim().toLowerCase();
    const value = kvMatch[2].trim();

    switch (key) {
      case "aws_access_key_id":
        current.access_key_id = value;
        break;
      case "aws_secret_access_key":
        current.secret_access_key = value;
        break;
      case "aws_session_token":
        current.session_token = value;
        break;
      case "region":
        current.region = value;
        break;
      default:
        if (key.startsWith("sso_")) current.has_sso = true;
        break;
    }
  }

  flush();
  return { profiles, skipped, syntaxErrors: [] };
}
