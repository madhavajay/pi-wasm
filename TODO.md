# TODO: Browser WASM Pi

## Goal

Build a browser-runnable version of Pi Rust where the Rust/WASM interface is independent from the UI, but the default browser experience looks and behaves like the native Pi / `pi_agent_rust` TUI.

MVP constraint: run as much as possible in the browser. Credentials may be pasted into the browser app for local experimentation. Browser CORS makes a proxy necessary for the real Codex endpoint, so the app must support both a local development proxy and a hosted proxy option while keeping the UI/WASM app static-hostable.

Target shape:

- `pi_agent_rust/`: upstream Rust source, kept as a submodule or vendored checkout.
- `crates/pi-browser/`: WASM-facing Rust crate that exposes a stable browser API and increasingly reuses `pi_agent_rust` provider, event, and render-state code.
- `web/`: browser UI shell only. It imports the generated WASM package and renders a Pi-style terminal surface instead of a custom chat app.
- `./dev.sh`: builds or watches the WASM package, starts the static web server plus local proxy, and serves the browser UI.
- `web/auth/callback/`: static browser OAuth callback capture route for PKCE experiments.
- Terminal UI: real-provider-only Pi-style interface with raw request/response diagnostics hidden behind a Debug/Inspector pane.
- Proxy selector:
  - direct browser fetch for endpoints that explicitly allow browser CORS;
  - local dev proxy, currently `/proxy/codex/responses`;
  - hosted proxy, starting with Sitegeist's default `https://proxy.mariozechner.at/proxy` shape.

Code ownership rule:

- Keep prototype code in this repo root, outside `pi_agent_rust/` and `litter/`.
- Patch `pi_agent_rust/` only when a small upstream change clearly unlocks reuse.
- Do not patch `litter/`; treat it as reference material only.
- Treat `sitegeist/` as reference material for proxy and extension tooling patterns only.
- Generated WASM output goes under `pkg/` and stays ignored.

## Current Code Findings

- `pi_agent_rust/src/provider.rs` already has the right high-level boundary: `Provider`, `Context`, `StreamOptions`, and streamed `StreamEvent` values.
- Provider implementations such as `src/providers/openai.rs` and `src/providers/anthropic.rs` are mostly request/response translation plus streaming SSE parsing.
- `pi_agent_rust/src/sse.rs` and `src/http/sse.rs` are likely reusable in the browser if the byte stream comes from `fetch`.
- `pi_agent_rust/src/http/client.rs` is not browser compatible because it uses raw TCP/TLS (`asupersync::net::tcp::stream::TcpStream`) and native root certs.
- `pi_agent_rust/src/auth.rs` is not browser compatible as-is because it uses filesystem auth storage, file locks, env vars, local CLI credential discovery, local callback listeners, threads, and shell command resolution.
- `pi_agent_rust/src/pi_wasm.rs` is unrelated to this browser goal. It provides WebAssembly support inside the embedded QuickJS extension runtime using `wasmtime`.
- The full CLI path (`src/main.rs`, `src/interactive.rs`, `src/agent.rs`) pulls in terminal UI, filesystem, SQLite sessions, local tools, threads, and native process behavior. Do not try to compile the whole CLI to `wasm32-unknown-unknown` first.
- `pi_agent_rust/src/interactive.rs` contains the native TUI loop using Bubble Tea, crossterm, textarea, viewport, lipgloss, and glamour. Reuse its state/view ideas, but expect to adapt the terminal host layer for the browser.
- `pi_agent_rust/src/interactive/view.rs` has string-oriented rendering helpers that are better candidates for browser reuse than the raw native terminal event loop.
- `sitegeist/src/sidepanel.ts` uses `@mariozechner/pi-agent-core` for the agent loop, registers tools through a `toolsFactory`, and supports a configurable CORS proxy. The browser automation tools depend on Chrome extension APIs and cannot be copied directly into a normal static web page.
- The prototype now owns the browser transport in `crates/pi-browser`: it constructs browser `fetch` requests in WASM, reads `Response.body.getReader()`, emits chunks, parses basic SSE frames, and accepts a browser `AbortSignal`.

