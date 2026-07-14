import init, { PiClient } from "../pkg/pi_browser/pi_browser.js";

const els = {
  status: document.querySelector("#status"),
  provider: document.querySelector("#provider"),
  storageMode: document.querySelector("#storage-mode"),
  token: document.querySelector("#token"),
  endpoint: document.querySelector("#endpoint"),
  model: document.querySelector("#model"),
  allowDirectCredentials: document.querySelector("#allow-direct-credentials"),
  directCredentialLabel: document.querySelector("#direct-credential-label"),
  saveAuth: document.querySelector("#save-auth"),
  clearAuth: document.querySelector("#clear-auth"),
  listModels: document.querySelector("#list-models"),
  generatePkce: document.querySelector("#generate-pkce"),
  tokenHelp: document.querySelector("#token-help"),
  draftRequest: document.querySelector("#draft-request"),
  sendReal: document.querySelector("#send-real"),
  runState: document.querySelector("#run-state"),
  chatTab: document.querySelector("#chat-tab"),
  debugTab: document.querySelector("#debug-tab"),
  chatView: document.querySelector("#chat-view"),
  debugView: document.querySelector("#debug-view"),
  chatLog: document.querySelector("#chat-log"),
  terminal: document.querySelector("#terminal"),
  prompt: document.querySelector("#prompt"),
  cancel: document.querySelector("#cancel"),
};

let client;
let memoryToken = "";
let currentAbortController = null;
let activeAssistantBubble = null;
let activeToolBubbles = new Map();

const TOKEN_KEY = "piWasm.token";
const PROVIDER_KEY = "piWasm.provider";
const ENDPOINT_KEY = "piWasm.endpoint";
const MODEL_KEY = "piWasm.model";
const STORAGE_MODE_KEY = "piWasm.storageMode";
const DIRECT_CREDENTIALS_KEY = "piWasm.directBrowserCredentialsAllowed";
const IDB_NAME = "piWasmAuth";
const IDB_STORE = "credentials";
const PKCE_VERIFIER_KEY = "piWasm.pkceVerifier";
const PKCE_STATE_KEY = "piWasm.pkceState";
const PKCE_CALLBACK_KEY = "piWasm.pkceCallback";
const PKCE_CODE_KEY = "piWasm.pkceCode";
const OPENAI_RESPONSES_ENDPOINT = "https://api.openai.com/v1/responses";
const OPENAI_CODEX_ENDPOINT = "/proxy/codex/responses";
const OPENAI_CODEX_DIRECT_ENDPOINT = "https://chatgpt.com/backend-api/codex/responses";

