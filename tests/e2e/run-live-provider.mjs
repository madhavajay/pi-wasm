import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";

const env = { ...process.env };
const reportPath = "live-results/live-provider-latest.json";
const evidencePath = "live-results/live-provider-evidence.json";
const liveProvider = env.PI_WASM_LIVE_PROVIDER || "openai";
let tokenSource = "missing";
let tokenLoadError = null;

mkdirSync("live-results", { recursive: true });
if (existsSync(evidencePath)) {
  unlinkSync(evidencePath);
}

if (env.PI_WASM_LIVE_TOKEN) {
  tokenSource = "PI_WASM_LIVE_TOKEN";
} else if (env.PI_WASM_LIVE_TOKEN_FILE) {
  tokenSource = "PI_WASM_LIVE_TOKEN_FILE";
  try {
    env.PI_WASM_LIVE_TOKEN = readFileSync(env.PI_WASM_LIVE_TOKEN_FILE, "utf8").trim();
  } catch (error) {
    tokenLoadError = error instanceof Error ? error.message : String(error);
  }
} else if (liveProvider === "openai" && env.OPENAI_API_KEY) {
  tokenSource = "OPENAI_API_KEY";
  env.PI_WASM_LIVE_TOKEN = env.OPENAI_API_KEY;
}

function writeReport(status, exitCode, extra = {}) {
  mkdirSync("live-results", { recursive: true });
  let browserEvidence = null;
  if (existsSync(evidencePath)) {
    try {
      browserEvidence = JSON.parse(readFileSync(evidencePath, "utf8"));
    } catch (error) {
      browserEvidence = {
        parseError: error instanceof Error ? error.message : String(error),
        tokenValueRecorded: false,
      };
    }
  }
  writeFileSync(
    reportPath,
    `${JSON.stringify(
      {
        ranAt: new Date().toISOString(),
        status,
        exitCode,
        provider: liveProvider,
        endpoint:
          env.PI_WASM_LIVE_ENDPOINT ||
          (liveProvider === "openai-codex"
            ? "/proxy/codex/responses"
            : "/proxy/openai/responses"),
        model:
          env.PI_WASM_LIVE_MODEL ||
          (liveProvider === "openai-codex" ? "gpt-5.5" : "gpt-4.1-mini"),
        tokenSource,
        tokenPresent: Boolean(env.PI_WASM_LIVE_TOKEN),
        tokenValueRecorded: false,
        browserEvidence,
        ...extra,
      },
      null,
      2
    )}\n`
  );
}

if (tokenLoadError) {
  writeReport("failed_to_read_token_file", 1, { tokenLoadError });
  console.error(`Could not read PI_WASM_LIVE_TOKEN_FILE: ${tokenLoadError}`);
  console.error(`Wrote sanitized live provider report to ${reportPath}`);
  process.exit(1);
}

if (!env.PI_WASM_LIVE_TOKEN) {
  writeReport("skipped", 0);
  console.log(
    "Skipping live provider probe: set PI_WASM_LIVE_TOKEN, PI_WASM_LIVE_TOKEN_FILE, or OPENAI_API_KEY for the openai provider."
  );
  console.log(`Wrote sanitized live provider report to ${reportPath}`);
  process.exit(0);
}

const result = spawnSync(
  process.platform === "win32" ? "npx.cmd" : "npx",
  ["playwright", "test", "tests/e2e/live-provider.spec.js"],
  {
    stdio: "inherit",
    env,
  }
);

writeReport(result.status === 0 ? "passed" : "failed", result.status ?? 1);
console.log(`Wrote sanitized live provider report to ${reportPath}`);
process.exit(result.status ?? 1);