## Current Uncertainties

- The browser-only architecture is viable for the prototype shell, mock agent loop, pasted-token storage, same-origin SSE fixture, and WASM-owned fetch streaming.
- The unresolved MVP risk is real Codex/OpenAI auth from a browser origin: official docs say OpenAI API keys must not be exposed in browser code, and Codex ChatGPT/access-token flows are documented for Codex app/CLI/IDE/local automation rather than public browser `fetch`.
- If there is no supported browser OAuth/token exchange, the MVP path is manual token paste for local experimentation, with no mediator backend. For OpenAI API experiments that means a Platform API key; Codex subscription/access tokens remain CLI/local-automation credentials unless OpenAI documents browser API use.
- The current request body follows an OpenAI Responses-style shape, which official API docs confirm for Platform API keys and `POST /v1/responses`; it is not confirmed for ChatGPT/Codex subscription tokens.
- Live probe with the local Codex auth `tokens.access_token` reached `https://api.openai.com/v1/responses` from Chrome and received a provider HTTP response, but the response was `401` with missing scope `api.responses.write`. This proves the browser transport/CORS path is not blocked for that endpoint, but the Codex subscription access token is not accepted as a Responses API write credential.
- The browser prototype now has an explicit `OpenAI Codex` provider mode copied from Pi's native provider path: `https://chatgpt.com/backend-api/codex/responses`, `chatgpt-account-id` extracted from the pasted access token, `OpenAI-Beta: responses=experimental`, `originator: pi`, and Codex Responses body fields. Live browser probe with the local Codex `tokens.access_token` fails before an HTTP response with `Failed to fetch`, which is the expected browser symptom for CORS/preflight rejection.
- The local Node proxy proves the real Codex token/model path works from the browser UI by moving only the provider HTTP call out of the browser security model.
- A hosted proxy can make the static WASM app deployable without a local server. This is technically no longer "all in browser" for the provider HTTP call, but the app remains static-hostable and does not require a WSGI/app server.

## Official OpenAI/Codex Docs Findings

- OpenAI API authentication uses HTTP bearer API keys, and the docs explicitly say not to expose API keys in browser/client-side code.
- OpenAI Responses API examples use `POST https://api.openai.com/v1/responses` with `Authorization: Bearer $OPENAI_API_KEY`, including `stream: true` for streaming.
- Codex supports ChatGPT sign-in for subscription access and API-key sign-in for usage-based access in the app/CLI/IDE. The CLI can also read `CODEX_ACCESS_TOKEN` via `codex login --with-access-token`.
- Codex access tokens are documented for trusted scripts, schedulers, private CI runners, and local Codex workflows. The docs say to continue using Platform API keys for general OpenAI API calls.
- The Codex TypeScript SDK is documented as server-side Node.js, not a browser SDK.

## Architecture

- [x] Decide whether `pi_agent_rust/` should be repaired into a real git submodule before implementation.
- [x] Add a root workspace for the browser project without forcing the upstream crate to change shape immediately.
- [x] Create `crates/pi-browser` as the only Rust crate that compiles to `wasm32-unknown-unknown`.
- [x] Keep the generated WASM package out of `web/` source code ownership, for example `pkg/pi_browser`.
- [x] Create `web/` as a thin test UI that imports the package from `pkg/pi_browser`.
- [x] Add `./dev.sh` that:
  - builds the WASM crate with `wasm-pack build crates/pi-browser --target web --out-dir ../../pkg/pi_browser`;
  - starts the UI dev server;
  - starts the local provider proxy for development;
  - prints the local URL;
  - keeps rebuild behavior simple at first, then adds watch mode.
- [ ] Replace the current chat-first web UI with a Pi-style terminal surface.
  - Render native-looking sections: model/resource header, transcript, assistant streaming area, prompt input, token/status footer.
  - Keep Debug/Inspector as a separate hidden pane for raw JSON/SSE traffic.
  - Prefer a terminal emulator surface such as `xterm.js` once ANSI/frame rendering is useful; start with a controlled `<pre>`/textarea layout if that keeps the prototype moving.
