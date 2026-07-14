import { expect, test } from "@playwright/test";
import { mkdirSync, writeFileSync } from "node:fs";

const liveToken = process.env.PI_WASM_LIVE_TOKEN || "";
const liveProvider = process.env.PI_WASM_LIVE_PROVIDER || "openai";
const liveEndpoint =
  process.env.PI_WASM_LIVE_ENDPOINT ||
  (liveProvider === "openai-codex" ? "/proxy/codex/responses" : "/proxy/openai/responses");
const liveModel =
  process.env.PI_WASM_LIVE_MODEL || (liveProvider === "openai-codex" ? "gpt-5.5" : "gpt-4.1-mini");
const evidencePath = "live-results/live-provider-evidence.json";

test.skip(!liveToken, "Set PI_WASM_LIVE_TOKEN to run the live browser provider probe.");
test.setTimeout(60_000);

function sanitizeText(text) {
  let sanitized = text.replaceAll(liveToken, "<redacted-token>");
  sanitized = sanitized.replace(/[A-Za-z0-9_.-]{64,}/g, "<redacted-long-token>");
  return sanitized;
}

async function collectEvidence(page) {
  const terminalText = await page.locator("#terminal").innerText().catch(() => "");
  const sanitizedTerminal = sanitizeText(terminalText);
  const lines = sanitizedTerminal
    .split(/\n+/)
    .map((line) => line.trim())
    .filter(Boolean);

  return {
    provider: liveProvider,
    endpoint: liveEndpoint,
    model: liveModel,
    reachedHttp: sanitizedTerminal.includes("Received browser HTTP response."),
    emittedTextDelta: sanitizedTerminal.includes("textDelta"),
    completed: sanitizedTerminal.includes("Real request attempt complete."),
    failedBeforeHttp: sanitizedTerminal.includes("Browser fetch failed before an HTTP response."),
    sawProviderError: sanitizedTerminal.includes("providerError"),
    sawTokenInTerminal: liveToken ? terminalText.includes(liveToken) : false,
    terminalTail: lines.slice(-40),
    tokenValueRecorded: false,
  };
}

async function writeEvidence(page) {
  mkdirSync("live-results", { recursive: true });
  writeFileSync(evidencePath, `${JSON.stringify(await collectEvidence(page), null, 2)}\n`);
}

test("live browser provider accepts CORS and streams useful text deltas", async ({ page }) => {
  await page.goto("./");

  await page.getByLabel("Provider").selectOption(liveProvider);
  await page.getByLabel("Storage").selectOption("memory");
  await page.getByRole("textbox", { name: "Token" }).fill(liveToken);
  await page.getByLabel("Endpoint").fill(liveEndpoint);
  await page.getByLabel("Model").fill(liveModel);
  await page.getByLabel("Send token from this browser").check();
  await page.getByRole("button", { name: "Save" }).click();
  await page
    .getByPlaceholder("Message gpt-5.5 via Pi Codex")
    .fill("Reply with exactly: pi wasm live ok");

  await page.getByRole("button", { name: "Send" }).click();
  await page.locator("#token").evaluate((input) => {
    input.value = "<redacted-live-token>";
  });

  await expect
    .poll(
      async () => {
        const text = await page.locator("#terminal").innerText().catch(() => "");
        if (text.includes("Real request attempt complete.")) return "completed";
        if (text.includes("Browser fetch failed before an HTTP response.")) return "failedBeforeHttp";
        if (text.includes("Provider returned a non-success HTTP response")) return "providerError";
        if (text.includes("Received browser HTTP response.")) return "reachedHttp";
        if (text.includes("Sending browser provider request.")) return "sent";
        return "waiting";
      },
      { timeout: 45_000 }
    )
    .not.toBe("waiting");
  await page.getByRole("button", { name: "Debug" }).click();
  await writeEvidence(page);

  try {
    await expect(page.locator("#terminal")).toContainText("Received browser HTTP response.", {
      timeout: 30_000,
    });
    await expect(page.locator("#terminal")).toContainText("textDelta", {
      timeout: 30_000,
    });
    await expect(page.locator("#terminal")).toContainText("Real request attempt complete.", {
      timeout: 30_000,
    });
    await expect(page.locator("#terminal")).not.toContainText(liveToken);
  } finally {
    await writeEvidence(page);
  }
});
