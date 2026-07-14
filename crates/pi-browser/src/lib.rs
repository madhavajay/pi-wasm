use js_sys::{Function, Promise, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortSignal, Headers, ReadableStreamDefaultReader, Request, RequestInit, RequestMode, Response,
};

#[wasm_bindgen(
    inline_js = "export function sleepMs(ms) { return new Promise((resolve) => setTimeout(resolve, ms)); }"
)]
extern "C" {
    #[wasm_bindgen(js_name = sleepMs)]
    fn sleep_ms(ms: u32) -> Promise;
}

#[wasm_bindgen]
pub struct PiBrowserClient {
    token: Option<String>,
    provider: String,
    endpoint: String,
    model: String,
    direct_browser_credentials_allowed: bool,
    transcript: Vec<Turn>,
    cancel_requested: bool,
    request_seq: u32,
}

#[wasm_bindgen]
pub struct PiClient {
    inner: PiBrowserClient,
}

#[derive(Clone, Serialize)]
struct Turn {
    role: String,
    content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentEvent<'a> {
    kind: &'a str,
    message: Option<&'a str>,
    name: Option<&'a str>,
    data: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BrowserSendRequest {
    #[serde(default = "default_provider")]
    provider: String,
    model: Option<String>,
    messages: Option<Vec<BrowserMessage>>,
    system_prompt: Option<String>,
    input: Option<String>,
    prompt: Option<String>,
    tools: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BrowserClientConfig {
    endpoint: Option<String>,
    model: Option<String>,
    direct_browser_credentials_allowed: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct BrowserMessage {
    role: Option<String>,
    content: Value,
}

impl BrowserSendRequest {
    fn from_js_value(value: JsValue) -> Result<Self, JsValue> {
        serde_wasm_bindgen::from_value(value)
            .map_err(|err| JsValue::from_str(&format!("invalid send request: {err}")))
    }

    fn to_prompt(&self) -> Result<String, JsValue> {
        let mut parts = Vec::new();

        if let Some(system_prompt) = self.system_prompt.as_deref().map(str::trim) {
            if !system_prompt.is_empty() {
                parts.push(format!("system: {system_prompt}"));
            }
        }

        if let Some(messages) = self.messages.as_ref() {
            for message in messages {
                let Some(content) = message.content_text() else {
                    continue;
                };
                let content = content.trim();
                if content.is_empty() {
                    continue;
                }
                let role = message
                    .role
                    .as_deref()
                    .map(str::trim)
                    .filter(|role| !role.is_empty())
                    .unwrap_or("user");
                parts.push(format!("{role}: {content}"));
            }
        }

        for direct in [self.input.as_deref(), self.prompt.as_deref()]
            .into_iter()
            .flatten()
        {
            let direct = direct.trim();
            if !direct.is_empty() {
                parts.push(direct.to_string());
            }
        }

        let prompt = parts.join("\n\n");
        if prompt.trim().is_empty() {
            Err(JsValue::from_str(
                "send request requires input, prompt, or at least one message with text content",
            ))
        } else {
            Ok(prompt)
        }
    }
}

impl BrowserMessage {
    fn content_text(&self) -> Option<String> {
        if let Some(text) = self.content.as_str() {
            return Some(text.to_string());
        }

        if let Some(array) = self.content.as_array() {
            let parts = array
                .iter()
                .filter_map(|item| {
                    item.pointer("/text")
                        .or_else(|| item.pointer("/input_text"))
                        .or_else(|| item.pointer("/content"))
                        .and_then(Value::as_str)
                })
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            if !parts.is_empty() {
                return Some(parts.join("\n"));
            }
        }

        self.content
            .pointer("/text")
            .or_else(|| self.content.pointer("/input_text"))
            .or_else(|| self.content.pointer("/content"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
    }
}

fn split_codex_prompt(prompt: &str) -> (String, String) {
    let trimmed = prompt.trim();
    let default_instructions = "You are a helpful assistant.";

    let Some(rest) = trimmed.strip_prefix("system:") else {
        return (default_instructions.to_string(), trimmed.to_string());
    };

    let (instructions, remaining) = rest
        .split_once("\n\n")
        .map(|(head, tail)| (head.trim(), tail.trim()))
        .unwrap_or((rest.trim(), ""));
    let user_text = remaining
        .strip_prefix("user:")
        .map(str::trim)
        .unwrap_or(remaining);

    (
        if instructions.is_empty() {
            default_instructions
        } else {
            instructions
        }
        .to_string(),
        user_text.to_string(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserCredentialStatus {
    provider: String,
    has_credential: bool,
    direct_browser_credentials_allowed: bool,
    storage: &'static str,
}

impl BrowserCredentialStatus {
    fn to_json_value(&self) -> Value {
        json!({
            "provider": self.provider,
            "hasCredential": self.has_credential,
            "directBrowserCredentialsAllowed": self.direct_browser_credentials_allowed,
            "storage": self.storage,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProviderRequestParts {
    endpoint: String,
    method: &'static str,
    authorization: String,
    content_type: &'static str,
    accept: &'static str,
    extra_headers: Vec<(String, String)>,
    body: Value,
    direct_browser_credentials_allowed: bool,
}

#[derive(Default)]
struct ProviderStreamResult {
    text: String,
    tool_calls: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BrowserSseEvent {
    event: Cow<'static, str>,
    data: String,
    id: Option<String>,
    retry: Option<u64>,
}

#[derive(Default)]
struct SseEventOutput {
    delta: Option<String>,
    tool_calls: Vec<Value>,
}

struct ProviderTurnContext<'a> {
    on_event: &'a Function,
    request_id: u32,
    phase: &'a str,
    signal: Option<&'a AbortSignal>,
}

trait ProviderHttpClient {
    fn send<'a>(
        &'a self,
        request_parts: ProviderRequestParts,
        context: ProviderTurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ProviderStreamResult>, JsValue>> + 'a>>;
}

struct BrowserHttpClient;

#[derive(Debug)]
struct BrowserSseParser {
    buffer: String,
    current: BrowserSseEvent,
    has_data: bool,
    bom_checked: bool,
    max_event_data_bytes: usize,
}

#[derive(Clone, Debug)]
struct OpenAiResponsesProvider {
    provider: String,
    endpoint: String,
    model: String,
    direct_browser_credentials_allowed: bool,
}

#[derive(Default)]
struct StreamingUtf8Decoder {
    pending: Vec<u8>,
}

impl StreamingUtf8Decoder {
    fn decode(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        self.drain_complete(false)
    }

    fn finish(&mut self) -> String {
        self.drain_complete(true)
    }

    fn drain_complete(&mut self, finish: bool) -> String {
        let mut decoded = String::new();

        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    decoded.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        decoded.push_str(
                            std::str::from_utf8(&self.pending[..valid_up_to])
                                .expect("valid prefix"),
                        );
                        self.pending.drain(..valid_up_to);
                    }

                    if let Some(error_len) = error.error_len() {
                        decoded.push('\u{FFFD}');
                        self.pending.drain(..error_len);
                        continue;
                    }

                    if finish {
                        decoded.push_str(&String::from_utf8_lossy(&self.pending));
                        self.pending.clear();
                    }
                    break;
                }
            }
        }

        decoded
    }
}

impl ProviderRequestParts {
    fn body_json(&self) -> String {
        self.body.to_string()
    }

    fn redacted_json_value(&self) -> Value {
        let mut headers = serde_json::Map::new();
        headers.insert(
            "Authorization".to_string(),
            json!(if self.authorization.is_empty() {
                "<missing>"
            } else {
                "Bearer <redacted>"
            }),
        );
        headers.insert("Content-Type".to_string(), json!(self.content_type));
        headers.insert("Accept".to_string(), json!(self.accept));
        for (key, value) in &self.extra_headers {
            let redacted = if key.eq_ignore_ascii_case("chatgpt-account-id") {
                "<redacted>"
            } else {
                value.as_str()
            };
            headers.insert(key.clone(), json!(redacted));
        }

        json!({
            "endpoint": self.endpoint,
            "method": self.method,
            "headers": headers,
            "directBrowserCredentialsAllowed": self.direct_browser_credentials_allowed,
            "body": self.body,
        })
    }
}

impl OpenAiResponsesProvider {
    fn new(
        provider: String,
        endpoint: String,
        model: String,
        direct_browser_credentials_allowed: bool,
    ) -> Self {
        Self {
            provider,
            endpoint,
            model,
            direct_browser_credentials_allowed,
        }
    }

    fn codex_mode(&self) -> bool {
        matches!(self.provider.as_str(), "codex" | "openai-codex")
    }

    fn initial_request(
        &self,
        prompt: &str,
        token: &str,
        tools: Option<&Vec<Value>>,
    ) -> ProviderRequestParts {
        let mut body = if self.codex_mode() {
            let (instructions, user_text) = split_codex_prompt(prompt);
            json!({
                "model": self.model,
                "instructions": instructions,
                "input": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": user_text }],
                }],
                "stream": true,
                "tools": tools.cloned().unwrap_or_else(default_fake_tools)
            })
        } else {
            json!({
                "model": self.model,
                "input": prompt,
                "stream": true,
                "tools": tools.cloned().unwrap_or_else(default_fake_tools)
            })
        };
        self.apply_codex_body_fields(&mut body);
        self.request_with_body(token, body)
    }

    fn tool_follow_up_request(
        &self,
        prompt: &str,
        token: &str,
        tool_results: &[Value],
        tools: Option<&Vec<Value>>,
    ) -> ProviderRequestParts {
        let input = tool_results
            .iter()
            .map(|tool_result| {
                let call_id = tool_result
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .or_else(|| tool_result.get("name").and_then(Value::as_str))
                    .unwrap_or("browser_tool_call");
                let output = tool_result
                    .get("output")
                    .cloned()
                    .unwrap_or(Value::Null)
                    .to_string();
                json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                })
            })
            .collect::<Vec<_>>();

        let mut body = if self.codex_mode() {
            let (instructions, _user_text) = split_codex_prompt(prompt);
            json!({
                "model": self.model,
                "instructions": instructions,
                "input": input,
                "stream": true,
                "tools": tools.cloned().unwrap_or_else(default_fake_tools)
            })
        } else {
            json!({
                "model": self.model,
                "input": input,
                "stream": true,
                "tools": tools.cloned().unwrap_or_else(default_fake_tools),
                "metadata": {
                    "pi_wasm_follow_up": "tool_outputs",
                    "original_prompt_preview": prompt.chars().take(200).collect::<String>(),
                }
            })
        };
        self.apply_codex_body_fields(&mut body);
        self.request_with_body(token, body)
    }

    fn request_with_body(&self, token: &str, body: Value) -> ProviderRequestParts {
        let mut extra_headers = Vec::new();
        if self.codex_mode() {
            if let Some(account_id) = extract_chatgpt_account_id(token) {
                extra_headers.push(("chatgpt-account-id".to_string(), account_id));
            }
            extra_headers.push((
                "OpenAI-Beta".to_string(),
                "responses=experimental".to_string(),
            ));
            extra_headers.push(("originator".to_string(), "pi".to_string()));
        }

        ProviderRequestParts {
            endpoint: self.endpoint.clone(),
            method: "POST",
            authorization: if token.trim().is_empty() {
                String::new()
            } else {
                format!("Bearer {}", token.trim())
            },
            content_type: "application/json",
            accept: if self.codex_mode() {
                "text/event-stream"
            } else {
                "application/json, text/event-stream"
            },
            extra_headers,
            direct_browser_credentials_allowed: self.direct_browser_credentials_allowed,
            body,
        }
    }

    fn apply_codex_body_fields(&self, body: &mut Value) {
        if !self.codex_mode() {
            return;
        }
        let Some(object) = body.as_object_mut() else {
            return;
        };
        object.insert("store".to_string(), json!(false));
        object.insert("tool_choice".to_string(), json!("auto"));
        object.insert("parallel_tool_calls".to_string(), json!(true));
        object.insert("text".to_string(), json!({ "verbosity": "medium" }));
        object.insert(
            "include".to_string(),
            json!(["reasoning.encrypted_content"]),
        );
        object.insert(
            "reasoning".to_string(),
            json!({
                "effort": "high",
                "summary": "auto",
            }),
        );
    }
}

impl Default for BrowserSseParser {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            current: BrowserSseEvent::default(),
            has_data: false,
            bom_checked: false,
            max_event_data_bytes: 100 * 1024 * 1024,
        }
    }
}

impl BrowserSseParser {
    fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_max_event_data_bytes(limit: usize) -> Self {
        Self {
            max_event_data_bytes: limit,
            ..Self::default()
        }
    }

    fn feed(&mut self, data: &str) -> Vec<BrowserSseEvent> {
        if data.is_empty() {
            return Vec::new();
        }

        if !self.bom_checked {
            self.bom_checked = true;
            if let Some(stripped) = data.strip_prefix('\u{FEFF}') {
                self.buffer.push_str(stripped);
            } else {
                self.buffer.push_str(data);
            }
        } else {
            self.buffer.push_str(data);
        }

        let mut events = Vec::new();
        loop {
            let Some((line, consumed)) = next_sse_line(&self.buffer) else {
                break;
            };
            self.buffer.drain(..consumed);
            self.process_line(&line, &mut events);
        }
        events
    }

    fn flush(&mut self) -> Option<BrowserSseEvent> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.process_field_line(line.trim_end_matches('\r'));
        }

        self.take_current_event()
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<BrowserSseEvent>) {
        if line.is_empty() {
            if let Some(event) = self.take_current_event() {
                events.push(event);
            } else {
                self.current.event = Cow::Borrowed("message");
                self.current.data.clear();
            }
        } else {
            self.process_field_line(line);
        }
    }