- [ ] Extract a browser-portable TUI boundary from `pi_agent_rust` instead of compiling the native terminal program unchanged.
  - Desired shape: `update(input_event) -> effects` and `view(width, height) -> frame`.
  - Native CLI frontend keeps `crossterm`/Bubble Tea.
  - Browser frontend feeds DOM keyboard/resize events into WASM and renders returned frames.
- [ ] Identify the smallest `pi_agent_rust` modules to reuse directly in `crates/pi-browser`.
  - Provider request/body construction.
  - SSE parsing.
  - Agent event model.
  - Tool-call loop and tool-result follow-up.
  - TUI state/view formatting that does not require native terminal I/O.

## Rust/WASM Boundary

- [x] Expose a small JS-facing API from `crates/pi-browser`, not the full CLI.
- [x] Evolve the first temporary minimal surface:
  - `PiClient.new(config)`; [x] first temporary `PiBrowserClient.fromConfig(config)` added
  - `PiClient.setCredential(provider, credential)`; [x] first temporary `PiBrowserClient.setCredential(provider, credential)` added
  - `PiClient.listModels()`; [x] first temporary static `PiBrowserClient.listModels(provider)` added
  - `PiClient.send({ provider, model, messages, systemPrompt, tools })`; [x] first temporary `PiBrowserClient.send()` and `PiBrowserClient.sendWithSignal()` added
  - `PiClient.cancel(requestId)`; [x] first temporary `PiBrowserClient.cancelRequest()` plus browser `AbortSignal` added
- [x] Rename or wrap the temporary `PiBrowserClient` API as the final `PiClient` shape once the provider boundary is extracted.
  - `PiClient` is now exported as the preferred JS-facing wrapper; `PiBrowserClient` remains for compatibility while the prototype settles.
- [x] Start with a temporary mock surface:
  - `PiBrowserClient.new()`;
  - `PiBrowserClient.setAuth(token)`;
  - `PiBrowserClient.setCredential(provider, credential)`;
  - `PiBrowserClient.clearCredential(provider)`;
  - `PiBrowserClient.credentialStatusJson(provider)`;
  - `PiBrowserClient.listModels(provider)`;
  - `PiBrowserClient.setDirectBrowserCredentialsAllowed(allowed)`;
  - `PiBrowserClient.setEndpoint(endpoint)`;
  - `PiBrowserClient.setModel(model)`;
  - `PiBrowserClient.sendMock(prompt, onEvent)`;
  - `PiBrowserClient.sendMockStreaming(prompt, onEvent)`;
  - `PiBrowserClient.sendMockStreamingWithSignal(prompt, onEvent, signal)`;
  - `PiBrowserClient.send(request, onEvent)`;
  - `PiBrowserClient.sendWithSignal(request, onEvent, signal)`;
  - `PiBrowserClient.draftRequestJson(prompt)`;
  - `PiBrowserClient.draftRequest(request)`.
- [x] Emit events through one browser-friendly mechanism:
  - `AsyncIterator` if ergonomic with JS glue;
  - callback registration like `onEvent(event)`;
  - or `ReadableStream` if the JS side stays simple.
- [x] Serialize all WASM boundary payloads as JSON initially to avoid binding many Rust structs too early.
- [x] Add fake tool-call event transport before real provider streaming.
- [x] Add real model-driven tool calls after browser HTTP works.
  - [x] Recognize common real provider tool-call stream shapes and emit browser `toolCall` events.
  - [x] Execute supported model-requested browser tools and emit `toolResult` events.
  - [x] Send tool outputs back through the provider follow-up turn loop.

## HTTP Transport

- [x] Split native HTTP from browser HTTP behind a trait, for example:
  - `HttpClient::send(request) -> Stream<ResponseChunk>`;
  - native implementation wraps current `src/http/client.rs`;
  - browser implementation wraps `window.fetch`.
  - Browser crate now has a `ProviderHttpClient` trait and `BrowserHttpClient` implementation around `window.fetch`; native upstream extraction is still pending.
