import { expect, test } from "@playwright/test";

test("loads a Codex-first chat UI", async ({ page }) => {
  await page.goto("./");

  await expect(page.getByRole("heading", { name: "Pi WASM" })).toBeVisible();
  await expect(page.locator("#terminal")).toContainText("WASM loaded. Real provider mode is ready.");
  await expect(page.getByText("no auth")).toBeVisible();
  await expect(page.getByLabel("Provider")).toHaveValue("openai-codex");
  await expect(page.getByLabel("Endpoint")).toHaveValue("/proxy/codex/responses");
  await expect(page.getByLabel("Model")).toHaveValue("gpt-5.5");
  await expect(page.getByRole("button", { name: "Send" })).toBeDisabled();
  await expect(page.getByText("Paste a token to enable chat.")).toBeVisible();
  await expect(page.locator("#debug-view")).toHaveClass(/hidden/);
});

test("auth controls and actions fit narrow browser widths", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 780 });
  await page.goto("./");

  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Cancel" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Debug" })).toBeVisible();

  const overflow = await page.evaluate(() => {
    const doc = document.documentElement;
    return doc.scrollWidth - doc.clientWidth;
  });
  expect(overflow).toBeLessThanOrEqual(1);
});

test("debug log scrolls internally instead of growing the page", async ({ page }) => {
  await page.goto("./");
  await page.getByRole("button", { name: "Debug" }).click();

  const metrics = await page.locator("#terminal").evaluate((terminal) => {
    const row = terminal.querySelector(".line");
    if (!row) throw new Error("expected initial debug row");
    for (let i = 0; i < 80; i += 1) {
      terminal.append(row.cloneNode(true));
    }
    terminal.scrollTop = terminal.scrollHeight;
    const style = window.getComputedStyle(terminal);
    return {
      clientHeight: terminal.clientHeight,
      scrollHeight: terminal.scrollHeight,
      scrollTop: terminal.scrollTop,
      overflowY: style.overflowY,
    };
  });

  expect(metrics.overflowY).toBe("auto");
  expect(metrics.scrollHeight).toBeGreaterThan(metrics.clientHeight);
  expect(metrics.scrollTop).toBeGreaterThan(0);
});

test("stores pasted credentials without rendering token contents", async ({ page }) => {
  await page.goto("./");

  const fakeCodexToken = [
    "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0",
    "eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiZWUyZS1hY2NvdW50In19",
    "sig",
  ].join(".");

  await page.getByLabel("Storage").selectOption("memory");
  await page.getByRole("textbox", { name: "Token" }).fill(fakeCodexToken);
  await expect(page.getByRole("button", { name: "Send" })).toBeDisabled();
  await expect(page.locator("#direct-credential-label")).toHaveClass(/needs-attention/);
  await page.getByLabel("Send token from this browser").check();
  await expect(page.getByRole("button", { name: "Send" })).toBeEnabled();
  await page.getByRole("button", { name: "Save" }).click();

  await page.getByRole("button", { name: "Debug" }).click();
  await page.getByRole("button", { name: "Draft request" }).click();

  await expect(page.getByText("auth set (memory)")).toBeVisible();
  await expect(page.getByRole("button", { name: "Send" })).toBeEnabled();
  await expect(page.getByText("Bearer <redacted>")).toBeVisible();
  await expect(page.getByText("Browser auth store status")).toBeVisible();
  await expect(page.getByText(fakeCodexToken)).toHaveCount(0);
});

test("chat composer appends the user message before a real request attempt", async ({ page }) => {
  await page.goto("./");

  const fakeCodexToken = [
    "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0",
    "eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiZWUyZS1hY2NvdW50In19",
    "sig",
  ].join(".");

  await page.getByLabel("Storage").selectOption("memory");
  await page.getByRole("textbox", { name: "Token" }).fill(fakeCodexToken);
  await page.getByLabel("Send token from this browser").check();
  await page.getByPlaceholder("Message gpt-5.5 via Pi Codex").fill("hello real codex");
  await page.getByRole("button", { name: "Send" }).click();

  await expect(page.locator("#chat-log")).toContainText("hello real codex");
  await expect(page.locator("#chat-log")).toContainText(
    /Request failed|Browser fetch failed|No assistant text|Done\.|Provider error/,
    {
      timeout: 15_000,
    }
  );
});
