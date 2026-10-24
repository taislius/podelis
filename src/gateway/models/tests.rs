use std::time::Duration;

use serde_json::json;

use super::sse::{Event, Parser};
use super::*;

fn config(api: Api, model: &str, url: Option<&str>) -> ModelConfig {
    ModelConfig {
        name: "m".into(),
        api,
        model: model.into(),
        url: url.map(str::to_string),
        key_command: None,
        effort: None,
        max_tokens: None,
        tools: None,
    }
}

fn prompt(text: &str) -> Prompt {
    Prompt {
        system: Some("Be brief.".into()),
        messages: vec![Message::new("user", text)],
        max_tokens: None,
        tools: Vec::new(),
        no_more_calls: false,
    }
}

#[test]
fn events_are_cut_at_blank_lines_whatever_the_chunks() {
    let stream = "event: a\ndata: {\"x\":1}\n\n: keep-alive\n\ndata: one\r\ndata: two\r\n\r\nevent: b\ndata:";
    let mut parser = Parser::default();
    let mut events = Vec::new();
    // A byte at a time: chunks split anywhere.
    for b in stream.as_bytes() {
        events.extend(parser.push(std::slice::from_ref(b)));
    }
    assert_eq!(
        events,
        [Event { name: "a".into(), data: "{\"x\":1}".into() }, Event { name: String::new(), data: "one\ntwo".into() },]
    );
    // The unfinished one comes with its blank line.
    assert_eq!(parser.push(b" end\n\n"), [Event { name: "b".into(), data: "end".into() }]);
}

#[test]
fn an_anthropic_request_sets_effort_and_fallbacks_where_they_apply() {
    let opus = config(Api::Anthropic, "claude-opus-5-5", None);
    let body = anthropic::body(&opus, &prompt("hi"));
    assert_eq!(body["model"], json!("claude-opus-5-5"));
    assert_eq!(body["max_tokens"], json!(DEFAULT_MAX_TOKENS));
    assert_eq!((body["stream"].clone(), body["system"].clone()), (json!(true), json!("Be brief.")));
    assert_eq!(body["messages"], json!([{ "role": "user", "content": "hi" }]));
    assert_eq!(body["output_config"], json!({ "effort": "high" }));
    assert_eq!(body["fallbacks"], json!("default"));
    // Not through another address, nor for other models; effort "" sends none.
    let proxied = config(Api::Anthropic, "claude-opus-5-5", Some("https://proxy.example"));
    assert!(anthropic::body(&proxied, &prompt("hi")).get("fallbacks").is_none());
    let mut old = config(Api::Anthropic, "claude-haiku-4-5", None);
    old.effort = Some(String::new());
    let body = anthropic::body(&old, &prompt("hi"));
    assert!(body.get("fallbacks").is_none() && body.get("output_config").is_none());
}

#[test]
fn an_anthropic_stream_becomes_the_answer() {
    let events = [
        (
            "message_start",
            json!({"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":12}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"O"}}),
        ),
        ("ping", json!({"type":"ping"})),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"K"}}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    let mut answer = anthropic::Answer::default();
    let mut pieces = Vec::new();
    let mut done = false;
    for (name, data) in events {
        let event = Event { name: name.into(), data: data.to_string() };
        done = answer.take(&event, &mut |t: &str| pieces.push(t.to_string())).unwrap();
    }
    assert!(done);
    assert_eq!(pieces, ["O", "K"]);
    let reply = answer.reply;
    assert_eq!(
        (reply.text.as_str(), reply.model.as_str(), reply.stop_reason.as_str()),
        ("OK", "claude-opus-5-5", "end_turn")
    );
    assert_eq!((reply.input_tokens, reply.output_tokens), (12, 7));
    // An overloaded stream can be tried again.
    let error = Event {
        name: "error".into(),
        data: json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}).to_string(),
    };
    let e = anthropic::Answer::default().take(&error, &mut |_: &str| {}).unwrap_err();
    assert!(e.retry && e.message.contains("Overloaded"));
}

#[test]
fn an_openai_stream_becomes_the_answer() {
    let lines = [
        json!({"model":"qwen3:8b","choices":[{"delta":{"role":"assistant","content":"O"}}]}).to_string(),
        json!({"model":"qwen3:8b","choices":[{"delta":{"content":"K"},"finish_reason":"stop"}]}).to_string(),
        json!({"model":"qwen3:8b","choices":[],"usage":{"prompt_tokens":20,"completion_tokens":2}}).to_string(),
        "[DONE]".to_string(),
    ];
    let mut answer = openai::Answer::default();
    let mut done = false;
    for data in lines {
        done = answer.take(&Event { name: String::new(), data }, &mut |_: &str| {}).unwrap();
    }
    assert!(done);
    let r = answer.reply;
    assert_eq!((r.text.as_str(), r.stop_reason.as_str(), r.input_tokens, r.output_tokens), ("OK", "stop", 20, 2));
    let body = openai::body(&config(Api::OpenAi, "qwen3:8b", None), &prompt("hi"));
    assert_eq!(body["messages"][0], json!({ "role": "system", "content": "Be brief." }));
    assert!(body.get("max_tokens").is_none());
}

