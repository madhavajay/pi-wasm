use js_sys::Function;
use pi_browser::{PiBrowserClient, PiClient, read_object_path};
use serde_json::json;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn config_value_is_a_plain_js_object() {
    let mut client = PiBrowserClient::new();
    client.set_auth("browser-test-token".to_string());
    client.set_direct_browser_credentials_allowed(true);

    let value = client.config_value().expect("config value");

    assert_eq!(
        read_object_path(&value, "hasAuth".to_string()).as_bool(),
        Some(true)
    );
    assert_eq!(
        read_object_path(&value, "directBrowserCredentialsAllowed".to_string()).as_bool(),
        Some(true)
    );
    assert_eq!(
        read_object_path(&value, "model".to_string()).as_string(),
        Some("gpt-5.5".to_string())
    );
}

#[wasm_bindgen_test]
fn from_config_applies_initial_client_settings() {
    let config = serde_wasm_bindgen::to_value(&json!({
        "endpoint": "https://example.test/v1/responses",
        "model": "gpt-test",
        "directBrowserCredentialsAllowed": true
    }))
    .expect("config");

    let client = PiBrowserClient::from_config(config).expect("client");
    let value = client.config_value().expect("config value");

    assert_eq!(
        read_object_path(&value, "endpoint".to_string()).as_string(),
        Some("https://example.test/v1/responses".to_string())
    );
    assert_eq!(
        read_object_path(&value, "model".to_string()).as_string(),
        Some("gpt-test".to_string())
    );
    assert_eq!(
        read_object_path(&value, "directBrowserCredentialsAllowed".to_string()).as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test]
fn pi_client_constructor_applies_initial_client_settings() {
    let config = serde_wasm_bindgen::to_value(&json!({
        "endpoint": "https://example.test/v1/responses",
        "model": "gpt-test",
        "directBrowserCredentialsAllowed": true
    }))
    .expect("config");

    let client = PiClient::new(config).expect("client");
    let value = client.config_value().expect("config value");

    assert_eq!(
        read_object_path(&value, "endpoint".to_string()).as_string(),
        Some("https://example.test/v1/responses".to_string())
    );
    assert_eq!(
        read_object_path(&value, "model".to_string()).as_string(),
        Some("gpt-test".to_string())
    );
    assert_eq!(
        read_object_path(&value, "directBrowserCredentialsAllowed".to_string()).as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test]
fn pi_client_exposes_final_send_surface() {
    let mut client = PiClient::new(JsValue::UNDEFINED).expect("client");
    client.set_auth("browser-secret-token".to_string());
    let request = serde_wasm_bindgen::to_value(&json!({
        "provider": "openai",
        "model": "gpt-4.1-mini",
        "messages": [
            { "role": "user", "content": "hello" }
        ]
    }))
    .expect("request");

    let draft = client.draft_request(request).expect("draft");

    assert!(draft.contains("\"model\":\"gpt-4.1-mini\""));
    assert!(draft.contains("user: hello"));
    assert!(draft.contains("Bearer <redacted>"));
    assert!(!draft.contains("browser-secret-token"));
}

#[wasm_bindgen_test]
fn credential_methods_reject_unsupported_provider() {
    let mut client = PiBrowserClient::new();

    assert!(
        client
            .set_credential("anthropic".to_string(), "token".to_string())
            .is_err()
    );
    assert!(client.clear_credential("anthropic".to_string()).is_err());
}

#[wasm_bindgen_test]
fn draft_request_redacts_credentials_across_boundary() {
    let mut client = PiBrowserClient::new();
    client.set_auth("browser-secret-token".to_string());

    let draft = client.draft_request_json("hello from wasm test".to_string());

    assert!(draft.contains("Bearer <redacted>"));
    assert!(draft.contains("\"stream\":true"));
    assert!(draft.contains("\"tools\""));
    assert!(!draft.contains("browser-secret-token"));
}

#[wasm_bindgen_test]
fn structured_draft_request_accepts_messages_and_model() {
    let mut client = PiBrowserClient::new();
    client.set_auth("browser-secret-token".to_string());
    let request = serde_wasm_bindgen::to_value(&json!({
        "provider": "openai",
        "model": "gpt-4.1-mini",
        "systemPrompt": "Be concise.",
        "messages": [
            { "role": "user", "content": "hello" }
        ],
        "tools": [
            { "type": "function", "name": "hello_world", "description": "test tool" }
        ]
    }))
    .expect("request");

    let draft = client.draft_request(request).expect("draft");

    assert!(draft.contains("\"model\":\"gpt-4.1-mini\""));
    assert!(draft.contains("system: Be concise."));
    assert!(draft.contains("user: hello"));
    assert!(draft.contains("hello_world"));
    assert!(draft.contains("Bearer <redacted>"));
    assert!(!draft.contains("browser-secret-token"));
}

#[wasm_bindgen_test]
fn sync_mock_emits_plain_js_event_objects() {
    let mut client = PiBrowserClient::new();
    let events = js_sys::Array::new();
    let callback = Function::new_with_args("event", "this.push(event);").bind0(&events);

    client
        .send_mock("echo from wasm test".to_string(), &callback)
        .expect("mock send");

    assert!(events.length() >= 4);
    let first = events.get(0);
    assert_eq!(
        read_object_path(&first, "kind".to_string()).as_string(),
        Some("status".to_string())
    );
    let tool_event = events.get(1);
    assert_eq!(
        read_object_path(&tool_event, "kind".to_string()).as_string(),
        Some("toolCall".to_string())
    );
    assert_eq!(
        read_object_path(&tool_event, "data.arguments.text".to_string()).as_string(),
        Some("echo from wasm test".to_string())
    );
}

#[wasm_bindgen_test]
fn read_object_path_handles_missing_paths() {
    assert!(read_object_path(&JsValue::NULL, "missing.path".to_string()).is_undefined());
}