    fn process_field_line(&mut self, line: &str) {
        if line.starts_with(':') {
            return;
        }

        let (field, value) = line
            .split_once(':')
            .map(|(field, value)| (field, value.strip_prefix(' ').unwrap_or(value)))
            .unwrap_or((line, ""));

        match field {
            "event" => self.current.event = intern_sse_event_type(value),
            "data" => self.append_data_line(value),
            "id" if !value.contains('\0') => self.current.id = Some(value.to_string()),
            "retry" => self.current.retry = parse_sse_retry(value),
            _ => {}
        }
    }

    fn append_data_line(&mut self, value: &str) {
        let projected_len = self
            .current
            .data
            .len()
            .saturating_add(value.len())
            .saturating_add(1);
        if projected_len <= self.max_event_data_bytes {
            self.current.data.push_str(value);
            self.current.data.push('\n');
        }
        self.has_data = true;
    }

    fn take_current_event(&mut self) -> Option<BrowserSseEvent> {
        if !self.has_data {
            return None;
        }

        if self.current.data.ends_with('\n') {
            self.current.data.pop();
        }
        if self.current.event.is_empty() {
            self.current.event = Cow::Borrowed("message");
        }
        let next = BrowserSseEvent {
            id: self.current.id.clone(),
            retry: self.current.retry,
            ..Default::default()
        };
        let event = std::mem::replace(&mut self.current, next);
        self.has_data = false;
        Some(event)
    }
}

fn next_sse_line(buffer: &str) -> Option<(String, usize)> {
    let bytes = buffer.as_bytes();
    for (idx, byte) in bytes.iter().enumerate() {
        match *byte {
            b'\n' => return Some((buffer[..idx].trim_end_matches('\r').to_string(), idx + 1)),
            b'\r' => {
                if idx + 1 == bytes.len() {
                    return None;
                }
                let consumed = if bytes[idx + 1] == b'\n' {
                    idx + 2
                } else {
                    idx + 1
                };
                return Some((buffer[..idx].to_string(), consumed));
            }
            _ => {}
        }
    }
    None
}

fn intern_sse_event_type(value: &str) -> Cow<'static, str> {
    match value {
        "message" => Cow::Borrowed("message"),
        "response.completed" => Cow::Borrowed("response.completed"),
        "response.done" => Cow::Borrowed("response.done"),
        "response.failed" => Cow::Borrowed("response.failed"),
        "response.incomplete" => Cow::Borrowed("response.incomplete"),
        "response.output_text.delta" => Cow::Borrowed("response.output_text.delta"),
        "response.output_text.done" => Cow::Borrowed("response.output_text.done"),
        "response.output_item.added" => Cow::Borrowed("response.output_item.added"),
        "response.output_item.done" => Cow::Borrowed("response.output_item.done"),
        "response.content_part.done" => Cow::Borrowed("response.content_part.done"),
        "response.function_call_arguments.delta" => {
            Cow::Borrowed("response.function_call_arguments.delta")
        }
        "response.reasoning_text.delta" => Cow::Borrowed("response.reasoning_text.delta"),
        "response.reasoning_text.done" => Cow::Borrowed("response.reasoning_text.done"),
        "response.reasoning_summary_text.delta" => {
            Cow::Borrowed("response.reasoning_summary_text.delta")
        }
        "response.reasoning_summary_text.done" => {
            Cow::Borrowed("response.reasoning_summary_text.done")
        }
        "response.reasoning_summary_part.done" => {
            Cow::Borrowed("response.reasoning_summary_part.done")
        }
        "response.created" => Cow::Borrowed("response.created"),
        "ping" => Cow::Borrowed("ping"),
        "error" => Cow::Borrowed("error"),
        _ => Cow::Owned(value.to_string()),
    }
}

