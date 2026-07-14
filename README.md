# Pi WASM Prototype

Browser-only prototype for running the Pi Rust agent interface through a small WASM crate and a separate static test UI.

## Layout

- `pi_agent_rust/`: upstream Rust source, present as a submodule/reference checkout.
- `crates/pi-browser/`: WASM-facing Rust crate. This is the browser API surface.
- `web/`: static test UI only.
- `pkg/`: generated `wasm-pack` output, ignored by git.

## Dev

Prerequisites:

- Rust stable with the `wasm32-unknown-unknown` target.
- `wasm-pack`.
- Python 3 for the static file server.

Run:

```sh
./dev.sh
```

Open:

```text
http://127.0.0.1:8787/web/
```

`./dev.sh` builds `crates/pi-browser` with `wasm-pack` and serves the repo root so the UI can import `pkg/pi_browser/pi_browser.js`.

The dev server also exposes local proxy routes for real-provider experiments:

```text
/proxy/codex/responses -> https://chatgpt.com/backend-api/codex/responses
/proxy/openai/responses -> https://api.openai.com/v1/responses
```

The default Codex endpoint in the web UI is `/proxy/codex/responses`, so the browser sends the token to localhost and the local dev server forwards the streaming request to the real Pi/Codex endpoint. This is a temporary local mediator for testing, not the final browser-only architecture.

For a lower-level local-only CORS experiment without the proxy, launch a separate unsafe Chrome profile:

```sh
./dev.sh --unsafe-chrome
```

This disables browser web security for that Chrome profile so the direct Codex browser request can be tested without normal CORS enforcement. Do not use that browser profile for normal browsing.

## Browser Auth Modes

The MVP has no mediator backend. Credentials are handled only in the browser origin running the test UI.

Current OpenAI/Codex docs imply three different credential lanes:

- OpenAI Platform API keys authenticate API calls such as `POST /v1/responses`, but OpenAI says API keys are secret and should not be exposed in client-side browser code.
- Codex ChatGPT sign-in returns access tokens to the Codex CLI/IDE flow and stores/refreshes them locally; this is documented for Codex clients, not as a public browser OAuth flow for arbitrary `fetch` calls.
- Codex access tokens are documented for trusted scripts, schedulers, private CI runners, and `codex login --with-access-token`; for general OpenAI API calls, the docs say to keep using Platform API keys.

So the prototype keeps direct browser credentials behind an explicit local-testing switch. A pasted Platform API key can be used to probe the Responses API from the browser, but CORS and key exposure may still block or make that unsuitable. A Codex access token should be treated as a Codex CLI/local automation credential unless OpenAI documents a browser-safe API use for it.

Supported local test modes:

- Memory token: token stays in the page instance.
- Session token: token is stored in `sessionStorage` for the tab session.
- Local token: token is stored in `localStorage`; use only for deliberate local testing.
- IndexedDB token: token is stored in the `piWasmAuth` IndexedDB database under the selected provider.

For the MVP, agent sessions are stateless apart from the in-memory turn transcript inside the WASM client. Credentials may persist only when the user explicitly selects `sessionStorage`, `localStorage`, or IndexedDB. Browser OAuth refresh tokens are not implemented; if a provider later requires them, they must be origin-isolated, never logged, and stored no more persistently than the user-selected credential mode.

Real direct provider calls are disabled by default. To send a pasted token from the browser, enable `Send token from this browser` in the UI for that page instance, optionally audit `Draft request` in the Debug tab, then send a chat prompt.

## Manual Token Lane

Use this when browser OAuth is unsupported or not yet confirmed:

1. Obtain a bearer/API token outside the page using a supported native Pi/Codex/OpenAI auth flow.
   - For OpenAI API experiments, this means a Platform API key from the OpenAI dashboard.
   - For Codex subscription experiments, the documented non-browser path is a Codex ChatGPT login or Codex access token for CLI/local automation. Browser `fetch` use is not documented as supported.
   - If you are reading a Codex-style `auth.json`, paste `tokens.access_token`. Do not paste `tokens.id_token`, `tokens.refresh_token`, or `tokens.account_id`.
2. Paste the token into the test UI.
3. Prefer Memory or Session storage.
4. Enable `Send token from this browser` only for the test run that should send the token.
5. For Codex subscription-token experiments, select `OpenAI Codex`. The web UI defaults to local proxy endpoint `/proxy/codex/responses`, which forwards to Pi's native Codex Responses path: `https://chatgpt.com/backend-api/codex/responses`. The WASM request still uses the `chatgpt-account-id` header extracted from the access token, `OpenAI-Beta: responses=experimental`, `originator: pi`, and Codex Responses body fields.
6. Send a chat prompt. Inspect the Debug tab for raw request, response, and CORS/provider diagnostics.