function base64Url(bytes) {
  const binary = String.fromCharCode(...bytes);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function randomBase64Url(byteLength) {
  const bytes = new Uint8Array(byteLength);
  crypto.getRandomValues(bytes);
  return base64Url(bytes);
}

async function sha256Base64Url(text) {
  const bytes = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return base64Url(new Uint8Array(digest));
}

function normalizeProvider(provider) {
  const normalized = String(provider || "").trim().toLowerCase();
  return normalized || "openai-codex";
}

function tokenKind(token) {
  const trimmed = String(token || "").trim();
  if (!trimmed) return "missing";
  if (trimmed.split(".").length === 3) return "codex-access-token";
  if (trimmed.startsWith("sk-")) return "openai-platform-key";
  return "bearer-token";
}

function tokenProviderHint(provider, token) {
  const kind = tokenKind(token);
  if (kind === "missing") return "Paste a token first.";
  if (provider === "openai-codex" && kind !== "codex-access-token") {
    return "OpenAI Codex expects tokens.access_token from Codex auth.json.";
  }
  if (provider === "openai" && kind === "codex-access-token") {
    return "This looks like a Codex access token; select OpenAI Codex or use a Platform API key for OpenAI Platform.";
  }
  if (provider === "openai" && kind !== "openai-platform-key") {
    return "OpenAI Platform works best with a write-scoped Platform API key.";
  }
  return "";
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function idbRequest(request) {
  return new Promise((resolve, reject) => {
    request.addEventListener("success", () => resolve(request.result));
    request.addEventListener("error", () => reject(request.error));
  });
}

async function openCredentialDb() {
  if (!("indexedDB" in window)) {
    throw new Error("IndexedDB is unavailable in this browser context.");
  }

  const request = indexedDB.open(IDB_NAME, 1);
  request.addEventListener("upgradeneeded", () => {
    const db = request.result;
    if (!db.objectStoreNames.contains(IDB_STORE)) {
      db.createObjectStore(IDB_STORE, { keyPath: "id" });
    }
  });
  return idbRequest(request);
}

async function idbGetCredential(provider) {
  const db = await openCredentialDb();
  try {
    const tx = db.transaction(IDB_STORE, "readonly");
    return (await idbRequest(tx.objectStore(IDB_STORE).get(normalizeProvider(provider)))) || null;
  } finally {
    db.close();
  }
}

async function idbSetCredential(provider, credential) {
  const db = await openCredentialDb();
  try {
    const tx = db.transaction(IDB_STORE, "readwrite");
    await idbRequest(
      tx.objectStore(IDB_STORE).put({
        id: normalizeProvider(provider),
        credential,
        updatedAt: new Date().toISOString(),
      })
    );
  } finally {
    db.close();
  }
}

async function idbRemoveCredential(provider) {
  const db = await openCredentialDb();
  try {
    const tx = db.transaction(IDB_STORE, "readwrite");
    await idbRequest(tx.objectStore(IDB_STORE).delete(normalizeProvider(provider)));
  } finally {
    db.close();
  }
}

async function removeEveryStoredToken(provider) {
  memoryToken = "";
  sessionStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(TOKEN_KEY);
  try {
    await idbRemoveCredential(provider);
  } catch (_error) {
    // IndexedDB may be unavailable in some browser contexts; clearing the
    // selected mode will report the actual error if the user chooses it.
  }
}

function append(kind, text, data) {
  const row = document.createElement("div");
  row.className = `line line-${kind}`;

  const tag = document.createElement("span");
  tag.className = "tag";
  tag.textContent = kind;

  const body = document.createElement("pre");
  body.textContent = data ? `${text}\n${JSON.stringify(data, null, 2)}` : text;

  row.append(tag, body);
  els.terminal.append(row);
  els.terminal.scrollTop = els.terminal.scrollHeight;
}

function addChatMessage(role, text = "") {
  const row = document.createElement("article");
  row.className = `message message-${role}`;

  const label = document.createElement("div");
  label.className = "message-role";
  label.textContent = role === "assistant" ? "Pi" : role === "user" ? "You" : "System";

  const body = document.createElement("div");
  body.className = "message-body";
  body.textContent = text;

  row.append(label, body);
  els.chatLog.append(row);
  els.chatLog.scrollTop = els.chatLog.scrollHeight;
  return body;
}

function updateToolStatus(name, state) {
  const key = String(name || "tool");
  let body = activeToolBubbles.get(key);
  if (!body) {
    body = addChatMessage("system", "");
    body.classList.add("message-tool-status");
    activeToolBubbles.set(key, body);
  }
  body.textContent = state === "done" ? `Used ${key}` : `Running ${key}...`;
  els.chatLog.scrollTop = els.chatLog.scrollHeight;
}

function appendAssistantText(text) {
  if (!activeAssistantBubble) {
    activeAssistantBubble = addChatMessage("assistant");
  }
  activeAssistantBubble.textContent += text;
  els.chatLog.scrollTop = els.chatLog.scrollHeight;
}

function removeEmptyAssistantBubble() {
  if (activeAssistantBubble && !activeAssistantBubble.textContent.trim()) {
    activeAssistantBubble.closest(".message")?.remove();
  }
  activeAssistantBubble = null;
}

function setActiveTab(tab) {
  const debug = tab === "debug";
  els.chatView.classList.toggle("hidden", debug);
  els.debugView.classList.toggle("hidden", !debug);
  els.chatTab.classList.toggle("active", !debug);
  els.debugTab.classList.toggle("active", debug);
  els.chatTab.setAttribute("aria-selected", String(!debug));
  els.debugTab.setAttribute("aria-selected", String(debug));
}

function refreshStatus() {
  const cfg = JSON.parse(client.configJson());
  const hasToken = Boolean(els.token.value.trim());
  const directAllowed = els.allowDirectCredentials.checked || cfg.directBrowserCredentialsAllowed;
  const canRunReal = directAllowed && hasToken;
  const providerHint = tokenProviderHint(els.provider.value, els.token.value);
  els.status.textContent = cfg.hasAuth ? `auth set (${els.storageMode.value})` : "no auth";
  els.status.className = `status ${cfg.hasAuth ? "ok" : "warn"}`;
  els.sendReal.disabled = !canRunReal;
  const runReason = !hasToken
    ? "Paste a token to enable chat."
    : !directAllowed
      ? "Enable browser sending to chat."
      : providerHint || "Ready to send a chat request with the selected provider/model.";
  els.runState.textContent = runReason;
  els.sendReal.title = runReason;
  els.directCredentialLabel.classList.toggle("needs-attention", hasToken && !directAllowed);
}

function syncClientConfig() {
  client.setEndpoint(els.endpoint.value);
  client.setModel(els.model.value);
  client.setCredential(els.provider.value, els.token.value);
  client.setDirectBrowserCredentialsAllowed(els.allowDirectCredentials.checked);
}

function buildAgentRequest() {
  return buildAgentRequestWithPrompt(els.prompt.value || "Hello from Pi WASM");
}

function buildAgentRequestWithPrompt(prompt) {
  return {
    provider: els.provider.value,
    model: els.model.value,
    systemPrompt: "You are running inside the Pi WASM browser prototype. Use tools only when they help.",
    messages: [
      {
        role: "user",
        content: prompt,
      },
    ],
    tools: [
      {
        type: "function",
        name: "get_time",
        description: "Return the browser-local current time.",
        parameters: {
          type: "object",
          properties: {},
          additionalProperties: false,
        },
      },
      {
        type: "function",
        name: "echo",
        description: "Echo a short string back to the user.",
        parameters: {
          type: "object",
          properties: {
            text: {
              type: "string",
            },
          },
          required: ["text"],
          additionalProperties: false,
        },
      },
      {
        type: "function",
        name: "browser_note",
        description: "Create a local note in the browser prototype.",
        parameters: {
          type: "object",
          properties: {
            note: {
              type: "string",
            },
          },
          required: ["note"],
          additionalProperties: false,
        },
      },
    ],
  };
}

const browserAuthStore = {
  async get(provider, mode) {
    if (mode === "memory") return memoryToken;
    if (mode === "session") return sessionStorage.getItem(TOKEN_KEY) || "";
    if (mode === "local") return localStorage.getItem(TOKEN_KEY) || "";
    if (mode === "indexeddb") {
      const record = await idbGetCredential(provider);
      return record?.credential || "";
    }
    return "";
  },

  async set(provider, credential, mode) {
    await removeEveryStoredToken(provider);
    if (!credential) return;

    if (mode === "memory") {
      memoryToken = credential;
      return;
    }
    if (mode === "session") {
      sessionStorage.setItem(TOKEN_KEY, credential);
      return;
    }
    if (mode === "local") {
      localStorage.setItem(TOKEN_KEY, credential);
      return;
    }
    if (mode === "indexeddb") {
      await idbSetCredential(provider, credential);
    }
  },

  async remove(provider) {
    await removeEveryStoredToken(provider);
  },

  async status(provider, mode) {
    const credential = await this.get(provider, mode);
    return {
      provider: normalizeProvider(provider),
      mode,
      hasCredential: Boolean(credential),
      persistent: mode === "local" || mode === "indexeddb",
    };
  },
};

async function readStoredToken() {
  return browserAuthStore.get(els.provider.value, els.storageMode.value);
}

async function writeStoredToken(token) {
  await browserAuthStore.set(els.provider.value, token, els.storageMode.value);
}

async function loadSavedConfig() {
  const storageMode = localStorage.getItem(STORAGE_MODE_KEY) || sessionStorage.getItem(STORAGE_MODE_KEY) || "session";
  els.storageMode.value = ["memory", "session", "local", "indexeddb"].includes(storageMode) ? storageMode : "session";
  const provider = localStorage.getItem(PROVIDER_KEY) || sessionStorage.getItem(PROVIDER_KEY) || els.provider.value;
  els.provider.value = ["openai", "openai-codex"].includes(provider) ? provider : "openai-codex";

  const token = await readStoredToken();
  if (els.provider.value === "openai" && tokenKind(token) === "codex-access-token") {
    els.provider.value = "openai-codex";
  }
  const endpoint = localStorage.getItem(ENDPOINT_KEY) || sessionStorage.getItem(ENDPOINT_KEY) || els.endpoint.value;
  const model = localStorage.getItem(MODEL_KEY) || sessionStorage.getItem(MODEL_KEY) || els.model.value;

  els.token.value = token;
  els.endpoint.value = endpoint;
  els.model.value = model;
  applyProviderDefaults({
    force:
      els.provider.value === "openai-codex" &&
      (endpoint === OPENAI_RESPONSES_ENDPOINT || model === "codex-mini-latest" || model === "gpt-4.1-mini"),
  });
  els.allowDirectCredentials.checked =
    (localStorage.getItem(DIRECT_CREDENTIALS_KEY) || sessionStorage.getItem(DIRECT_CREDENTIALS_KEY)) === "true";
  syncClientConfig();
  refreshStatus();
  addChatMessage("system", "WASM loaded. Configure Codex auth, then send a prompt.");

  const callback = sessionStorage.getItem(PKCE_CALLBACK_KEY);
  if (callback) {
    sessionStorage.removeItem(PKCE_CALLBACK_KEY);
    const callbackData = JSON.parse(callback);
    append("auth", "OAuth callback captured in sessionStorage.", {
      ...callbackData,
      code: sessionStorage.getItem(PKCE_CODE_KEY) ? "<stored in sessionStorage>" : "<missing>",
    });
  }
}

function applyProviderDefaults({ force = false } = {}) {
  if (els.provider.value === "openai-codex") {
    if (
      force ||
      els.endpoint.value === OPENAI_RESPONSES_ENDPOINT ||
      els.endpoint.value === OPENAI_CODEX_DIRECT_ENDPOINT ||
      !els.endpoint.value.trim()
    ) {
      els.endpoint.value = OPENAI_CODEX_ENDPOINT;
    }
    if (force || els.model.value === "codex-mini-latest" || els.model.value === "gpt-4.1-mini") {
      els.model.value = "gpt-5.5";
    }
    return;
  }

  if (force || els.endpoint.value === OPENAI_CODEX_ENDPOINT || !els.endpoint.value.trim()) {
    els.endpoint.value = OPENAI_RESPONSES_ENDPOINT;
  }
  if (force || els.model.value === "gpt-5.5") {
    els.model.value = "gpt-4.1-mini";
  }
}

async function saveConfig() {
  const token = els.token.value.trim();
  const endpoint = els.endpoint.value.trim();
  const model = els.model.value.trim();

  await writeStoredToken(token);
  localStorage.setItem(PROVIDER_KEY, els.provider.value);
  localStorage.setItem(STORAGE_MODE_KEY, els.storageMode.value);
  localStorage.setItem(ENDPOINT_KEY, endpoint);
  localStorage.setItem(MODEL_KEY, model);
  localStorage.setItem(DIRECT_CREDENTIALS_KEY, String(els.allowDirectCredentials.checked));

  syncClientConfig();
  refreshStatus();
  append("status", `Saved auth/config using ${els.storageMode.value} token storage. Token contents are not logged.`);
  const hint = tokenProviderHint(els.provider.value, token);
  if (hint) {
    append("auth", "Token/provider hint", {
      provider: els.provider.value,
      tokenKind: tokenKind(token),
      hint,
    });
  }
}

async function clearConfig() {
  await browserAuthStore.remove(els.provider.value);
  els.token.value = "";
  client.clearCredential(els.provider.value);
  refreshStatus();
  append("status", "Cleared token from memory, sessionStorage, localStorage, IndexedDB, and WASM memory.");
}

function onAgentEvent(event) {
  append(event.kind, event.message || "", event.data);
  if (event.kind === "textDelta") {
    appendAssistantText(event.message || "");
    return;
  }
  if (event.kind === "done" || event.message === "Real request attempt complete.") {
    if (activeAssistantBubble && !activeAssistantBubble.textContent.trim()) {
      activeAssistantBubble.textContent = "Done.";
    }
    activeAssistantBubble = null;
    return;
  }
  if (event.kind === "error") {
    removeEmptyAssistantBubble();
    addChatMessage("system", event.message || "Request failed. Open Debug for details.");
    return;
  }
  if (event.kind === "providerError") {
    const message = providerErrorMessage(event.data);
    if (message) {
      removeEmptyAssistantBubble();
      addChatMessage("system", message);
    }
    return;
  }
  if (event.kind === "toolCall") {
    updateToolStatus(event.name, "running");
    return;
  }
  if (event.kind === "toolResult") {
    updateToolStatus(event.name, "done");
    return;
  }
}

function providerErrorMessage(data) {
  if (!data || !data.bodyPreview) {
    return null;
  }

  let body = null;
  try {
    body = JSON.parse(data.bodyPreview);
  } catch (_error) {
    body = null;
  }

  const code = body?.error?.code || body?.code;
  const detail = body?.error?.message || body?.detail || data.bodyPreview;
  if (code === "token_expired") {
    return "Codex rejected this token as expired. Clear the saved token, paste a fresh openai-codex.access value from ~/.pi/agent/auth.json, then save again.";
  }
  return `Provider error ${data.status || ""} ${data.statusText || ""}: ${detail}`.trim();
}

async function generatePkce() {
  const verifier = randomBase64Url(32);
  const state = randomBase64Url(24);
  const challenge = await sha256Base64Url(verifier);
  const redirectUri = new URL("./auth/callback/", window.location.href).toString();

  sessionStorage.setItem(PKCE_VERIFIER_KEY, verifier);
  sessionStorage.setItem(PKCE_STATE_KEY, state);

  append("auth", "PKCE material generated and verifier stored in sessionStorage.", {
    flow: "authorization_code_pkce",
    codeChallengeMethod: "S256",
    codeChallenge: challenge,
    state,
    redirectUri,
    verifierStored: true,
    verifierPreview: `${verifier.slice(0, 6)}...${verifier.slice(-6)}`,
    tokenExchange: "Only perform in browser when the provider explicitly supports public clients and CORS on its token endpoint.",
  });
}

function showTokenHelp() {
  append("auth", "Manual token lane", {
    mode: "browser-only-no-mediator",
    steps: [
      "Obtain a bearer/API token outside this page using a supported native Pi/Codex/OpenAI auth flow.",
      "For Pi auth.json, paste openai-codex.access; do not paste refresh, id_token, or accountId.",
      "Paste it into the Token field.",
      "Prefer Memory only or Session tab storage for local testing.",
      "Enable browser sending only for the run that should send the token from this origin.",
      "Use Draft request in Debug to audit endpoint, model, headers, and body shape.",
    ],
    notes: [
      "The UI and WASM client redact token contents from logs.",
      "If the provider blocks browser CORS or does not allow public-client token exchange, this MVP stays on pasted-token import rather than adding a broker.",
    ],
  });
}

async function runRealRequest({ prompt }) {
  syncClientConfig();
  activeToolBubbles = new Map();
  append("user", prompt);
  addChatMessage("user", prompt);
  activeAssistantBubble = addChatMessage("assistant", "");
  currentAbortController = new AbortController();
  els.sendReal.disabled = true;
  els.cancel.disabled = false;
  try {
    await client.sendWithSignal(
      buildAgentRequestWithPrompt(prompt),
      onAgentEvent,
      currentAbortController.signal
    );
    if (activeAssistantBubble && !activeAssistantBubble.textContent.trim()) {
      activeAssistantBubble.textContent = "No assistant text was returned. Open Debug for the provider events.";
    }
  } finally {
    removeEmptyAssistantBubble();
    currentAbortController = null;
    els.sendReal.disabled = false;
    els.cancel.disabled = false;
    refreshStatus();
  }
}

async function main() {
  await init();
  client = new PiClient();
  await loadSavedConfig();
  append("status", "WASM loaded. Real provider mode is ready.");

  els.saveAuth.addEventListener("click", () => {
    saveConfig().catch((error) => append("error", error?.stack || String(error)));
  });
  els.clearAuth.addEventListener("click", () => {
    clearConfig().catch((error) => append("error", error?.stack || String(error)));
  });
  els.chatTab.addEventListener("click", () => setActiveTab("chat"));
  els.debugTab.addEventListener("click", () => setActiveTab("debug"));
  els.allowDirectCredentials.addEventListener("change", () => {
    localStorage.setItem(DIRECT_CREDENTIALS_KEY, String(els.allowDirectCredentials.checked));
    client.setDirectBrowserCredentialsAllowed(els.allowDirectCredentials.checked);
    refreshStatus();
    append(
      "status",
      els.allowDirectCredentials.checked
        ? "Direct browser credential calls enabled for this page instance."
        : "Direct browser credential calls disabled."
    );
  });
  els.storageMode.addEventListener("change", async () => {
    const current = els.token.value.trim();
    els.token.value = current;
    try {
      await writeStoredToken(current);
      localStorage.setItem(STORAGE_MODE_KEY, els.storageMode.value);
      syncClientConfig();
      refreshStatus();
      append("status", `Switched token storage to ${els.storageMode.value}.`);
    } catch (error) {
      append("error", error?.stack || String(error));
    }
  });
  els.token.addEventListener("input", () => {
    syncClientConfig();
    refreshStatus();
  });
  els.endpoint.addEventListener("input", () => {
    syncClientConfig();
    refreshStatus();
  });
  els.model.addEventListener("input", () => {
    syncClientConfig();
    refreshStatus();
  });
  els.provider.addEventListener("change", async () => {
    applyProviderDefaults({ force: true });
    const token = await readStoredToken();
    els.token.value = token;
    syncClientConfig();
    refreshStatus();
    append("status", `Provider switched to ${els.provider.value}. Defaults applied.`);
  });
  els.draftRequest.addEventListener("click", () => {
    syncClientConfig();
    append("draft", client.draftRequest(buildAgentRequest()));
    append("credential", "Credential status", JSON.parse(client.credentialStatusJson(els.provider.value)));
    browserAuthStore
      .status(els.provider.value, els.storageMode.value)
      .then((status) => append("credential", "Browser auth store status", status))
      .catch((error) => append("error", error?.stack || String(error)));
    refreshStatus();
  });
  els.listModels.addEventListener("click", () => {
    syncClientConfig();
    append("models", "Available model hints", JSON.parse(client.listModels(els.provider.value)));
    refreshStatus();
  });
  els.generatePkce.addEventListener("click", () => {
    generatePkce().catch((error) => append("error", error?.stack || String(error)));
  });
  els.tokenHelp.addEventListener("click", showTokenHelp);
  els.sendReal.addEventListener("click", async () => {
    const prompt = els.prompt.value.trim() || "Hello from Pi WASM";
    await runRealRequest({
      prompt,
    }).catch((error) => append("error", error?.stack || String(error)));
    els.prompt.value = "";
  });
  els.cancel.addEventListener("click", () => {
    if (currentAbortController) {
      currentAbortController.abort();
      append("status", "Cancel requested. Browser abort signal sent.");
      addChatMessage("system", "Cancel requested.");
      return;
    }
    client.cancel();
    append("status", "Cancel requested. No real browser fetch is currently running.");
  });
  els.prompt.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      els.sendReal.click();
    }
  });
}

main().catch((error) => {
  console.error(error);
  els.status.textContent = "failed";
  append("error", error?.stack || String(error));
});