fn parse_sse_retry(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

#[wasm_bindgen]
impl PiBrowserClient {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        console_error_panic_hook::set_once();
        Self {
            token: None,
            provider: "openai-codex".to_string(),
            endpoint: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            model: "gpt-5.5".to_string(),
            direct_browser_credentials_allowed: false,
            transcript: Vec::new(),
            cancel_requested: false,
            request_seq: 0,
        }
    }

    #[wasm_bindgen(js_name = fromConfig)]
    pub fn from_config(config: JsValue) -> Result<Self, JsValue> {
        let config = if config.is_null() || config.is_undefined() {
            BrowserClientConfig::default()
        } else {
            serde_wasm_bindgen::from_value(config)
                .map_err(|err| JsValue::from_str(&format!("invalid client config: {err}")))?
        };

        let mut client = Self::new();
        if let Some(endpoint) = config.endpoint {
            client.set_endpoint(endpoint);
        }
        if let Some(model) = config.model {
            client.set_model(model);
        }
        if let Some(allowed) = config.direct_browser_credentials_allowed {
            client.set_direct_browser_credentials_allowed(allowed);
        }
        Ok(client)
    }

    #[wasm_bindgen(js_name = configValue)]
    pub fn config_value(&self) -> Result<JsValue, JsValue> {
        to_js_value(&json!({
            "endpoint": self.endpoint,
            "provider": self.provider,
            "model": self.model,
            "hasAuth": self.has_auth(),
            "directBrowserCredentialsAllowed": self.direct_browser_credentials_allowed,
            "turns": self.transcript.len(),
        }))
    }

    #[wasm_bindgen(js_name = setAuth)]
    pub fn set_auth(&mut self, token: String) {
        let trimmed = token.trim();
        self.token = (!trimmed.is_empty()).then(|| trimmed.to_string());
    }

    #[wasm_bindgen(js_name = setCredential)]
    pub fn set_credential(&mut self, provider: String, credential: String) -> Result<(), JsValue> {
        let provider = normalize_provider(&provider);
        if !is_supported_provider(&provider) {
            return Err(JsValue::from_str(
                "Unsupported provider. Browser MVP currently supports openai/codex only.",
            ));
        }
        self.provider = provider;
        self.set_auth(credential);
        Ok(())
    }

    #[wasm_bindgen(js_name = clearAuth)]
    pub fn clear_auth(&mut self) {
        self.token = None;
    }

    #[wasm_bindgen(js_name = clearCredential)]
    pub fn clear_credential(&mut self, provider: String) -> Result<(), JsValue> {
        let provider = normalize_provider(&provider);
        if !is_supported_provider(&provider) {
            return Err(JsValue::from_str(
                "Unsupported provider. Browser MVP currently supports openai/codex only.",
            ));
        }
        self.clear_auth();
        Ok(())
    }

    #[wasm_bindgen(js_name = hasAuth)]
    pub fn has_auth(&self) -> bool {
        self.token.as_ref().is_some_and(|token| !token.is_empty())
    }

    #[wasm_bindgen(js_name = credentialStatusJson)]
    pub fn credential_status_json(&self, provider: String) -> String {
        self.credential_status(&provider)
            .to_json_value()
            .to_string()
    }

    #[wasm_bindgen(js_name = setDirectBrowserCredentialsAllowed)]
    pub fn set_direct_browser_credentials_allowed(&mut self, allowed: bool) {
        self.direct_browser_credentials_allowed = allowed;
    }

    #[wasm_bindgen(js_name = setEndpoint)]
    pub fn set_endpoint(&mut self, endpoint: String) {
        let endpoint = endpoint.trim();
        if !endpoint.is_empty() {
            self.endpoint = endpoint.to_string();
        }
    }

    #[wasm_bindgen(js_name = setModel)]
    pub fn set_model(&mut self, model: String) {
        let model = model.trim();
        if !model.is_empty() {
            self.model = model.to_string();
        }
    }

    #[wasm_bindgen(js_name = configJson)]
    pub fn config_json(&self) -> String {
        json!({
            "endpoint": self.endpoint,
            "provider": self.provider,
            "model": self.model,
            "hasAuth": self.has_auth(),
            "directBrowserCredentialsAllowed": self.direct_browser_credentials_allowed,
            "turns": self.transcript.len(),
        })
        .to_string()
    }

    #[wasm_bindgen(js_name = listModels)]
    pub fn list_models(&self, provider: String) -> String {
        let provider = normalize_provider(&provider);
        let models = if is_codex_provider(&provider) {
            json!([
                {
                    "id": "gpt-5.5",
                    "default": true,
                    "notes": "Pi OpenAI Codex ChatGPT endpoint default from the native provider path."
                },
                {
                    "id": "gpt-5.4",
                    "default": false,
                    "notes": "Pi OpenAI Codex ChatGPT endpoint model hint."
                },
                {
                    "id": "gpt-5.2-codex",
                    "default": false,
                    "notes": "Pi OpenAI Codex ChatGPT endpoint model hint."
                }
            ])
        } else {
            json!([
                {
                    "id": "gpt-4.1-mini",
                    "default": true,
                    "notes": "OpenAI Responses API-compatible Platform API key model placeholder."
                },
                {
                    "id": "gpt-4.1",
                    "default": false,
                    "notes": "OpenAI Responses API-compatible model placeholder."
                }
            ])
        };
        json!({
            "provider": provider,
            "source": "static-browser-mvp",
            "models": models
        })
        .to_string()
    }

    #[wasm_bindgen(js_name = reset)]
    pub fn reset(&mut self) {
        self.transcript.clear();
    }

    #[wasm_bindgen(js_name = cancel)]
    pub fn cancel(&mut self) {
        self.cancel_requested = true;
    }

    #[wasm_bindgen(js_name = cancelRequest)]
    pub fn cancel_request(&mut self, _request_id: u32) {
        self.cancel();
    }

    /// Browser-only MVP agent loop.
    ///
    /// This deliberately does not call OpenAI yet. It proves the WASM-owned
    /// agent/tool event loop and keeps the pasted-token lane ready for the
    /// first real browser endpoint experiment.
    #[wasm_bindgen(js_name = sendMock)]
    pub fn send_mock(&mut self, prompt: String, on_event: &Function) -> Result<(), JsValue> {
        let prompt = prompt.trim().to_string();
        self.cancel_requested = false;
        self.request_seq = self.request_seq.wrapping_add(1);
        let request_id = self.request_seq;
        if prompt.is_empty() {
            emit(
                on_event,
                AgentEvent {
                    kind: "error",
                    message: Some("Prompt is empty."),
                    name: None,
                    data: None,
                },
            )?;
            return Ok(());
        }

        self.transcript.push(Turn {
            role: "user".to_string(),
            content: prompt.clone(),
        });

        emit(
            on_event,
            AgentEvent {
                kind: "status",
                message: Some("Pi WASM agent started."),
                name: None,
                data: Some(json!({
                    "auth": self.has_auth(),
                    "endpoint": self.endpoint,
                    "model": self.model,
                    "requestId": request_id,
                })),
            },
        )?;

        let mut answer = String::from("WASM agent response:");
        let lowered = prompt.to_ascii_lowercase();

        if lowered.contains("time") {
            let result = call_time_tool();
            emit_tool(on_event, "get_time", json!({}), result.clone())?;
            answer.push_str(&format!("\n- Time tool returned {result}."));
        }

        if lowered.contains("random") || lowered.contains("number") {
            let result = call_random_tool(&prompt);
            emit_tool(
                on_event,
                "random_number",
                json!({ "seed": prompt }),
                json!(result),
            )?;
            answer.push_str(&format!("\n- Random number tool returned {result}."));
        }

        if lowered.contains("echo") {
            let result = json!({ "echo": prompt });
            emit_tool(on_event, "echo", json!({ "text": prompt }), result)?;
            answer.push_str("\n- Echo tool mirrored the prompt.");
        }

        if lowered.contains("note") {
            let result = json!({
                "stored": true,
                "note": "Browser note tool is a fake in-memory tool for now."
            });
            emit_tool(
                on_event,
                "browser_note",
                json!({ "action": "write" }),
                result,
            )?;
            answer.push_str("\n- Browser note tool accepted a fake note.");
        }

        if answer == "WASM agent response:" {
            answer.push_str(
                "\n- No fake tool was needed. Try asking for time, random, echo, or note.",
            );
        }

        self.transcript.push(Turn {
            role: "assistant".to_string(),
            content: answer.clone(),
        });

        emit(
            on_event,
            AgentEvent {
                kind: "assistant",
                message: Some(&answer),
                name: None,
                data: None,
            },
        )?;

        emit(
            on_event,
            AgentEvent {
                kind: "done",
                message: Some("Turn complete."),
                name: None,
                data: Some(json!({ "turns": self.transcript.len() })),
            },
        )?;

        Ok(())
    }

    #[wasm_bindgen(js_name = sendMockStreaming)]
    pub async fn send_mock_streaming(
        &mut self,
        prompt: String,
        on_event: Function,
    ) -> Result<(), JsValue> {
        self.send_mock_streaming_inner(prompt, on_event, None).await
    }

    #[wasm_bindgen(js_name = sendMockStreamingWithSignal)]
    pub async fn send_mock_streaming_with_signal(
        &mut self,
        prompt: String,
        on_event: Function,
        signal: AbortSignal,
    ) -> Result<(), JsValue> {
        self.send_mock_streaming_inner(prompt, on_event, Some(signal))
            .await
    }

    async fn send_mock_streaming_inner(
        &mut self,
        prompt: String,
        on_event: Function,
        signal: Option<AbortSignal>,
    ) -> Result<(), JsValue> {
        let prompt = prompt.trim().to_string();
        self.cancel_requested = false;
        self.request_seq = self.request_seq.wrapping_add(1);
        let request_id = self.request_seq;
        if prompt.is_empty() {
            emit(
                &on_event,
                AgentEvent {
                    kind: "error",
                    message: Some("Prompt is empty."),
                    name: None,
                    data: None,
                },
            )?;
            return Ok(());
        }

        self.transcript.push(Turn {
            role: "user".to_string(),
            content: prompt.clone(),
        });

        emit(
            &on_event,
            AgentEvent {
                kind: "status",
                message: Some("Pi WASM streaming mock agent started."),
                name: None,
                data: Some(json!({
                    "auth": self.has_auth(),
                    "endpoint": self.endpoint,
                    "model": self.model,
                    "requestId": request_id,
                })),
            },
        )?;

        delay_ms(80).await?;
        if self.emit_mock_cancelled(&on_event, request_id, signal.as_ref())? {
            return Ok(());
        }

        let mut answer = String::from("WASM agent response:");
        let lowered = prompt.to_ascii_lowercase();

        if lowered.contains("time") {
            let result = call_time_tool();
            emit_tool(&on_event, "get_time", json!({}), result.clone())?;
            answer.push_str(&format!("\n- Time tool returned {result}."));
            delay_ms(80).await?;
            if self.emit_mock_cancelled(&on_event, request_id, signal.as_ref())? {
                return Ok(());
            }
        }

        if lowered.contains("random") || lowered.contains("number") {
            let result = call_random_tool(&prompt);
            emit_tool(
                &on_event,
                "random_number",
                json!({ "seed": prompt }),
                json!(result),
            )?;
            answer.push_str(&format!("\n- Random number tool returned {result}."));
            delay_ms(80).await?;
            if self.emit_mock_cancelled(&on_event, request_id, signal.as_ref())? {
                return Ok(());
            }
        }

        if lowered.contains("echo") {
            let result = json!({ "echo": prompt });
            emit_tool(&on_event, "echo", json!({ "text": prompt }), result)?;
            answer.push_str("\n- Echo tool mirrored the prompt.");
            delay_ms(80).await?;
            if self.emit_mock_cancelled(&on_event, request_id, signal.as_ref())? {
                return Ok(());
            }
        }

        if lowered.contains("note") {
            let result = json!({
                "stored": true,
                "note": "Browser note tool is a fake in-memory tool for now."
            });
            emit_tool(
                &on_event,
                "browser_note",
                json!({ "action": "write" }),
                result,
            )?;
            answer.push_str("\n- Browser note tool accepted a fake note.");
            delay_ms(80).await?;
            if self.emit_mock_cancelled(&on_event, request_id, signal.as_ref())? {
                return Ok(());
            }
        }

        if answer == "WASM agent response:" {
            answer.push_str(
                "\n- No fake tool was needed. Try asking for time, random, echo, or note.",
            );
        }

        self.transcript.push(Turn {
            role: "assistant".to_string(),
            content: answer.clone(),
        });

        emit(
            &on_event,
            AgentEvent {
                kind: "assistant",
                message: Some(&answer),
                name: None,
                data: None,
            },
        )?;

        emit(
            &on_event,
            AgentEvent {
                kind: "done",
                message: Some("Streaming mock turn complete."),
                name: None,
                data: Some(json!({
                    "requestId": request_id,
                    "turns": self.transcript.len()
                })),
            },
        )?;

        Ok(())
    }

    fn emit_mock_cancelled(
        &mut self,
        on_event: &Function,
        request_id: u32,
        signal: Option<&AbortSignal>,
    ) -> Result<bool, JsValue> {
        if !self.cancel_requested && !signal.is_some_and(AbortSignal::aborted) {
            return Ok(false);
        }

        self.cancel_requested = false;
        emit(
            on_event,
            AgentEvent {
                kind: "cancelled",
                message: Some("Mock streaming turn cancelled."),
                name: None,
                data: Some(json!({ "requestId": request_id })),
            },
        )?;
        Ok(true)
    }

    /// Browser-only real request experiment.
    ///
    /// This sends a direct browser `fetch` to the configured endpoint with the
    /// pasted token. It is intentionally diagnostic-first: the token is never
    /// emitted, and failures surface as CORS/HTTP diagnostics for the UI.
    #[wasm_bindgen(js_name = sendReal)]
    pub async fn send_real(&mut self, prompt: String, on_event: Function) -> Result<(), JsValue> {
        self.send_real_inner(prompt, on_event, None).await
    }

    #[wasm_bindgen(js_name = sendRealWithSignal)]
    pub async fn send_real_with_signal(
        &mut self,
        prompt: String,
        on_event: Function,
        signal: JsValue,
    ) -> Result<(), JsValue> {
        let signal = if signal.is_null() || signal.is_undefined() {
            None
        } else {
            Some(signal.dyn_into::<AbortSignal>()?)
        };
        self.send_real_inner_with_tools(prompt, on_event, signal, None)
            .await
    }

    #[wasm_bindgen(js_name = send)]
    pub async fn send(&mut self, request: JsValue, on_event: Function) -> Result<(), JsValue> {
        let request = BrowserSendRequest::from_js_value(request)?;
        self.apply_send_request_config(&request)?;
        let prompt = request.to_prompt()?;
        self.send_real_inner_with_tools(prompt, on_event, None, request.tools.as_ref())
            .await
    }

    #[wasm_bindgen(js_name = sendWithSignal)]
    pub async fn send_with_signal(
        &mut self,
        request: JsValue,
        on_event: Function,
        signal: JsValue,
    ) -> Result<(), JsValue> {
        let signal = if signal.is_null() || signal.is_undefined() {
            None
        } else {
            Some(signal.dyn_into::<AbortSignal>()?)
        };
        let request = BrowserSendRequest::from_js_value(request)?;
        self.apply_send_request_config(&request)?;
        let prompt = request.to_prompt()?;
        self.send_real_inner_with_tools(prompt, on_event, signal, request.tools.as_ref())
            .await
    }

    async fn send_real_inner(
        &mut self,
        prompt: String,
        on_event: Function,
        signal: Option<AbortSignal>,
    ) -> Result<(), JsValue> {
        self.send_real_inner_with_tools(prompt, on_event, signal, None)
            .await
    }

    async fn send_real_inner_with_tools(
        &mut self,
        prompt: String,
        on_event: Function,
        signal: Option<AbortSignal>,
        tools: Option<&Vec<Value>>,
    ) -> Result<(), JsValue> {
        let prompt = prompt.trim().to_string();
        self.cancel_requested = false;
        self.request_seq = self.request_seq.wrapping_add(1);
        let request_id = self.request_seq;

        if prompt.is_empty() {
            emit(
                &on_event,
                AgentEvent {
                    kind: "error",
                    message: Some("Prompt is empty."),
                    name: None,
                    data: None,
                },
            )?;
            return Ok(());
        }

        let Some(token) = self.token.clone().filter(|token| !token.trim().is_empty()) else {
            emit(
                &on_event,
                AgentEvent {
                    kind: "error",
                    message: Some(
                        "Missing pasted token. Add a Codex/OpenAI bearer token or API key first.",
                    ),
                    name: None,
                    data: Some(json!({ "requestId": request_id })),
                },
            )?;
            return Ok(());
        };

        if !self.direct_browser_credentials_allowed {
            emit(
                &on_event,
                AgentEvent {
                    kind: "error",
                    message: Some(
                        "Direct browser credential calls are disabled. Enable the explicit browser credential switch for this local test run.",
                    ),
                    name: None,
                    data: Some(json!({
                        "requestId": request_id,
                        "endpoint": self.endpoint,
                        "token": "<redacted>",
                    })),
                },
            )?;
            return Ok(());
        }

        if is_codex_provider(&self.provider) && extract_chatgpt_account_id(&token).is_none() {
            emit(
                &on_event,
                AgentEvent {
                    kind: "error",
                    message: Some(
                        "Invalid OpenAI Codex access token. Expected a JWT access_token with a chatgpt_account_id claim.",
                    ),
                    name: None,
                    data: Some(json!({
                        "requestId": request_id,
                        "provider": self.provider,
                        "endpoint": self.endpoint,
                        "token": "<redacted>",
                    })),
                },
            )?;
            return Ok(());
        }

        self.transcript.push(Turn {
            role: "user".to_string(),
            content: prompt.clone(),
        });

        emit(
            &on_event,
            AgentEvent {
                kind: "status",
                message: Some("Starting direct browser fetch."),
                name: None,
                data: Some(json!({
                    "requestId": request_id,
                    "endpoint": self.endpoint,
                    "model": self.model,
                    "token": "<redacted>",
                })),
            },
        )?;

        let transport = BrowserHttpClient;
        let provider = self.openai_responses_provider();
        let initial_request = provider.initial_request(&prompt, &token, tools);
        let Some(mut stream_result) = transport
            .send(
                initial_request,
                ProviderTurnContext {
                    on_event: &on_event,
                    request_id,
                    phase: "initial",
                    signal: signal.as_ref(),
                },
            )
            .await?
        else {
            return Ok(());
        };

        if !stream_result.tool_calls.is_empty() {
            let tool_results =
                execute_provider_tool_calls(&on_event, request_id, &stream_result.tool_calls)?;
            emit(
                &on_event,
                AgentEvent {
                    kind: "status",
                    message: Some(
                        "Provider-requested browser tools executed. Sending follow-up provider turn.",
                    ),
                    name: None,
                    data: Some(json!({
                        "requestId": request_id,
                        "toolResults": tool_results,
                    })),
                },
            )?;

            let follow_up_request =
                provider.tool_follow_up_request(&prompt, &token, &tool_results, tools);
            if let Some(follow_up_result) = transport
                .send(
                    follow_up_request,
                    ProviderTurnContext {
                        on_event: &on_event,
                        request_id,
                        phase: "tool_follow_up",
                        signal: signal.as_ref(),
                    },
                )
                .await?
            {
                stream_result = follow_up_result;
            }
        }

        let assistant_text = extract_response_text(&stream_result.text).unwrap_or_else(|| {
            "Received response. See raw preview in providerResponse event.".to_string()
        });
        self.transcript.push(Turn {
            role: "assistant".to_string(),
            content: assistant_text.clone(),
        });
        emit(
            &on_event,
            AgentEvent {
                kind: "assistant",
                message: Some(&assistant_text),
                name: None,
                data: None,
            },
        )?;

        emit(
            &on_event,
            AgentEvent {
                kind: "done",
                message: Some("Real request attempt complete."),
                name: None,
                data: Some(json!({
                    "requestId": request_id,
                    "turns": self.transcript.len()
                })),
            },
        )?;

        Ok(())
    }

    /// Build the JSON we expect to send once browser Codex/OpenAI auth is proven.
    #[wasm_bindgen(js_name = draftRequestJson)]
    pub fn draft_request_json(&self, prompt: String) -> String {
        self.provider_request_parts(&prompt, self.token.as_deref().unwrap_or_default())
            .redacted_json_value()
            .to_string()
    }

    #[wasm_bindgen(js_name = draftRequest)]
    pub fn draft_request(&self, request: JsValue) -> Result<String, JsValue> {
        let request = BrowserSendRequest::from_js_value(request)?;
        let prompt = request.to_prompt()?;
        let mut clone = self.clone_config_only();
        clone.apply_send_request_config(&request)?;
        Ok(clone
            .provider_request_parts_with_tools(
                &prompt,
                clone.token.as_deref().unwrap_or_default(),
                request.tools.as_ref(),
            )
            .redacted_json_value()
            .to_string())
    }
}