The UI and WASM client avoid logging token contents, but browser-only credential use is still higher risk than brokered auth.

## Live Browser Probe

To prove real browser CORS and real provider SSE text deltas, run the gated live probe with a token:

```sh
PI_WASM_LIVE_TOKEN='...' npm run test:live
```

For the default OpenAI Platform provider, the runner also accepts the standard environment variable:

```sh
OPENAI_API_KEY='sk-...' npm run test:live
```

To avoid putting the token directly in shell history, put it in a local ignored file and use:

```sh
PI_WASM_LIVE_TOKEN_FILE=/path/to/token.txt npm run test:live
```

Optional overrides:

```sh
PI_WASM_LIVE_PROVIDER='openai-codex' \
PI_WASM_LIVE_ENDPOINT='https://api.openai.com/v1/responses' \
PI_WASM_LIVE_MODEL='gpt-4.1-mini' \
PI_WASM_LIVE_TOKEN_FILE=/path/to/token.txt \
npm run test:live
```

This test runs the static UI in Chrome, stores the token in memory only, enables browser token sending, sends a real browser `fetch`, requires an HTTP response, requires at least one `textDelta` event, and asserts the token is not rendered in the debug log. If it fails before an HTTP response, the failure is the CORS/network proof needed for the remaining TODO.

Each run also writes a token-free local report to `live-results/live-provider-latest.json`. The report records whether the probe was skipped, passed, or failed, plus endpoint/model metadata and sanitized browser evidence such as `reachedHttp`, `emittedTextDelta`, `completed`, and `failedBeforeHttp`, but never the token value.

Observed local Codex-token result: using `tokens.access_token` from the local Codex auth file reached `https://api.openai.com/v1/responses` from Chrome and returned a provider HTTP `401` JSON response rather than a CORS/network failure. The provider error reported missing `api.responses.write`, so this Codex subscription access token is not a valid Responses API write credential. A successful `textDelta` proof still needs a write-scoped OpenAI Platform key or a documented Codex-specific browser endpoint.

Observed Pi-compatible Codex result: using the local Pi auth `openai-codex.access` token with the copied Pi path `https://chatgpt.com/backend-api/codex/responses` and `gpt-5.5` failed in browser `fetch` before an HTTP response with `Failed to fetch`, after sending the Pi-style Codex headers/body. Native Pi can use that token/model because it is not subject to browser CORS/preflight; this browser-only prototype still cannot prove Codex SSE from that endpoint.

## PKCE Lane

The UI can generate an OAuth Authorization Code with PKCE verifier, state, and challenge. The verifier and state are stored in `sessionStorage`; the code challenge and redirect URI are shown in the event log.

The static callback route is:

```text
http://127.0.0.1:8787/web/auth/callback/
```

Only exchange authorization codes in the browser if the provider explicitly supports public browser clients and CORS on the token endpoint. Do not embed OAuth client secrets in this repo or UI.

## Current Limitations

- The Pi-compatible Codex subscription-token endpoint has been copied and probed. It currently fails before HTTP response from browser `fetch`, likely because ChatGPT does not allow this browser origin through CORS/preflight.
- `./dev.sh --unsafe-chrome` can launch a separate local Chrome profile with browser web security disabled for CORS experiments. This is only for local proof, not a deployable browser auth model.
- The browser SSE parser mirrors the upstream `pi_agent_rust` parser behavior needed by this WASM prototype, but it is still duplicated locally rather than shared from the submodule.
- Model-requested browser tool calls still need to be proven against real OpenAI/Codex SSE payloads.
- Anthropic direct BYOK is intentionally deferred until the OpenAI/Codex browser path is proven.

## Upstream Policy

Prototype code lives in this repo root. Do not patch `litter/`. Keep `pi_agent_rust/` as a submodule/reference until browser-compatible provider and SSE extraction is small, reviewable, and useful upstream; broad browser refactors should be developed upstream in `pi_agent_rust` rather than hidden in this wrapper repo.

The MVP target is local-only. A hosted version would need a separate security review because browser-only pasted credentials are origin-sensitive and should not be exposed to arbitrary hosted origins by default.
