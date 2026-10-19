use codex_collab_core::{
    protocol::*,
    secrets::{Secret, redact},
};
use serde_json::json;
fn request() -> serde_json::Value {
    json!({"model":"model","stream":true,"instructions":"system","input":[{"role":"developer","content":[{"type":"input_text","text":"shared"}]},{"role":"user","content":"task"}],"tools":[{"type":"function","name":"lookup","parameters":{"type":"object"}}]})
}
#[test]
fn converts_roles_tools_and_full_tool_history() {
    let mut r = request();
    r["input"].as_array_mut().unwrap().extend([
        json!({"type":"function_call","name":"lookup","call_id":"c1","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"c1","output":"result"}),
    ]);
    let c = to_chat(&r).unwrap();
    assert_eq!(c.body["messages"][1]["role"], "system");
    assert_eq!(
        c.body["messages"][3]["tool_calls"][0]["function"]["name"],
        "lookup"
    );
    assert_eq!(c.body["messages"][4]["role"], "tool");
}
#[test]
fn assembles_fragmented_calls_text_and_usage() {
    let c = to_chat(&request()).unwrap();
    let mut s = ChatStream::new(c.tools);
    assert_eq!(s.created()["type"], "response.created");
    let text = s
        .chunk(&json!({"choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}))
        .unwrap();
    assert!(text.iter().any(|x| x["delta"] == "hello"));
    s.chunk(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call1","function":{"name":"lookup","arguments":"{\"a\":"}}]}}]})).unwrap();
    s.chunk(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    s.chunk(&json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":10}}))
        .unwrap();
    let out = s.complete().unwrap();
    assert_eq!(out.last().unwrap()["response"]["usage"]["total_tokens"], 30);
    assert!(out.iter().any(|x| x["item"]["arguments"] == "{\"a\":1}"));
}
#[test]
fn namespaced_custom_tool_roundtrip() {
    let mut r = request();
    r["tools"] = json!([{"type":"namespace","name":"functions","tools":[{"type":"custom","name":"apply_patch","description":"patch","format":{"type":"grammar","syntax":"lark","definition":"grammar"}}]}]);
    let c = to_chat(&r).unwrap();
    assert_eq!(
        c.body["tools"][0]["function"]["name"],
        "functions__apply_patch"
    );
    let mut s = ChatStream::new(c.tools);
    s.chunk(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"functions__apply_patch","arguments":"{\"input\":\"patch content\"}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    let events = s.complete().unwrap();
    assert_eq!(events[0]["item"]["type"], "custom_tool_call");
    assert_eq!(events[0]["item"]["namespace"], "functions");
    assert_eq!(events[0]["item"]["input"], "patch content");
}
#[test]
fn rejects_incomplete_and_unknown_output() {
    let c = to_chat(&request()).unwrap();
    assert!(ChatStream::new(c.tools).complete().is_err());
    let mut s = ChatStream::new(to_chat(&request()).unwrap().tools);
    assert!(
        s.chunk(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}))
            .is_err()
    );
}
#[test]
fn rejects_unsupported_capabilities() {
    let mut r = request();
    r["tools"] = json!([{"type":"web_search"}]);
    assert!(to_chat(&r).is_err());
    let mut r = request();
    r["previous_response_id"] = json!("previous");
    assert!(to_chat(&r).is_err());
    let mut r = request();
    r["input"] = json!([{"role":"user","content":[{"type":"input_image","image_url":"image"}]}]);
    assert!(to_chat(&r).is_err());
}
#[test]
fn sse_decoder_handles_fragmentation_crlf_and_utf8() {
    let mut d = SseDecoder::default();
    let bytes = "event: x\r\ndata: {\"text\":\"中文\"}\r\n\r\n".as_bytes();
    let mut frames = vec![];
    for b in bytes {
        frames.extend(d.push(&[*b]).unwrap());
    }
    assert_eq!(frames, vec!["{\"text\":\"中文\"}"]);
    assert!(d.is_empty());
    let frames = d.push(b":comment\n\ndata: a\ndata: b\n\n").unwrap();
    assert_eq!(frames, vec!["a\nb"]);
}
#[test]
fn secret_never_serializes_and_redaction_covers_repeats() {
    assert!(Secret::new("bad\nkey".into()).is_err());
    assert_eq!(redact("key key", &["key"]), "[REDACTED] [REDACTED]");
}
#[test]
fn sse_mixed_line_endings_keep_frame_order() {
    let mut d = SseDecoder::default();
    assert_eq!(
        d.push(b"data: one\r\n\r\ndata: two\n\n").unwrap(),
        vec!["one", "two"]
    );
}
#[test]
fn stream_redaction_handles_keys_split_at_every_boundary() {
    let key = "private-api-key";
    for split in 1..key.len() {
        let mut r = codex_collab_core::secrets::StreamRedactor::new(vec![key.into()]);
        let mut out = r.push(&format!("中文 {}", &key[..split]));
        out += &r.push(&format!("{} 完成", &key[split..]));
        out += &r.finish();
        assert_eq!(out, "中文 [REDACTED] 完成");
    }
}
