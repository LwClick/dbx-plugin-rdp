import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const result = spawnSync("dbx-plugin", ["package", "."], {
  cwd: root,
  stdio: "inherit",
  env: { ...process.env, DBX_PLUGIN_SDK_ROOT: resolve(root, "vendor/sdk-root") },
  shell: process.platform === "win32",
});
process.exit(result.status ?? 1);
