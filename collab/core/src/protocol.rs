//! Conservative Responses <-> Chat Completions mapping. Unsupported capabilities fail closed.
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Tool {
    pub name: String,
    pub namespace: Option<String>,
    pub custom: bool,
}
#[derive(Debug)]
pub struct ChatRequest {
    pub body: Value,
    pub tools: BTreeMap<String, Tool>,
}
fn content_text(v: &Value) -> Result<String> {
    if let Some(s) = v.as_str() {
        return Ok(s.into());
    }
    if v.is_null() {
        return Ok(String::new());
    }
    let parts = v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("unsupported message content"))?;
    let mut out = String::new();
    for p in parts {
        ensure!(
            matches!(p["type"].as_str(), Some("input_text" | "output_text")),
            "only text content is supported by the chat adapter"
        );
        out.push_str(
            p["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing text"))?,
        );
    }
    Ok(out)
}
fn add_tool(
    v: &Value,
    namespace: Option<&str>,
    list: &mut Vec<Value>,
    map: &mut BTreeMap<String, Tool>,
) -> Result<()> {
    let kind = v["type"].as_str().unwrap_or("");
    ensure!(
        matches!(kind, "function" | "custom"),
        "unsupported tool type: {kind}"
    );
    let name = v["name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    let wire = if let Some(ns) = namespace {
        format!("{ns}__{name}")
    } else {
        name.into()
    };
    ensure!(
        wire.len() <= 64 && !map.contains_key(&wire),
        "duplicate or overlong tool name"
    );
    map.insert(
        wire.clone(),
        Tool {
            name: name.into(),
            namespace: namespace.map(str::to_owned),
            custom: kind == "custom",
        },
    );
    let description = v["description"].as_str().unwrap_or("");
    let (description, parameters) = if kind == "custom" {
        (
            format!(
                "{description}\nPass the original tool input verbatim in the input field. Tool format: {}",
                v.get("format").unwrap_or(&Value::Null)
            ),
            json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false}),
        )
    } else {
        (
            description.into(),
            v.get("parameters")
                .cloned()
                .unwrap_or(json!({"type":"object","properties":{}})),
        )
    };
    let mut function = json!({"name":wire,"description":description,"parameters":parameters});
    if let Some(strict) = v.get("strict") {
        function["strict"] = strict.clone();
    }
    list.push(json!({"type":"function","function":function}));
    Ok(())
}
pub fn to_chat(request: &Value) -> Result<ChatRequest> {
    ensure!(request["stream"] == true, "chat adapter requires streaming");
    ensure!(
        request
            .get("previous_response_id")
            .is_none_or(Value::is_null),
        "incremental server-side history is unsupported; send full input"
    );
    ensure!(
        request.get("conversation").is_none_or(Value::is_null),
        "server-side conversations are unsupported"
    );
    let mut messages = Vec::new();
    let mut tools = Vec::new();
    let mut mapping = BTreeMap::new();
    if let Some(s) = request["instructions"].as_str() {
        messages.push(json!({"role":"system","content":s}));
    }
    for tool in request["tools"].as_array().into_iter().flatten() {
        if tool["type"] == "namespace" {
            let ns = tool["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("namespace name missing"))?;
            for t in tool["tools"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("namespace tools missing"))?
            {
                add_tool(t, Some(ns), &mut tools, &mut mapping)?;
            }
        } else {
            add_tool(tool, None, &mut tools, &mut mapping)?;
        }
    }
    let input = if let Some(text) = request["input"].as_str() {
        vec![json!({"role":"user","content":text})]
    } else {
        request["input"]
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("input must contain messages"))?
    };
    for item in input {
        match item["type"].as_str().unwrap_or("message") {
            "message"=>{let role=item["role"].as_str().ok_or_else(||anyhow::anyhow!("missing message role"))?;ensure!(matches!(role,"system"|"developer"|"user"|"assistant"),"unsupported message role");messages.push(json!({"role":if role=="developer"{"system"}else{role},"content":content_text(&item["content"])?}));}
            "function_call"|"custom_tool_call"=>{
                let name=item["name"].as_str().ok_or_else(||anyhow::anyhow!("tool name missing"))?;
                let wire=if let Some(ns)=item["namespace"].as_str(){format!("{ns}__{name}")}else{name.to_owned()};
                ensure!(mapping.contains_key(&wire),"history contains unavailable tool {wire}");
                let arguments=if item["type"]=="custom_tool_call"{json!({"input":item["input"]}).to_string()}else{item["arguments"].as_str().ok_or_else(||anyhow::anyhow!("tool arguments missing"))?.into()};
                let call=json!({"id":item["call_id"],"type":"function","function":{"name":wire,"arguments":arguments}});
                if let Some(last)=messages.last_mut().filter(|v|v["role"]=="assistant" && v.get("tool_calls").is_some()){last["tool_calls"].as_array_mut().unwrap().push(call);}else{messages.push(json!({"role":"assistant","content":null,"tool_calls":[call]}));}
            }
            "function_call_output"|"custom_tool_call_output"=>messages.push(json!({"role":"tool","tool_call_id":item["call_id"],"content":content_text(&item["output"])?})),
            // A provider's private reasoning cannot be replayed into another protocol.
            "reasoning"=>{},
            other=>bail!("unsupported history item: {other}"),
        }
    }
    let mut body = json!({"model":request["model"],"messages":messages,"stream":true,"stream_options":{"include_usage":true}});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(choice) = request.get("tool_choice") {
        if choice.is_string() {
            body["tool_choice"] = choice.clone();
        } else if choice["type"] == "function" {
            let name = choice["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing tool choice name"))?;
            let name = if let Some(ns) = choice["namespace"].as_str() {
                format!("{ns}__{name}")
            } else {
                name.into()
            };
            ensure!(mapping.contains_key(&name), "unknown tool choice");
            body["tool_choice"] = json!({"type":"function","function":{"name":name}});
        } else {
            bail!("unsupported tool_choice");
        }
    }
    for (from, to) in [
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("max_output_tokens", "max_completion_tokens"),
        ("parallel_tool_calls", "parallel_tool_calls"),
    ] {
        if let Some(v) = request.get(from) {
            body[to] = v.clone();
        }
    }
    if request
        .get("text")
        .and_then(|v| v.get("format"))
        .is_some_and(|v| v["type"] != "text")
    {
        bail!("structured output is not supported by this adapter");
    }
    Ok(ChatRequest {
        body,
        tools: mapping,
    })
}
#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}
pub struct ChatStream {
    pub response_id: String,
    tools: BTreeMap<String, Tool>,
    calls: BTreeMap<u64, Call>,
    text: String,
    message_started: bool,
    finished: Option<String>,
    usage: Value,
}
impl ChatStream {
    pub fn new(tools: BTreeMap<String, Tool>) -> Self {
        Self {
            response_id: format!("resp_{}", crate::model::id()),
            tools,
            calls: BTreeMap::new(),
            text: String::new(),
            message_started: false,
            finished: None,
            usage: Value::Null,
        }
    }
    pub fn created(&self) -> Value {
        json!({"type":"response.created","response":{"id":self.response_id}})
    }
    pub fn chunk(&mut self, v: &Value) -> Result<Vec<Value>> {
        ensure!(
            v.get("error").is_none(),
            "upstream returned a streaming error"
        );
        if let Some(usage) = v.get("usage").filter(|x| !x.is_null()) {
            self.usage = usage.clone();
        }
        let mut events = Vec::new();
        for choice in v["choices"].as_array().into_iter().flatten() {
            ensure!(
                choice["index"].as_u64().unwrap_or(0) == 0,
                "multiple choices unsupported"
            );
            if let Some(text) = choice["delta"]["content"].as_str() {
                if !self.message_started {
                    self.message_started = true;
                    events.push(json!({"type":"response.output_item.added","output_index":0,"item":{"id":format!("{}_msg",self.response_id),"type":"message","role":"assistant","content":[]}}));
                }
                ensure!(
                    self.text.len() + text.len() <= 4_000_000,
                    "model output too large"
                );
                self.text.push_str(text);
                events.push(json!({"type":"response.output_text.delta","item_id":format!("{}_msg",self.response_id),"output_index":0,"content_index":0,"delta":text}));
            }
            ensure!(
                choice["delta"].get("refusal").is_none_or(|x| x.is_null()),
                "model refused request"
            );
            for call in choice["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let index = call["index"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("tool call missing index"))?;
                ensure!(index < 128, "too many tool calls");
                let entry = self.calls.entry(index).or_default();
                for (value, dest) in [
                    (&call["id"], &mut entry.id),
                    (&call["function"]["name"], &mut entry.name),
                    (&call["function"]["arguments"], &mut entry.args),
                ] {
                    if let Some(s) = value.as_str() {
                        ensure!(dest.len() + s.len() <= 1_000_000, "tool output too large");
                        dest.push_str(s);
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                ensure!(
                    matches!(reason, "stop" | "tool_calls"),
                    "upstream did not complete: {reason}"
                );
                self.finished = Some(reason.into());
            }
        }
        Ok(events)
    }
    pub fn complete(self) -> Result<Vec<Value>> {
        ensure!(
            self.finished.is_some(),
            "upstream stream ended before finish_reason"
        );
        ensure!(
            !self.text.is_empty() || !self.calls.is_empty(),
            "empty upstream response"
        );
        let mut events = Vec::new();
        let mut output = Vec::new();
        if self.message_started {
            let item = json!({"id":format!("{}_msg",self.response_id),"type":"message","role":"assistant","content":[{"type":"output_text","text":self.text}]});
            events.push(json!({"type":"response.output_item.done","output_index":0,"item":item}));
            output.push(item);
        }
        for (index, call) in self.calls {
            ensure!(!call.id.is_empty(), "missing tool call id");
            let tool = self
                .tools
                .get(&call.name)
                .ok_or_else(|| anyhow::anyhow!("model returned unknown tool"))?;
            let args: Value = serde_json::from_str(&call.args)
                .map_err(|_| anyhow::anyhow!("model returned invalid tool arguments"))?;
            let mut item = if tool.custom {
                json!({"type":"custom_tool_call","id":format!("{}_fc_{index}",self.response_id),"call_id":call.id,"name":tool.name,"input":args["input"].as_str().ok_or_else(||anyhow::anyhow!("custom tool requires string input"))?})
            } else {
                json!({"type":"function_call","id":format!("{}_fc_{index}",self.response_id),"call_id":call.id,"name":tool.name,"arguments":call.args})
            };
            if let Some(ns) = &tool.namespace {
                item["namespace"] = json!(ns);
            }
            events.push(json!({"type":"response.output_item.added","output_index":output.len(),"item":item}));
            events.push(
                json!({"type":"response.output_item.done","output_index":output.len(),"item":item}),
            );
            output.push(item);
        }
        let input = self.usage["prompt_tokens"].as_i64().unwrap_or(0);
        let out = self.usage["completion_tokens"].as_i64().unwrap_or(0);
        events.push(json!({"type":"response.completed","response":{"id":self.response_id,"output":output,"usage":{"input_tokens":input,"output_tokens":out,"total_tokens":input+out}}}));
        Ok(events)
    }
}
/// SSE frames can span TCP chunks and contain multiple data lines; metadata is ignored.
#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.buf.extend_from_slice(bytes);
        ensure!(self.buf.len() <= 2_000_000, "SSE frame too large");
        let mut frames = Vec::new();
        loop {
            let boundary = [
                self.buf
                    .windows(2)
                    .position(|w| w == b"\n\n")
                    .map(|p| (p, 2)),
                self.buf
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| (p, 4)),
            ]
            .into_iter()
            .flatten()
            .min_by_key(|(p, _)| *p);
            let Some((pos, len)) = boundary else { break };
            let chunk = self.buf.drain(..pos + len).collect::<Vec<_>>();
            let text = std::str::from_utf8(&chunk)?;
            let data = text
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|s| s.strip_prefix(' ').unwrap_or(s))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                frames.push(data);
            }
        }
        Ok(frames)
    }
    pub fn is_empty(&self) -> bool {
        self.buf.iter().all(u8::is_ascii_whitespace)
    }
}
pub fn sse(event: &Value) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        event["type"].as_str().unwrap_or("message"),
        event
    )
}