- [x] Keep provider modules using the abstract transport instead of constructing the native `Client` directly.
  - Browser OpenAI Responses path now uses `OpenAiResponsesProvider` for request construction plus `ProviderHttpClient`/`BrowserHttpClient` for transport. Upstream `pi_agent_rust` provider extraction remains future work.
- [x] Reuse the existing SSE parser with a browser byte stream.
  - Browser stream now feeds decoded chunks into `BrowserSseParser`, a browser-compatible parser modeled on `pi_agent_rust/src/sse.rs` line/event semantics.
- [x] Add an initial browser `fetch` transport for direct provider experiments.
- [x] Implement browser streaming from `Response.body.getReader()`.
- [x] Parse basic SSE frames in WASM and emit raw provider events.
- [x] Emit `textDelta` events for common OpenAI/Responses and chat-completion delta shapes.
- [x] Emit `toolCall` events for common OpenAI/Responses and chat-completion tool-call shapes.
- [x] Verify the WASM streaming path against a deterministic browser-only mock SSE provider.
- [x] Replace the temporary SSE parser with reused `pi_agent_rust` SSE code after the provider path is extracted.
  - The ad hoc `\n\n` frame splitter is gone. The local parser mirrors the upstream parser behavior needed in WASM without pulling native stream dependencies into `wasm32-unknown-unknown`.
- [x] Use streaming UTF-8 decoding instead of per-chunk lossy decoding before relying on arbitrary provider payloads.
- [x] Handle CORS errors as first-class browser errors with clear provider-specific messages.
- [x] Make request headers explicit and auditable. Browser builds should not inherit env vars or native credentials.
- [x] Wire browser `AbortController` cancellation into WASM fetch via `AbortSignal`.
- [ ] Add a proxy mode selector to the browser config.
  - `direct`: call the configured endpoint from browser `fetch`.
  - `local`: route through this repo's local dev server proxy.
  - `hosted`: route through a hosted CORS proxy.
- [ ] Add Sitegeist-compatible hosted proxy support.
  - Default candidate: `https://proxy.mariozechner.at/proxy`.
  - Confirm exact request format before relying on it: Sitegeist commonly configures URL fetches as `${proxy}/?url=...`; provider stream requests may use the proxy through `pi-agent-core` stream plumbing.
  - Keep the hosted proxy URL editable in the UI.
  - Redact authorization and account headers in all Debug/Inspector output.
- [ ] Keep local proxy support for development and token validation.
  - Default Codex local path: `/proxy/codex/responses`.
  - Default OpenAI Platform local path: `/proxy/openai/responses`.
  - Local proxy should only forward explicitly selected requests and should not persist credentials.

## Auth And Tokens

- [x] Define a browser-specific `AuthStore` interface in the test UI:
  - `get(provider)`;
  - `set(provider, credential)`;
  - `remove(provider)`;
  - `status(provider)`.
- [x] Implement `BrowserAuthStore` with IndexedDB for persisted credentials.
- [x] Allow `sessionStorage` or in-memory storage for safer local testing.
- [x] Do not port native `AuthStorage` wholesale. It depends on `~/.pi/agent/auth.json`, file locks, env vars, and external CLI credential discovery.
- [x] Do not support `$ENV:` or `$CMD:` credential sources in browser builds.
- [x] Model credentials as:
  - API key;
  - bearer token;
  - OAuth access token plus refresh metadata where allowed; [x] refresh metadata intentionally not implemented for this MVP because no supported browser OAuth/token-exchange flow is documented
  - pasted Codex/OpenAI bearer token for this MVP.
- [x] Add a pasted-token lane that never logs or renders token contents.
- [x] Add an import box for tokens obtained by native Pi/Codex auth if no supported browser OAuth flow exists.
- [x] Add in-UI manual pasted-token import guidance for native Pi/Codex/OpenAI token experiments.
- [x] Prefer `sessionStorage` for pasted tokens by default; allow `localStorage` only behind an explicit UI choice.
- [x] Add an explicit IndexedDB storage mode for persisted browser credentials.
- [x] Add a visible warning when storing credentials in browser storage.

