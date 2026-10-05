import common from "./common.ts";
import errors from "./errors.ts";
import shell from "./shell.ts";
import onboarding from "./onboarding.ts";
import settings from "./settings.ts";
import browse from "./browse.ts";
import preview from "./preview.ts";
import bucketconfig from "./bucketconfig.ts";
import transfers from "./transfers.ts";
import logs from "./logs.ts";
import nightwatcher from "./nightwatcher.ts";
import mcp from "./mcp.ts";

export default {
  ...common,
  ...errors,
  ...shell,
  ...onboarding,
  ...settings,
  ...browse,
  ...preview,
  ...bucketconfig,
  ...transfers,
  ...logs,
  ...nightwatcher,
  ...mcp,
};