#[wasm_bindgen]
impl PiClient {
    #[wasm_bindgen(constructor)]
    pub fn new(config: JsValue) -> Result<Self, JsValue> {
        Ok(Self {
            inner: PiBrowserClient::from_config(config)?,
        })
    }

    #[wasm_bindgen(js_name = fromConfig)]
    pub fn from_config(config: JsValue) -> Result<Self, JsValue> {
        Self::new(config)
    }

    #[wasm_bindgen(js_name = configValue)]
    pub fn config_value(&self) -> Result<JsValue, JsValue> {
        self.inner.config_value()
    }

    #[wasm_bindgen(js_name = configJson)]
    pub fn config_json(&self) -> String {
        self.inner.config_json()
    }

    #[wasm_bindgen(js_name = setAuth)]
    pub fn set_auth(&mut self, token: String) {
        self.inner.set_auth(token);
    }

    #[wasm_bindgen(js_name = setCredential)]
    pub fn set_credential(&mut self, provider: String, credential: String) -> Result<(), JsValue> {
        self.inner.set_credential(provider, credential)
    }

    #[wasm_bindgen(js_name = clearAuth)]
    pub fn clear_auth(&mut self) {
        self.inner.clear_auth();
    }

    #[wasm_bindgen(js_name = clearCredential)]
    pub fn clear_credential(&mut self, provider: String) -> Result<(), JsValue> {
        self.inner.clear_credential(provider)
    }

    #[wasm_bindgen(js_name = hasAuth)]
    pub fn has_auth(&self) -> bool {
        self.inner.has_auth()
    }

    #[wasm_bindgen(js_name = credentialStatusJson)]
    pub fn credential_status_json(&self, provider: String) -> String {
        self.inner.credential_status_json(provider)
    }