## Browser-Only Auth Strategy

- [x] Treat direct browser API keys/tokens as local-development or explicit BYOK mode only.
- [x] Defer backend token broker/proxy work for the MVP.
- [x] If a provider supports direct browser use, keep it behind an explicit `dangerouslyAllowBrowserCredentials` style switch.
- [x] Add browser PKCE helper generation for Authorization Code with PKCE experiments.
- [x] Do not embed OAuth client secrets in the browser.
- [x] Add a static browser redirect route at `/web/auth/callback/` for OAuth callback capture.
- [x] Store PKCE verifier and OAuth state in `sessionStorage` during login.
- [x] Exchange authorization codes in the browser only when the provider explicitly supports public browser clients and CORS for the token endpoint.
  - Official OpenAI/Codex docs reviewed so far do not document a public browser OAuth/token-exchange flow for this use case; keep exchange disabled.
- [x] If browser OAuth is unsupported, use a manual pasted-token flow from native Pi/Codex auth instead of adding a broker.
- [x] If browser-only mode requires refresh tokens, document the risk and isolate storage per origin.
  - README documents that refresh tokens are not implemented; if later required, they must be origin-isolated, never logged, and stored no more persistently than the selected credential mode.
- [x] Confirm whether the Codex subscription token endpoint accepts browser CORS before wiring real calls.
  - [x] Add gated live browser probe: `PI_WASM_LIVE_TOKEN='...' npm run test:live`.
  - [x] Add `PI_WASM_LIVE_TOKEN_FILE=/path/to/token.txt npm run test:live` so the probe can run without putting secrets in shell history.
  - [x] Write a sanitized token-free live-probe report to `live-results/live-provider-latest.json`.
  - [x] Run the live probe with the local Codex auth `tokens.access_token` and record the result: browser `fetch` reached OpenAI and received HTTP `401` JSON, not a CORS/network failure.
  - [x] Record the auth result: the Codex subscription access token lacks `api.responses.write` for `POST /v1/responses`, so it is not a valid Responses API write credential.
- [x] Confirm whether a ChatGPT/Codex access token can be used from browser `fetch` against the target model endpoint.
  - Official docs do not document ChatGPT/Codex access tokens as general browser `fetch` credentials for model endpoints; Codex access tokens are for trusted Codex local/automation workflows.
- [x] Confirm whether OpenAI Responses API or a Codex-specific endpoint is the correct browser target for subscription tokens.
  - Responses API is the documented target for Platform API keys.
  - Pi's native Codex subscription-token path targets `https://chatgpt.com/backend-api/codex/responses` with `chatgpt-account-id`, `OpenAI-Beta: responses=experimental`, and `originator: pi`.
  - Browser `fetch` to the Pi-compatible ChatGPT Codex endpoint failed before HTTP response with `Failed to fetch`, so the Pi path appears native-only unless ChatGPT changes CORS policy.

## Provider Plan

- [x] Start with Codex/OpenAI browser mode because the MVP target is Codex subscription auth.
- [x] Implement a fake provider first so the WASM agent/tool loop works without network auth.
- [x] Add a manual pasted-token real provider experiment next.
- [x] Add browser PKCE OAuth scaffolding with verifier/state/challenge generation and callback capture.
- [x] Complete browser PKCE OAuth only if the flow can be completed with supported browser redirects and CORS-accessible token exchange.
  - Not completed by design: official docs reviewed do not document a supported browser token exchange for this MVP.
- [x] Keep Anthropic direct BYOK as a later comparison provider, not the MVP.
  - README documents Anthropic direct BYOK as deferred until OpenAI/Codex browser mode is proven.
- [x] Avoid Anthropic consumer OAuth as a default path. The current code already warns that it is not recommended.

## Build Setup