#[test]
fn tool_calls_go_out_and_come_back_in_both_formats() {
    let tool = Tool { name: "describe_table", description: "d", input: json!({"type":"object"}) };
    let call = ToolCall { id: "c1".into(), name: "describe_table".into(), input: json!({"name": "t"}) };
    let mut p = prompt("hi");
    p.tools = vec![tool];
    p.messages.push(Message { calls: vec![call], ..Message::new("assistant", "") });
    p.messages.push(Message::tool_answer("c1", "t (id)"));
    // OpenAI's: a function, the call's arguments as a string, a `tool` turn.
    let body = openai::body(&config(Api::OpenAi, "gpt-5", None), &p);
    assert_eq!(body["tools"][0]["function"]["name"], json!("describe_table"));
    assert_eq!(body["messages"][2]["tool_calls"][0]["function"]["arguments"], json!("{\"name\":\"t\"}"));
    assert_eq!(body["messages"][3], json!({ "role": "tool", "tool_call_id": "c1", "content": "t (id)" }));
    assert!(body.get("tool_choice").is_none());
    // Anthropic's: `tool_use` and `tool_result` blocks; no more calls: none.
    p.no_more_calls = true;
    let body = anthropic::body(&config(Api::Anthropic, "claude-opus-5-5", None), &p);
    assert_eq!(body["messages"][1]["content"][0]["type"], json!("tool_use"));
    assert_eq!(
        body["messages"][2]["content"][0],
        json!({ "type": "tool_result", "tool_use_id": "c1", "content": "t (id)" })
    );
    assert_eq!(body["tool_choice"], json!({ "type": "none" }));
    // A streamed OpenAI call, in pieces.
    let lines = [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_9","function":{"name":"find_tables","arguments":"{\"wo"}}]}}]}).to_string(),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"rds\": \"pay\"}"}}]},"finish_reason":"tool_calls"}]}).to_string(),
        "[DONE]".to_string(),
    ];
    let mut answer = openai::Answer::default();
    for data in lines {
        answer.take(&Event { name: String::new(), data }, &mut |_: &str| {}).unwrap();
    }
    answer.finish_calls();
    assert_eq!(
        answer.reply.calls,
        [ToolCall { id: "call_9".into(), name: "find_tables".into(), input: json!({"words": "pay"}) }]
    );
}

#[test]
fn a_model_on_this_machine_looks_up_nothing_unless_set_to() {
    let mut local = config(Api::OpenAi, "qwen3:8b", Some("http://localhost:11434/v1"));
    assert!(!local.uses_tools());
    local.tools = Some(true);
    assert!(local.uses_tools());
    assert!(config(Api::Anthropic, "claude-opus-5-5", None).uses_tools());
}

#[test]
fn errors_say_what_happened_and_whether_to_try_again() {
    let body = json!({"type":"error","error":{"type":"not_found_error","message":"model: claude-nope"}}).to_string();
    let e = http_error(reqwest::StatusCode::NOT_FOUND, None, &body);
    assert!(!e.retry && e.message.contains("is the model's name right") && e.message.contains("claude-nope"));
    let e = http_error(reqwest::StatusCode::from_u16(529).unwrap(), Some(Duration::from_secs(3)), "busy");
    assert!(e.retry && e.retry_after == Some(Duration::from_secs(3)) && e.message.contains("busy"));
    assert!(!http_error(reqwest::StatusCode::UNAUTHORIZED, None, "{}").retry);
}

#[test]
fn keys_travel_only_encrypted_or_to_this_machine() {
    assert!(checked_url(&config(Api::OpenAi, "x", Some("http://localhost:11434/v1"))).is_ok());
    assert!(checked_url(&config(Api::OpenAi, "x", Some("http://[::1]:8080/v1"))).is_ok());
    let e = checked_url(&config(Api::OpenAi, "x", Some("http://gpu-box:8080/v1"))).unwrap_err();
    assert!(e.message.contains("must use https"), "{e}");
    assert!(checked_url(&config(Api::Anthropic, "x", None)).is_ok());
}