    #[wasm_bindgen(js_name = setDirectBrowserCredentialsAllowed)]
    pub fn set_direct_browser_credentials_allowed(&mut self, allowed: bool) {
        self.inner.set_direct_browser_credentials_allowed(allowed);
    }

    #[wasm_bindgen(js_name = setEndpoint)]
    pub fn set_endpoint(&mut self, endpoint: String) {
        self.inner.set_endpoint(endpoint);
    }

    #[wasm_bindgen(js_name = setModel)]
    pub fn set_model(&mut self, model: String) {
        self.inner.set_model(model);
    }

    #[wasm_bindgen(js_name = listModels)]
    pub fn list_models(&self, provider: String) -> String {
        self.inner.list_models(provider)
    }

    #[wasm_bindgen(js_name = reset)]
    pub fn reset(&mut self) {
        self.inner.reset();
    }

    #[wasm_bindgen(js_name = cancel)]
    pub fn cancel(&mut self) {
        self.inner.cancel();
    }

    #[wasm_bindgen(js_name = cancelRequest)]
    pub fn cancel_request(&mut self, request_id: u32) {
        self.inner.cancel_request(request_id);
    }

    #[wasm_bindgen(js_name = sendMock)]
    pub fn send_mock(&mut self, prompt: String, on_event: &Function) -> Result<(), JsValue> {
        self.inner.send_mock(prompt, on_event)
    }

    #[wasm_bindgen(js_name = sendMockStreaming)]
    pub async fn send_mock_streaming(
        &mut self,
        prompt: String,
        on_event: Function,
    ) -> Result<(), JsValue> {
        self.inner.send_mock_streaming(prompt, on_event).await
    }

    #[wasm_bindgen(js_name = sendMockStreamingWithSignal)]
    pub async fn send_mock_streaming_with_signal(
        &mut self,
        prompt: String,
        on_event: Function,
        signal: AbortSignal,
    ) -> Result<(), JsValue> {
        self.inner
            .send_mock_streaming_with_signal(prompt, on_event, signal)
            .await
    }

    #[wasm_bindgen(js_name = sendReal)]
    pub async fn send_real(&mut self, prompt: String, on_event: Function) -> Result<(), JsValue> {
        self.inner.send_real(prompt, on_event).await
    }

    #[wasm_bindgen(js_name = sendRealWithSignal)]
    pub async fn send_real_with_signal(
        &mut self,
        prompt: String,
        on_event: Function,
        signal: JsValue,
    ) -> Result<(), JsValue> {
        self.inner
            .send_real_with_signal(prompt, on_event, signal)
            .await
    }

    #[wasm_bindgen(js_name = send)]
    pub async fn send(&mut self, request: JsValue, on_event: Function) -> Result<(), JsValue> {
        self.inner.send(request, on_event).await
    }

    #[wasm_bindgen(js_name = sendWithSignal)]
    pub async fn send_with_signal(
        &mut self,
        request: JsValue,
        on_event: Function,
        signal: JsValue,
    ) -> Result<(), JsValue> {
        self.inner.send_with_signal(request, on_event, signal).await
    }

    #[wasm_bindgen(js_name = draftRequestJson)]
    pub fn draft_request_json(&self, prompt: String) -> String {
        self.inner.draft_request_json(prompt)
    }

    #[wasm_bindgen(js_name = draftRequest)]
    pub fn draft_request(&self, request: JsValue) -> Result<String, JsValue> {
        self.inner.draft_request(request)
    }
}

impl Default for PiBrowserClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PiBrowserClient {
    fn clone_config_only(&self) -> Self {
        Self {
            token: self.token.clone(),
            provider: self.provider.clone(),
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            direct_browser_credentials_allowed: self.direct_browser_credentials_allowed,
            transcript: Vec::new(),
            cancel_requested: false,
            request_seq: self.request_seq,
        }
    }

    fn apply_send_request_config(&mut self, request: &BrowserSendRequest) -> Result<(), JsValue> {
        let provider = normalize_provider(&request.provider);
        if !is_supported_provider(&provider) {
            return Err(JsValue::from_str(
                "Unsupported provider. Browser MVP currently supports openai/codex only.",
            ));
        }
        self.provider = provider;
        if let Some(model) = request.model.as_deref() {
            self.set_model(model.to_string());
        }
        Ok(())
    }

    fn credential_status(&self, provider: &str) -> BrowserCredentialStatus {
        BrowserCredentialStatus {
            provider: normalize_provider(provider),
            has_credential: self.has_auth(),
            direct_browser_credentials_allowed: self.direct_browser_credentials_allowed,
            storage: "host-ui-owned",
        }
    }

    fn openai_responses_provider(&self) -> OpenAiResponsesProvider {
        OpenAiResponsesProvider::new(
            self.provider.clone(),
            self.endpoint.clone(),
            self.model.clone(),
            self.direct_browser_credentials_allowed,
        )
    }

    fn provider_request_parts(&self, prompt: &str, token: &str) -> ProviderRequestParts {
        self.provider_request_parts_with_tools(prompt, token, None)
    }

    fn provider_request_parts_with_tools(
        &self,
        prompt: &str,
        token: &str,
        tools: Option<&Vec<Value>>,
    ) -> ProviderRequestParts {
        self.openai_responses_provider()
            .initial_request(prompt, token, tools)
    }
}

fn emit_tool(
    on_event: &Function,
    name: &'static str,
    arguments: Value,
    result: Value,
) -> Result<(), JsValue> {
    emit(
        on_event,
        AgentEvent {
            kind: "toolCall",
            message: None,
            name: Some(name),
            data: Some(json!({ "arguments": arguments })),
        },
    )?;
    emit(
        on_event,
        AgentEvent {
            kind: "toolResult",
            message: None,
            name: Some(name),
            data: Some(result),
        },
    )
}

fn execute_provider_tool_calls(
    on_event: &Function,
    request_id: u32,
    tool_calls: &[Value],
) -> Result<Vec<Value>, JsValue> {
    let mut results = Vec::new();

    for tool_call in tool_calls {
        let name = tool_call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("provider_tool_call")
            .to_string();
        let arguments = normalized_tool_arguments(tool_call.get("arguments"));
        let result = execute_browser_tool(&name, &arguments);
        let event_data = json!({
            "requestId": request_id,
            "toolCallId": tool_call.get("id").cloned().unwrap_or(Value::Null),
            "arguments": arguments,
            "result": result,
        });

        emit(
            on_event,
            AgentEvent {
                kind: "toolResult",
                message: Some("Browser tool executed for provider request."),
                name: Some(&name),
                data: Some(event_data.clone()),
            },
        )?;
        results.push(json!({
            "toolCallId": tool_call.get("id").cloned().unwrap_or(Value::Null),
            "name": name,
            "output": result,
        }));
    }

    Ok(results)
}

fn normalized_tool_arguments(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or_else(|_| {
            if text.trim().is_empty() {
                json!({})
            } else {
                json!({ "text": text })
            }
        }),
        Some(value) if value.is_null() => json!({}),
        Some(value) => value.clone(),
        None => json!({}),
    }
}