- [x] Add `rust-toolchain.toml` for the browser workspace.
- [x] Add target setup docs for `wasm32-unknown-unknown`.
- [x] Add `wasm-bindgen` and `js-sys` to the browser crate.
- [x] Add `wasm-bindgen-futures` and `web-sys` when real browser fetch/debugging lands.
- [x] Add `ReadableStream`, `ReadableStreamDefaultReader`, and `AbortSignal` browser bindings for streaming fetch.
- [x] Add `serde-wasm-bindgen` and `console_error_panic_hook` when richer JS/Rust payloads or panic diagnostics need them.
- [x] Use a static HTML/JS test UI first to avoid package-manager churn.
- [x] Revisit Vite only if module serving or watch mode becomes awkward.
  - Static serving remains sufficient after Playwright coverage; no Vite needed yet.
- [x] Keep generated files ignored:
  - `pkg/`;
  - `target/`.

## Test UI

- [x] Build only a test harness, not the product UI.
- [x] Complete initial UI controls:
  - provider selector;
  - model input;
  - credential entry;
  - storage mode selector;
  - IndexedDB storage mode;
  - explicit direct-browser-credential enable switch;
  - prompt textarea;
  - run and cancel buttons;
  - streaming output panel;
  - raw event log panel.
- [x] Keep UI source separate from WASM source.
- [x] Add visible auth state without rendering secrets.
- [x] Add a network/CORS diagnostic panel for failed browser provider calls.
- [x] Add initial terminal-like debug event log.
- [x] Add PKCE generation, callback capture, and manual token guidance controls to the test UI.
- [x] Remove mock-provider controls from the UI after switching the MVP back to real-provider-only testing.
- [ ] Convert the main browser UI from custom chat bubbles to a Pi-style terminal interface.
  - Match the visual rhythm of native Pi / `pi_agent_rust`: role labels, assistant block, prompt line, dim key hints, token footer, compact status.
  - Avoid noisy system messages in the transcript; show short status updates in-place like the TUI.
  - Move verbose provider/tool events to Debug/Inspector only.
- [ ] Add provider/proxy configuration UI with sensible Codex defaults.
  - Provider: OpenAI Codex.
  - Model: `gpt-5.5`.
  - Endpoint: Codex native endpoint when using direct/hosted proxy, local proxy path when using local mode.
  - Proxy mode: local by default during `./dev.sh`; hosted selectable for static hosting tests.

## Milestones

### Milestone 1: Skeleton

- [x] Repair or finish submodule registration for `pi_agent_rust/`.
- [x] Add `crates/pi-browser`.
- [x] Add `web/`.
- [x] Add `dev.sh`.
- [x] Confirm the WASM package builds.
- [x] Confirm the UI loads the WASM in a browser.
- [x] Stop or restart any stale `pi-wasm-dev` tmux session before final handoff.
  - Checked with `tmux ls`; no active tmux server/session was present.

### Milestone 2: Browser-Only Agent Loop

- [x] Implement fake WASM-owned tool calls:
  - `get_time`;
  - `random_number`;
  - `echo`;
  - `browser_note`.
- [x] Emit tool call and tool result events to JS.
- [x] Add async event streaming for real browser fetch turns.
- [x] Add async event streaming for mock turns instead of one synchronous mock turn.
- [x] Add cancellation shape even for mock turns.

### Milestone 3: Browser HTTP

- [x] Implement browser `fetch` transport.
- [x] Stream bytes from `fetch` into Rust.
- [x] Parse SSE in WASM.
- [x] Display raw provider events in the test UI.
- [x] Add CORS/error diagnostics.
- [x] Add deterministic same-origin SSE fixture for browser-only transport testing.

### Milestone 4: First Real Codex/OpenAI Call

- [x] Add pasted-token request mode.
- [x] Probe the configured endpoint from browser `fetch`.
- [x] Confirm request headers and CORS behavior without exposing token values in logs.
- [x] Port the smallest provider path needed for one model.
  - Browser crate has a minimal OpenAI Responses provider path for request construction, streaming, tool calls, and tool-output follow-up turns.