#[test]
fn the_default_model_is_the_first_and_names_are_checked() {
    let mut a = config(Api::Anthropic, "claude-opus-5-5", None);
    a.name = "claude".into();
    let mut b = config(Api::OpenAi, "qwen3:8b", Some("http://localhost:11434/v1"));
    b.name = "local".into();
    let models = [a, b];
    assert_eq!(pick(&models, None).unwrap().name, "claude");
    assert_eq!(pick(&models, Some("local")).unwrap().name, "local");
    let e = pick(&models, Some("gpt")).unwrap_err();
    assert!(e.message.contains("claude, local"), "{e}");
    assert!(pick(&[], None).unwrap_err().message.contains("[[models]]"));
}

#[test]
fn a_key_comes_from_the_keychain_then_its_command() {
    let mut m = config(Api::Anthropic, "claude-opus-5-5", Some("https://proxy.example"));
    m.name = "keyed-model-test".into();
    // A url of its own: ANTHROPIC_API_KEY doesn't count for it.
    assert_eq!(key(&m).unwrap(), (None, KeySource::Missing));
    m.key_command = Some("printf 'from-command\\nignored'".into());
    assert_eq!(key(&m).unwrap(), (Some("from-command".to_string()), KeySource::Command));
    crate::config::store::set_password(&ModelConfig::keychain_account(&m.name), "from-keychain").unwrap();
    assert_eq!(key(&m).unwrap(), (Some("from-keychain".to_string()), KeySource::Keychain));
    crate::config::store::delete_password(&ModelConfig::keychain_account(&m.name)).unwrap();
    let local = config(Api::OpenAi, "qwen3:8b", Some("http://127.0.0.1:11434/v1"));
    assert_eq!(key(&local).unwrap().1, KeySource::Local);
}

#[test]
fn settings_read_models_from_config_toml() {
    let dir = tempfile::tempdir().unwrap();
    let toml = "[[models]]\nname = \"claude\"\napi = \"anthropic\"\nmodel = \"claude-opus-5-5\"\n\n\
                [[models]]\nname = \"local\"\napi = \"openai\"\nurl = \"http://localhost:11434/v1\"\nmodel = \"qwen3:8b\"\n";
    std::fs::write(dir.path().join("config.toml"), toml).unwrap();
    let (settings, warning) = crate::config::settings::Settings::load(dir.path());
    assert!(warning.is_none(), "{warning:?}");
    let names: Vec<&str> = settings.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["claude", "local"]);
    assert_eq!(settings.models[0].url(), "https://api.anthropic.com");
}

#[test]
fn a_huge_retry_after_is_clamped_not_a_panic() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", "1e20".parse().unwrap());
    assert_eq!(retry_after(&headers), Some(Duration::from_secs(60)));
    headers.insert("retry-after", "2.5".parse().unwrap());
    assert_eq!(retry_after(&headers), Some(Duration::from_millis(2500)));
}

#[test]
fn effort_goes_to_the_models_that_take_it() {
    for (model, sent) in [
        ("claude-opus-5-5", true),
        ("claude-sonnet-4-6", true),
        ("claude-haiku-4-5", false),
        ("claude-sonnet-4-5", false),
    ] {
        let body = anthropic::body(&config(Api::Anthropic, model, None), &prompt("hi"));
        assert_eq!(body.get("output_config").is_some(), sent, "{model}");
    }
    // Set in config.toml: sent whatever the model.
    let mut old = config(Api::Anthropic, "claude-haiku-4-5", None);
    old.effort = Some("low".into());
    assert_eq!(anthropic::body(&old, &prompt("hi"))["output_config"], json!({ "effort": "low" }));
}

#[test]
fn a_last_event_without_its_blank_line_is_kept() {
    let mut parser = Parser::default();
    assert!(parser.push(b"event: message_stop\ndata: {}").is_empty());
    assert_eq!(parser.finish(), Some(Event { name: "message_stop".into(), data: "{}".into() }));
    assert_eq!(Parser::default().finish(), None);
}

#[tokio::test]
async fn a_request_that_fails_never_says_its_url_or_key() {
    // Nothing listens there: the request fails before any answer.
    // Port 1: nothing listens there, and nobody can bind it without root
    // (a port freed for the test could be taken by another test meanwhile).
    let port = 1;
    let url = format!("http://127.0.0.1:{port}/v1/chat?key=SECRET");
    let e = reqwest::Client::new().get(&url).send().await.unwrap_err();
    for said in [super::unreachable(&e), super::cause(&e)] {
        assert!(!said.contains("SECRET") && !said.contains("/v1/chat"), "{said}");
    }
    assert!(super::unreachable(&e).contains(&format!("http://127.0.0.1:{port}")));
}