fn execute_browser_tool(name: &str, arguments: &Value) -> Value {
    match name {
        "get_time" => call_time_tool(),
        "random_number" => {
            let seed = arguments
                .get("seed")
                .or_else(|| arguments.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!(call_random_tool(seed))
        }
        "echo" => {
            let text = arguments
                .get("text")
                .or_else(|| arguments.get("input"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!({ "echo": text })
        }
        "browser_note" => {
            let note = arguments
                .get("note")
                .or_else(|| arguments.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("Browser note tool is a fake in-memory tool for now.");
            json!({
                "stored": true,
                "note": note,
            })
        }
        _ => json!({
            "error": "Unsupported browser tool requested by provider.",
            "name": name,
        }),
    }
}

fn emit(on_event: &Function, event: AgentEvent<'_>) -> Result<(), JsValue> {
    on_event.call1(&JsValue::NULL, &to_js_value(&event)?)?;
    Ok(())
}

fn to_js_value<T>(value: &T) -> Result<JsValue, JsValue>
where
    T: Serialize + ?Sized,
{
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|err| JsValue::from_str(&format!("serialize js value: {err}")))
}

fn normalize_provider(provider: &str) -> String {
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() {
        "openai".to_string()
    } else {
        provider
    }
}

fn is_codex_provider(provider: &str) -> bool {
    matches!(provider, "codex" | "openai-codex")
}

fn is_supported_provider(provider: &str) -> bool {
    matches!(provider, "openai" | "codex" | "openai-codex")
}

fn default_provider() -> String {
    "openai-codex".to_string()
}

fn default_fake_tools() -> Vec<Value> {
    vec![
        json!({ "name": "get_time", "description": "Return the browser time." }),
        json!({ "name": "random_number", "description": "Return a deterministic fake random number." }),
        json!({ "name": "echo", "description": "Echo text back." }),
        json!({ "name": "browser_note", "description": "Store a fake browser note." }),
    ]
}

async fn delay_ms(ms: u32) -> Result<(), JsValue> {
    JsFuture::from(sleep_ms(ms)).await?;
    Ok(())
}

impl ProviderHttpClient for BrowserHttpClient {
    fn send<'a>(
        &'a self,
        request_parts: ProviderRequestParts,
        context: ProviderTurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ProviderStreamResult>, JsValue>> + 'a>> {
        Box::pin(send_browser_provider_request(request_parts, context))
    }
}

async fn send_browser_provider_request(
    request_parts: ProviderRequestParts,
    context: ProviderTurnContext<'_>,
) -> Result<Option<ProviderStreamResult>, JsValue> {
    let on_event = context.on_event;
    let request_id = context.request_id;
    let phase = context.phase;
    let body = request_parts.body_json();

    emit(
        on_event,
        AgentEvent {
            kind: "providerRequest",
            message: Some("Sending browser provider request."),
            name: Some(phase),
            data: Some(json!({
                "requestId": request_id,
                "phase": phase,
                "request": request_parts.redacted_json_value(),
            })),
        },
    )?;

    let headers = Headers::new()?;
    headers.set("Authorization", &request_parts.authorization)?;
    headers.set("Content-Type", request_parts.content_type)?;
    headers.set("Accept", request_parts.accept)?;
    for (key, value) in &request_parts.extra_headers {
        headers.set(key, value)?;
    }

    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(RequestMode::Cors);
    opts.set_headers(&headers);
    opts.set_body(&JsValue::from_str(&body));
    if let Some(signal) = context.signal {
        opts.set_signal(Some(signal));
    }

    let request = Request::new_with_str_and_init(&request_parts.endpoint, &opts)?;
    let Some(window) = web_sys::window() else {
        return Err(JsValue::from_str("window is unavailable"));
    };

    let fetch_result = JsFuture::from(window.fetch_with_request(&request)).await;
    let response_value = match fetch_result {
        Ok(value) => value,
        Err(error) => {
            emit(
                on_event,
                AgentEvent {
                    kind: "error",
                    message: Some(
                        "Browser fetch failed before an HTTP response. This is commonly CORS, DNS, network, or blocked mixed-content policy.",
                    ),
                    name: Some(phase),
                    data: Some(json!({
                        "requestId": request_id,
                        "phase": phase,
                        "endpoint": request_parts.endpoint,
                        "error": js_error_summary(&error),
                    })),
                },
            )?;
            return Ok(None);
        }
    };

    let response: Response = response_value.dyn_into()?;
    let status = response.status();
    let ok = response.ok();
    let status_text = response.status_text();
    let content_type = response
        .headers()
        .get("content-type")?
        .unwrap_or_else(|| "<missing>".to_string());

    emit(
        on_event,
        AgentEvent {
            kind: if ok {
                "providerHeaders"
            } else {
                "providerError"
            },
            message: Some(if ok {
                "Received browser HTTP response."
            } else {
                "Provider returned a non-success HTTP response."
            }),
            name: Some(phase),
            data: Some(json!({
                "requestId": request_id,
                "phase": phase,
                "status": status,
                "statusText": status_text,
                "contentType": content_type,
            })),
        },
    )?;

    let stream_result =
        match read_response_stream(&response, &content_type, request_id, on_event).await {
            Ok(result) => result,
            Err(error) => {
                let summary = js_error_summary(&error);
                let aborted = summary.contains("AbortError")
                    || summary.to_ascii_lowercase().contains("aborted");
                emit(
                    on_event,
                    AgentEvent {
                        kind: if aborted { "cancelled" } else { "error" },
                        message: Some(if aborted {
                            "Browser fetch was aborted."
                        } else {
                            "Browser fetch stream failed while reading response body."
                        }),
                        name: Some(phase),
                        data: Some(json!({
                            "requestId": request_id,
                            "phase": phase,
                            "endpoint": request_parts.endpoint,
                            "error": summary,
                        })),
                    },
                )?;
                return Ok(None);
            }
        };

    emit(
        on_event,
        AgentEvent {
            kind: if ok {
                "providerResponse"
            } else {
                "providerError"
            },
            message: Some(if ok {
                "Received browser HTTP response body."
            } else {
                "Provider returned a non-success HTTP response body."
            }),
            name: Some(phase),
            data: Some(json!({
                "requestId": request_id,
                "phase": phase,
                "status": status,
                "statusText": status_text,
                "contentType": content_type,
                "bodyPreview": redact_preview(&stream_result.text),
            })),
        },
    )?;

    if !ok {
        return Ok(None);
    }

    if let Ok(value) = serde_json::from_str::<Value>(&stream_result.text) {
        emit_usage_if_present(on_event, request_id, &value)?;
    }

    Ok(Some(stream_result))
}

async fn read_response_stream(
    response: &Response,
    content_type: &str,
    request_id: u32,
    on_event: &Function,
) -> Result<ProviderStreamResult, JsValue> {
    let Some(body) = response.body() else {
        emit(
            on_event,
            AgentEvent {
                kind: "providerChunk",
                message: Some("Response has no readable body stream."),
                name: None,
                data: Some(json!({ "requestId": request_id })),
            },
        )?;
        return Ok(ProviderStreamResult::default());
    };

    let reader: ReadableStreamDefaultReader = body.get_reader().dyn_into()?;
    let content_type_lower = content_type.to_ascii_lowercase();
    let is_sse = content_type == "<missing>" || content_type_lower.contains("text/event-stream");
    let mut raw_body = String::new();
    let mut sse_parser = BrowserSseParser::new();
    let mut assistant_text = String::new();
    let mut tool_calls = Vec::new();
    let mut decoder = StreamingUtf8Decoder::default();

    loop {
        let chunk = JsFuture::from(reader.read()).await?;
        let done = Reflect::get(&chunk, &JsValue::from_str("done"))?
            .as_bool()
            .unwrap_or(false);
        if done {
            break;
        }

        let value = Reflect::get(&chunk, &JsValue::from_str("value"))?;
        if value.is_undefined() || value.is_null() {
            continue;
        }

        let bytes = Uint8Array::new(&value).to_vec();
        let decoded_text = decoder.decode(&bytes);
        raw_body.push_str(&decoded_text);

        emit(
            on_event,
            AgentEvent {
                kind: "providerChunk",
                message: Some("Received response body chunk."),
                name: None,
                data: Some(json!({
                    "requestId": request_id,
                    "bytes": bytes.len(),
                    "preview": redact_preview(&decoded_text),
                })),
            },
        )?;

        if is_sse && !decoded_text.is_empty() {
            for event in sse_parser.feed(&decoded_text) {
                let output = emit_sse_event(on_event, request_id, &event)?;
                if let Some(delta) = output.delta {
                    assistant_text.push_str(&delta);
                }
                tool_calls.extend(output.tool_calls);
            }
        }
    }

    ReadableStreamDefaultReader::release_lock(&reader);
    let trailing_text = decoder.finish();
    if !trailing_text.is_empty() {
        raw_body.push_str(&trailing_text);
        if is_sse {
            for event in sse_parser.feed(&trailing_text) {
                let output = emit_sse_event(on_event, request_id, &event)?;
                if let Some(delta) = output.delta {
                    assistant_text.push_str(&delta);
                }
                tool_calls.extend(output.tool_calls);
            }
        }
    }

    if is_sse {
        if let Some(event) = sse_parser.flush() {
            let output = emit_sse_event(on_event, request_id, &event)?;
            if let Some(delta) = output.delta {
                assistant_text.push_str(&delta);
            }
            tool_calls.extend(output.tool_calls);
        }
        if !assistant_text.trim().is_empty() {
            return Ok(ProviderStreamResult {
                text: json!({ "output_text": assistant_text }).to_string(),
                tool_calls,
            });
        }
    }

    Ok(ProviderStreamResult {
        text: raw_body,
        tool_calls,
    })
}

fn emit_sse_event(
    on_event: &Function,
    request_id: u32,
    event: &BrowserSseEvent,
) -> Result<SseEventOutput, JsValue> {
    let event_name = event.event.as_ref();
    let data = event.data.as_str();
    if data.trim() == "[DONE]" {
        emit(
            on_event,
            AgentEvent {
                kind: "providerEvent",
                message: Some("SSE stream completed."),
                name: Some("done"),
                data: Some(json!({ "requestId": request_id })),
            },
        )?;
        return Ok(SseEventOutput::default());
    }

    let parsed = serde_json::from_str::<Value>(&data).ok();
    let mut output = SseEventOutput::default();
    emit(
        on_event,
        AgentEvent {
            kind: "providerEvent",
            message: Some("Received SSE event."),
            name: Some(event_name),
            data: Some(json!({
                "requestId": request_id,
                "raw": redact_preview(&data),
                "json": parsed,
                "id": event.id,
                "retry": event.retry,
            })),
        },
    )?;

    if let Some(value) = parsed.as_ref() {
        emit_usage_if_present(on_event, request_id, value)?;
        let tool_calls = extract_provider_tool_calls(&event_name, value);
        for tool_call in &tool_calls {
            let name = tool_call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("provider_tool_call")
                .to_string();
            emit(
                on_event,
                AgentEvent {
                    kind: "toolCall",
                    message: Some("Provider requested a tool call."),
                    name: Some(&name),
                    data: Some(json!({
                        "requestId": request_id,
                        "event": event_name,
                        "toolCall": tool_call,
                    })),
                },
            )?;
        }
        output.tool_calls = tool_calls;
    }

    let delta = parsed.as_ref().and_then(extract_delta_text);
    if let Some(delta) = delta.as_deref() {
        emit(
            on_event,
            AgentEvent {
                kind: "textDelta",
                message: Some(delta),
                name: None,
                data: Some(json!({ "requestId": request_id, "event": event_name })),
            },
        )?;
    }

    output.delta = delta;
    Ok(output)
}

fn emit_usage_if_present(
    on_event: &Function,
    request_id: u32,
    value: &Value,
) -> Result<(), JsValue> {
    let Some(usage) = extract_usage(value) else {
        return Ok(());
    };

    emit(
        on_event,
        AgentEvent {
            kind: "usage",
            message: Some("Provider usage reported."),
            name: None,
            data: Some(json!({
                "requestId": request_id,
                "usage": usage,
            })),
        },
    )
}

fn call_time_tool() -> Value {
    let date = js_sys::Date::new_0();
    json!({
        "iso": date.to_iso_string().as_string().unwrap_or_default(),
        "locale": date.to_locale_string("en-US", &JsValue::UNDEFINED).as_string().unwrap_or_default(),
    })
}

fn call_random_tool(seed: &str) -> u32 {
    let mut hash = 2_166_136_261u32;
    for byte in seed.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash % 100) + 1
}

fn extract_response_text(text: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(text).ok()?;

    if let Some(output_text) = value.get("output_text").and_then(Value::as_str) {
        let trimmed = output_text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(content) = value
        .pointer("/output/0/content/0/text")
        .and_then(Value::as_str)
    {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(content) = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
    {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

fn extract_delta_text(value: &Value) -> Option<String> {
    for pointer in [
        "/delta",
        "/output_text",
        "/choices/0/delta/content",
        "/choices/0/text",
        "/message/content",
    ] {
        if let Some(text) = value.pointer(pointer).and_then(Value::as_str) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }

    None
}

fn extract_provider_tool_calls(event_name: &str, value: &Value) -> Vec<Value> {
    let mut calls = Vec::new();

    if event_name == "response.output_item.done" {
        if let Some(item) = value.get("item").filter(|item| item.is_object()) {
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            if item_type.contains("function_call") || item.get("name").is_some() {
                calls.push(normalize_provider_tool_call(event_name, item));
            }
        }
    }

    if !event_name.starts_with("response.")
        && let Some(item) = value.get("item").filter(|item| item.is_object())
    {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if item_type.contains("function_call") || item.get("name").is_some() {
            calls.push(normalize_provider_tool_call(event_name, item));
        }
    }

    if let Some(tool_calls) = value
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for tool_call in tool_calls {
            calls.push(normalize_provider_tool_call(event_name, tool_call));
        }
    }

    calls
}

fn normalize_provider_tool_call(event_name: &str, value: &Value) -> Value {
    let function = value.get("function").unwrap_or(value);
    let name = value
        .get("name")
        .or_else(|| function.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("provider_tool_call");
    let arguments = value
        .get("arguments")
        .or_else(|| function.get("arguments"))
        .or_else(|| value.get("delta"))
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "id": value.get("id").or_else(|| value.get("call_id")).or_else(|| value.get("item_id")).cloned().unwrap_or(Value::Null),
        "name": name,
        "arguments": arguments,
        "raw": value,
        "sourceEvent": event_name,
    })
}

fn extract_usage(value: &Value) -> Option<Value> {
    for pointer in ["/usage", "/response/usage"] {
        if let Some(usage) = value.pointer(pointer).filter(|usage| usage.is_object()) {
            return Some(normalize_usage(usage));
        }
    }

    value
        .is_object()
        .then(|| normalize_usage(value))
        .filter(|usage| {
            usage.as_object().is_some_and(|object| {
                object.contains_key("input_tokens") || object.contains_key("prompt_tokens")
            })
        })
}

fn normalize_usage(usage: &Value) -> Value {
    let mut usage = usage.clone();
    let Some(object) = usage.as_object_mut() else {
        return usage;
    };

    if !object.contains_key("total_tokens") {
        let input = token_count(object.get("input_tokens"))
            .or_else(|| token_count(object.get("prompt_tokens")));
        let output = token_count(object.get("output_tokens"))
            .or_else(|| token_count(object.get("completion_tokens")));
        if let (Some(input), Some(output)) = (input, output) {
            object.insert("total_tokens".to_string(), json!(input + output));
        }
    }

    usage
}

fn token_count(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}

fn extract_chatgpt_account_id(token: &str) -> Option<String> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let decoded = base64_url_decode(payload)?;
    let payload_json: Value = serde_json::from_slice(&decoded).ok()?;
    payload_json
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut bit_count = 0u8;
    let mut output = Vec::new();

    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a' + 26),
            b'0'..=b'9' => u32::from(byte - b'0' + 52),
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        bits = (bits << 6) | value;
        bit_count += 6;
        if bit_count >= 8 {
            bit_count -= 8;
            output.push(((bits >> bit_count) & 0xff) as u8);
        }
    }

    Some(output)
}

fn redact_preview(text: &str) -> String {
    let preview = text.chars().take(4_000).collect::<String>();
    preview
        .replace("access_token", "access_[redacted]")
        .replace("refresh_token", "refresh_[redacted]")
        .replace("id_token", "id_[redacted]")
}

fn js_error_summary(value: &JsValue) -> String {
    if let Some(text) = value.as_string() {
        return text;
    }
    Reflect::get(value, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}

#[wasm_bindgen(js_name = readObjectPath)]
pub fn read_object_path(object: &JsValue, path: String) -> JsValue {
    let mut current = object.clone();
    for segment in path.split('.') {
        if segment.is_empty() {
            return JsValue::UNDEFINED;
        }
        match Reflect::get(&current, &JsValue::from_str(segment)) {
            Ok(value) => current = value,
            Err(_) => return JsValue::UNDEFINED,
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_sse_parser_handles_split_lf_crlf_and_flush() {
        let mut parser = BrowserSseParser::new();
        let mut events = parser.feed("event: one\ndata: {\"delta\":\"hel\"}\n\nevent: two\r\n");
        events.extend(
            parser.feed("id: 7\r\nretry: 250\r\ndata: {\"delta\":\"lo\"}\r\n\r\ndata: partial"),
        );
        let flushed = parser.flush().expect("flushed partial event");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_ref(), "one");
        assert_eq!(events[0].data, "{\"delta\":\"hel\"}");
        assert_eq!(events[1].event.as_ref(), "two");
        assert_eq!(events[1].id.as_deref(), Some("7"));
        assert_eq!(events[1].retry, Some(250));
        assert_eq!(events[1].data, "{\"delta\":\"lo\"}");
        assert_eq!(flushed.event.as_ref(), "message");
        assert_eq!(flushed.data, "partial");
    }

    #[test]
    fn browser_sse_parser_handles_bom_comments_and_data_limit() {
        let mut parser = BrowserSseParser::with_max_event_data_bytes(1);
        let events = parser.feed(
            "\u{FEFF}:keepalive\nevent: response.output_text.delta\ndata: abcd\ndata: ok\n\n",
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_ref(), "response.output_text.delta");
        assert_eq!(events[0].data, "");
    }

    #[test]
    fn streaming_utf8_decoder_preserves_split_multibyte_text() {
        let mut decoder = StreamingUtf8Decoder::default();
        let bytes = "data: {\"delta\":\"hello 🌏\"}\n\n".as_bytes();
        let split = bytes
            .windows(4)
            .position(|window| window == "🌏".as_bytes())
            .expect("emoji bytes");

        let mut decoded = String::new();
        decoded.push_str(&decoder.decode(&bytes[..split + 2]));
        decoded.push_str(&decoder.decode(&bytes[split + 2..]));
        decoded.push_str(&decoder.finish());

        assert_eq!(decoded, "data: {\"delta\":\"hello 🌏\"}\n\n");
        assert!(!decoded.contains('\u{FFFD}'));
    }

    #[test]
    fn streaming_utf8_decoder_flushes_incomplete_tail() {
        let mut decoder = StreamingUtf8Decoder::default();
        let bytes = "🌏".as_bytes();

        let decoded = decoder.decode(&bytes[..2]);
        let flushed = decoder.finish();

        assert!(decoded.is_empty());
        assert_eq!(flushed, "\u{FFFD}");
    }

    #[test]
    fn extracts_common_response_text_shapes() {
        assert_eq!(
            extract_response_text(r#"{"output_text":"hello"}"#).as_deref(),
            Some("hello")
        );
        assert_eq!(
            extract_response_text(r#"{"output":[{"content":[{"text":"from responses"}]}]}"#)
                .as_deref(),
            Some("from responses")
        );
        assert_eq!(
            extract_response_text(r#"{"choices":[{"message":{"content":"from chat"}}]}"#)
                .as_deref(),
            Some("from chat")
        );
    }

    #[test]
    fn extracts_common_delta_shapes() {
        assert_eq!(
            extract_delta_text(&json!({"delta": "a"})).as_deref(),
            Some("a")
        );
        assert_eq!(
            extract_delta_text(&json!({"choices": [{"delta": {"content": "b"}}]})).as_deref(),
            Some("b")
        );
        assert_eq!(
            extract_delta_text(&json!({"output_text": "c"})).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn extracts_common_provider_tool_call_shapes() {
        let responses_calls = extract_provider_tool_calls(
            "response.output_item.done",
            &json!({
                "item": {
                    "id": "call_1",
                    "type": "function_call",
                    "name": "get_time",
                    "arguments": "{}"
                }
            }),
        );
        assert_eq!(
            responses_calls
                .first()
                .and_then(|call| call.pointer("/name").and_then(Value::as_str)),
            Some("get_time")
        );

        let arguments_calls = extract_provider_tool_calls(
            "response.function_call_arguments.delta",
            &json!({
                "item_id": "call_1",
                "delta": "{\"timezone\":\"UTC\""
            }),
        );
        assert!(arguments_calls.is_empty());

        let chat_calls = extract_provider_tool_calls(
            "message",
            &json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "id": "call_2",
                            "function": {
                                "name": "echo",
                                "arguments": "{\"text\":\"hi\"}"
                            }
                        }]
                    }
                }]
            }),
        );
        assert_eq!(
            chat_calls
                .first()
                .and_then(|call| call.pointer("/name").and_then(Value::as_str)),
            Some("echo")
        );
    }

    #[test]
    fn codex_tool_call_extraction_ignores_partial_events() {
        assert!(
            extract_provider_tool_calls(
                "response.output_item.added",
                &json!({
                    "item": {
                        "id": "call_1",
                        "type": "function_call",
                        "name": "get_time",
                        "arguments": "{}"
                    }
                }),
            )
            .is_empty()
        );

        assert!(
            extract_provider_tool_calls(
                "response.function_call_arguments.delta",
                &json!({
                    "item_id": "call_1",
                    "delta": "{}"
                }),
            )
            .is_empty()
        );
        assert!(
            extract_provider_tool_calls(
                "response.function_call_arguments.done",
                &json!({
                    "item_id": "call_1",
                    "arguments": "{}"
                }),
            )
            .is_empty()
        );

        let calls = extract_provider_tool_calls(
            "response.output_item.done",
            &json!({
                "item": {
                    "id": "call_1",
                    "type": "function_call",
                    "name": "get_time",
                    "arguments": "{}"
                }
            }),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].pointer("/name").and_then(Value::as_str),
            Some("get_time")
        );
    }

    #[test]
    fn codex_follow_up_request_includes_instructions() {
        let provider = OpenAiResponsesProvider::new(
            "openai-codex".to_string(),
            "https://chatgpt.com/backend-api/codex/responses".to_string(),
            "gpt-5.5".to_string(),
            true,
        );
        let request = provider.tool_follow_up_request(
            "system: Use tools when useful.\n\nuser: what time is it?",
            "secret-token",
            &[json!({
                "toolCallId": "call_1",
                "name": "get_time",
                "output": { "iso": "2026-05-22T03:20:00.000Z" }
            })],
            None,
        );

        assert_eq!(
            request
                .body
                .pointer("/instructions")
                .and_then(Value::as_str),
            Some("Use tools when useful.")
        );
        assert_eq!(
            request
                .body
                .pointer("/input/0/type")
                .and_then(Value::as_str),
            Some("function_call_output")
        );
        assert_eq!(
            request
                .body
                .pointer("/input/0/call_id")
                .and_then(Value::as_str),
            Some("call_1")
        );
        assert!(request.body.pointer("/metadata").is_none());
    }

    #[test]
    fn normalizes_and_executes_provider_tool_arguments() {
        let parsed = normalized_tool_arguments(Some(&json!(r#"{"text":"hi"}"#)));
        assert_eq!(parsed.pointer("/text").and_then(Value::as_str), Some("hi"));

        let fallback = normalized_tool_arguments(Some(&json!("not json")));
        assert_eq!(
            fallback.pointer("/text").and_then(Value::as_str),
            Some("not json")
        );

        assert_eq!(
            execute_browser_tool("echo", &parsed)
                .pointer("/echo")
                .and_then(Value::as_str),
            Some("hi")
        );
        assert_eq!(
            execute_browser_tool("browser_note", &json!({ "note": "remember this" }))
                .pointer("/stored")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            execute_browser_tool("missing_tool", &json!({}))
                .pointer("/error")
                .and_then(Value::as_str),
            Some("Unsupported browser tool requested by provider.")
        );
    }

    #[test]
    fn extracts_and_normalizes_common_usage_shapes() {
        assert_eq!(
            extract_usage(&json!({
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5
                }
            }))
            .and_then(|usage| usage.pointer("/total_tokens").and_then(Value::as_u64)),
            Some(15)
        );
        assert_eq!(
            extract_usage(&json!({
                "usage": {
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "total_tokens": 10
                }
            }))
            .and_then(|usage| usage.pointer("/total_tokens").and_then(Value::as_u64)),
            Some(10)
        );
        assert!(extract_usage(&json!({ "delta": "hello" })).is_none());
    }

    #[test]
    fn redacts_token_key_names_in_previews() {
        let preview = redact_preview(r#"{"access_token":"a","refresh_token":"b","id_token":"c"}"#);

        assert!(!preview.contains("access_token"));
        assert!(!preview.contains("refresh_token"));
        assert!(!preview.contains("id_token"));
        assert!(preview.contains("access_[redacted]"));
        assert!(preview.contains("refresh_[redacted]"));
        assert!(preview.contains("id_[redacted]"));
    }

    #[test]
    fn request_parts_include_streaming_tools_and_redacted_preview() {
        let mut client = PiBrowserClient::new();
        client
            .set_credential("openai".to_string(), "secret-token".to_string())
            .expect("set provider");
        client.set_endpoint("https://example.test/v1/responses".to_string());
        client.set_model("mock-model".to_string());
        client.set_direct_browser_credentials_allowed(true);

        let request = client.provider_request_parts("hello", "secret-token");
        let redacted = request.redacted_json_value();

        assert_eq!(request.endpoint, "https://example.test/v1/responses");
        assert_eq!(request.method, "POST");
        assert_eq!(request.authorization, "Bearer secret-token");
        assert_eq!(request.content_type, "application/json");
        assert_eq!(request.accept, "application/json, text/event-stream");
        assert_eq!(
            request.body.pointer("/model").and_then(Value::as_str),
            Some("mock-model")
        );
        assert_eq!(
            request.body.pointer("/input").and_then(Value::as_str),
            Some("hello")
        );
        assert_eq!(
            request.body.pointer("/stream").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            request
                .body
                .pointer("/tools")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(4)
        );
        assert_eq!(
            redacted
                .pointer("/headers/Authorization")
                .and_then(Value::as_str),
            Some("Bearer <redacted>")
        );
        assert!(!redacted.to_string().contains("secret-token"));
    }

    #[test]
    fn openai_responses_provider_builds_initial_request_without_native_client() {
        let provider = OpenAiResponsesProvider::new(
            "openai".to_string(),
            "https://example.test/v1/responses".to_string(),
            "mock-model".to_string(),
            true,
        );
        let request = provider.initial_request("hello", "secret-token", None);

        assert_eq!(request.endpoint, "https://example.test/v1/responses");
        assert_eq!(request.method, "POST");
        assert_eq!(request.authorization, "Bearer secret-token");
        assert_eq!(
            request.body.pointer("/model").and_then(Value::as_str),
            Some("mock-model")
        );
        assert_eq!(
            request.body.pointer("/input").and_then(Value::as_str),
            Some("hello")
        );
        assert_eq!(
            request.body.pointer("/stream").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn codex_provider_copies_pi_endpoint_headers_and_body_shape() {
        let mut client = PiBrowserClient::new();
        client.set_credential(
            "openai-codex".to_string(),
            "eyJhbGciOiJub25lIn0.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjdF8xMjMifX0.sig".to_string(),
        )
        .expect("credential");
        client.set_endpoint("https://chatgpt.com/backend-api/codex/responses".to_string());
        client.set_model("gpt-5.5".to_string());
        client.set_direct_browser_credentials_allowed(true);

        let request = client.provider_request_parts("hello", client.token.as_deref().unwrap());
        let headers = request.redacted_json_value();

        assert_eq!(
            request.endpoint,
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            request.body.pointer("/tool_choice").and_then(Value::as_str),
            Some("auto")
        );
        assert_eq!(
            request
                .body
                .pointer("/parallel_tool_calls")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            request.body.pointer("/include/0").and_then(Value::as_str),
            Some("reasoning.encrypted_content")
        );
        assert_eq!(
            request
                .extra_headers
                .iter()
                .find(|(key, _)| key == "chatgpt-account-id")
                .map(|(_, value)| value.as_str()),
            Some("acct_123")
        );
        assert_eq!(
            headers
                .pointer("/headers/chatgpt-account-id")
                .and_then(Value::as_str),
            Some("<redacted>")
        );
        assert_eq!(
            headers
                .pointer("/headers/OpenAI-Beta")
                .and_then(Value::as_str),
            Some("responses=experimental")
        );
        assert!(!headers.to_string().contains("acct_123"));
    }

    #[test]
    fn follow_up_request_parts_include_function_call_outputs() {
        let provider = OpenAiResponsesProvider::new(
            "openai".to_string(),
            "https://api.openai.com/v1/responses".to_string(),
            "mock-model".to_string(),
            false,
        );
        let request = provider.tool_follow_up_request(
            "original prompt",
            "secret-token",
            &[json!({
                "toolCallId": "call_1",
                "name": "echo",
                "output": { "echo": "hi" }
            })],
            None,
        );

        assert_eq!(
            request
                .body
                .pointer("/input/0/type")
                .and_then(Value::as_str),
            Some("function_call_output")
        );
        assert_eq!(
            request
                .body
                .pointer("/input/0/call_id")
                .and_then(Value::as_str),
            Some("call_1")
        );
        assert_eq!(
            request
                .body
                .pointer("/input/0/output")
                .and_then(Value::as_str),
            Some(r#"{"echo":"hi"}"#)
        );
        assert_eq!(
            request
                .body
                .pointer("/metadata/pi_wasm_follow_up")
                .and_then(Value::as_str),
            Some("tool_outputs")
        );
        assert!(
            !request
                .redacted_json_value()
                .to_string()
                .contains("secret-token")
        );
    }

    #[test]
    fn structured_send_request_builds_prompt_from_messages() {
        let request = BrowserSendRequest {
            provider: "openai".to_string(),
            model: Some("gpt-test".to_string()),
            system_prompt: Some("You are terse.".to_string()),
            messages: Some(vec![
                BrowserMessage {
                    role: Some("user".to_string()),
                    content: json!("hello"),
                },
                BrowserMessage {
                    role: Some("assistant".to_string()),
                    content: json!([{ "type": "output_text", "text": "hi" }]),
                },
                BrowserMessage {
                    role: Some("user".to_string()),
                    content: json!([{ "type": "input_text", "text": "next" }]),
                },
            ]),
            input: None,
            prompt: None,
            tools: None,
        };

        assert_eq!(
            request.to_prompt().expect("prompt"),
            "system: You are terse.\n\nuser: hello\n\nassistant: hi\n\nuser: next"
        );
    }

    #[test]
    fn structured_request_can_override_tool_payloads() {
        let mut client = PiBrowserClient::new();
        client.set_model("mock-model".to_string());
        let tools = vec![json!({
            "type": "function",
            "name": "hello_world",
            "description": "fake test tool"
        })];

        let request =
            client.provider_request_parts_with_tools("hello", "secret-token", Some(&tools));

        assert_eq!(
            request
                .body
                .pointer("/tools/0/name")
                .and_then(Value::as_str),
            Some("hello_world")
        );
        assert_eq!(
            request
                .body
                .pointer("/tools/0/type")
                .and_then(Value::as_str),
            Some("function")
        );
    }

    #[test]
    fn missing_auth_stays_missing_in_redacted_request_preview() {
        let client = PiBrowserClient::new();
        let request = client.provider_request_parts("hello", "");
        let redacted = request.redacted_json_value();

        assert!(request.authorization.is_empty());
        assert_eq!(
            redacted
                .pointer("/headers/Authorization")
                .and_then(Value::as_str),
            Some("<missing>")
        );
    }

    #[test]
    fn credential_status_normalizes_provider_and_never_exposes_secret() {
        let mut client = PiBrowserClient::new();
        client.set_auth("secret-token".to_string());
        client.set_direct_browser_credentials_allowed(true);

        let status = client.credential_status(" Codex ");
        let value = status.to_json_value();

        assert_eq!(status.provider, "codex");
        assert!(status.has_credential);
        assert!(status.direct_browser_credentials_allowed);
        assert_eq!(
            value.pointer("/storage").and_then(Value::as_str),
            Some("host-ui-owned")
        );
        assert!(!value.to_string().contains("secret-token"));
    }
}