- [x] Send a user message through the structured browser request path.
- [x] Add explicit `OpenAI Codex` provider mode that copies Pi's native Codex Responses endpoint, headers, and request body shape.
- [x] Replace the debug `TEST`/`Run real` controls with a chat-first `Send` flow using the selected token/model path.
- [x] Add a local proxy path so the browser UI can test the real Pi/Codex token and endpoint despite CORS.
- [ ] Add hosted proxy mode so the same static WASM app can test real Pi/Codex tokens without a local server.
- [ ] Confirm real OpenAI/Codex SSE payloads produce useful `textDelta` output.
  - [x] Add gated Playwright assertion that a real provider run emits `textDelta` when `PI_WASM_LIVE_TOKEN` is supplied.
  - [x] Add token-file support for the live probe.
  - [x] Add `OPENAI_API_KEY` fallback for the default OpenAI Platform live probe so standard local API-key env setup can run the remaining proof.
  - [x] Write a sanitized token-free live-probe report to `live-results/live-provider-latest.json`.
  - [x] Add sanitized browser evidence fields to the live-probe report: `reachedHttp`, `emittedTextDelta`, `completed`, `failedBeforeHttp`, provider error presence, and terminal tail without token values.
  - [x] Run the live probe with the local Codex `tokens.access_token` against `POST /v1/responses`: HTTP `401`, missing `api.responses.write`, no SSE.
  - [x] Run the live probe with the local Pi auth `openai-codex.access` against Pi's `chatgpt.com/backend-api/codex/responses` using `gpt-5.5`: browser `Failed to fetch` before HTTP response, likely CORS/preflight rejection, no SSE. Native Pi can use the same token/model because it is not subject to browser CORS/preflight.
  - [ ] Run the live probe with a write-scoped OpenAI Platform API key and record whether real Responses SSE emits `textDelta`.
- [x] Display final usage if present.
- [x] Add browser abort/cancel behavior for in-flight real fetches.
- [x] Keep real direct credential calls disabled by default until explicitly enabled in the UI and WASM client.

### Milestone 5: Browser Auth

- [x] Add browser credential store.
- [x] Add BYOK entry and clear credential flows.
- [x] Add PKCE helper functions for browser OAuth.
- [x] Add manual pasted-token import/export guidance.
- [x] Add storage mode selector: memory, session, local.
- [x] Add IndexedDB to the storage mode selector.

### Milestone 6: Hardening

- [x] Add request construction and auth status tests for the browser client invariants.
- [x] Add wasm-bindgen browser tests for JS/WASM boundary behavior.
  - Current local run is blocked by ChromeDriver/Chrome compatibility: Chrome is `148.0.7778.168`; Homebrew chromedriver is `149.0.7827.22`; the runner starts ChromeDriver but receives WebDriver HTTP `404`.
  - No repo-local `chromedriver` binary exists to move. If we pin one, put the matching binary under `bin/` and prepend that directory only for `wasm-pack test`.
- [x] Add native Rust unit tests for SSE splitting, response text extraction, delta extraction, and secret-key redaction.
- [x] Add Playwright tests for UI loading, credential entry, mock streaming, and cancel.
- [x] Add mock provider server or Service Worker test fixture for deterministic SSE.
- [x] Add docs for the security modes: browser BYOK/token paste and browser OAuth.

### Milestone 7: Pi-Style Browser TUI

- [ ] Replace the chat layout with a terminal-like Pi surface.
- [ ] Implement keyboard handling that feels like the native TUI:
  - Enter to send;
  - Shift+Enter for newline;
  - scrollback support;
  - clear/cancel affordances.
- [ ] Add a status/footer line for model, token counts, proxy mode, and auth state.
- [ ] Render tool calls as compact in-place status lines instead of appending noisy log rows.
- [ ] Add a Debug/Inspector tab for:
  - request JSON;
  - response chunks/SSE events;
  - tool call/result payloads;
  - proxy mode and resolved endpoint.
- [ ] Decide whether to use `xterm.js` or a minimal DOM renderer for the first pass.
  - Use minimal DOM renderer if the WASM side returns semantic events.
  - Use `xterm.js` if the WASM side starts returning ANSI terminal frames.

### Milestone 8: Reuse `pi_agent_rust` In Browser WASM

- [ ] Create a browser-compatible feature boundary in `pi_agent_rust`.
  - Disable native terminal I/O for browser builds.
  - Disable native filesystem/auth discovery for browser builds.
  - Disable process/shell tools for browser builds.
  - Disable SQLite/session filesystem persistence for browser builds unless replaced by IndexedDB.
- [ ] Move provider request construction and SSE parsing toward reusable modules.
- [ ] Move shared agent events/tool-call abstractions toward reusable modules.
- [ ] Prototype a browser TUI adapter around `pi_agent_rust` state/view code.
- [ ] Keep the existing native `pi` binary working after each extraction.
- [ ] Add tests that compare native-provider request JSON with browser WASM request JSON for Codex.

## Open Questions

- [x] Is a backend broker acceptable for the MVP? No. MVP is browser-only.
- [x] Which provider should be first for the real browser call? Codex/OpenAI subscription-token path.
- [x] Should prototype code live outside `pi_agent_rust/` and `litter/`? Yes.
- [x] Should this repo own patches to `pi_agent_rust`, or should browser compatibility changes be developed upstream in the submodule first?
  - Keep this repo as the prototype wrapper. Only small, reviewable browser-compatible provider/SSE extraction patches should touch the submodule; broad compatibility refactors should be developed upstream in `pi_agent_rust`.
- [x] Is the target local-only, hosted, or both after the MVP?
  - MVP is local-only. Hosted browser-only credentials need a separate origin/security review.
- [x] Does OpenAI/Codex support a browser-safe OAuth flow for this use case? No documented supported flow found in official docs reviewed; keep PKCE as scaffolding only.
- [x] If browser OAuth is unsupported, what exact native Pi/Codex command should produce the token the user pastes? For Codex local automation, use `codex login --with-access-token` or `CODEX_ACCESS_TOKEN` with `codex exec`; for API experiments, paste a Platform API key from the OpenAI dashboard.
  - For Codex-style `auth.json`, paste `tokens.access_token`; do not paste `tokens.id_token`, `tokens.refresh_token`, or `tokens.account_id`.
- [x] Which endpoint should the pasted token call from the browser? Platform API keys should target `https://api.openai.com/v1/responses`; Pi Codex access tokens target `https://chatgpt.com/backend-api/codex/responses` natively, but that endpoint failed from browser `fetch` before HTTP response.
- [x] Should sessions persist in browser storage, or should the initial real-provider build be stateless?
  - Initial real-provider build is stateless except for the WASM in-memory turn transcript. Credentials persist only through explicit user-selected browser storage modes.
- [ ] Should hosted proxy mode default to Mario Zechner's Sitegeist proxy, or should it only be a selectable preset?
  - Current preference: selectable preset. Default to local proxy in `./dev.sh` to avoid silently sending credentials through a third-party proxy.
- [ ] Does Mario Zechner's proxy support the exact Codex streaming POST shape we need, or only URL-fetch style proxying?
  - Confirm before making it the default hosted mode.
- [ ] How much of the native TUI should be reused verbatim?
  - Current preference: reuse state/view/rendering concepts first, then extract actual Rust modules once browser host boundaries are clear.

## Implementation Notes

- The cleanest path is not "compile the native CLI unchanged to WASM"; it is "extract provider streaming, message types, agent events, and browser-portable TUI view state into a browser-compatible library".
- The native crate needs feature gates if we want to reuse more code directly:
  - native filesystem auth;
  - native HTTP;
  - terminal UI;
  - SQLite sessions;
  - QuickJS extensions;
  - wasmtime host support;
  - process and shell integrations.
- The browser crate should fail closed on auth: no ambient env vars, no hidden local credential discovery, and no automatic secret lookup.
- CORS support is provider and endpoint specific. The test UI should detect and explain CORS failures rather than hiding them behind generic fetch errors.
- Browser-only token handling is inherently higher risk than brokered auth. The MVP accepts that risk for local experimentation and must avoid accidental token display/logging.
- Hosted proxy mode is useful for static hosting, but it changes the trust boundary. The UI must make it obvious which proxy receives credentialed requests.
