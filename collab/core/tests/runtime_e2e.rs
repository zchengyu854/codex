//! Explicit opt-in integration test: runs the installed Codex against local mock APIs.
use axum::{Router, extract::State, http::HeaderMap, response::IntoResponse, routing::post};
use codex_collab_core::{
    model::*,
    protocol::sse,
    runtime::Runtime,
    secrets::{MemorySecrets, Secret, Secrets},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tokio::process::Command;
struct Mock {
    store: Arc<Store>,
    session: String,
    workers: Vec<String>,
    calls: Mutex<HashMap<String, usize>>,
    requests: Mutex<Vec<Value>>,
}
fn find_tool(body: &Value, target: &str) -> (String, Option<String>) {
    for t in body["tools"].as_array().into_iter().flatten() {
        if t["type"] == "namespace" {
            for f in t["tools"].as_array().into_iter().flatten() {
                if f["name"].as_str().is_some_and(|n| n.ends_with(target)) {
                    return (
                        f["name"].as_str().unwrap().into(),
                        t["name"].as_str().map(str::to_owned),
                    );
                }
            }
        }
        let name = t["function"]["name"]
            .as_str()
            .or_else(|| t["name"].as_str())
            .unwrap_or("");
        if name.ends_with(target) {
            return (name.into(), None);
        }
    }
    panic!("tool {target} not found: {}", body["tools"]);
}
fn call(body: &Value, name: &str, args: Value, index: usize) -> Value {
    let (name, ns) = find_tool(body, name);
    let mut item = json!({"type":"function_call","id":format!("fc_{index}"),"call_id":format!("call_{}_{index}",id()),"name":name,"arguments":args.to_string()});
    if let Some(ns) = ns {
        item["namespace"] = json!(ns);
    }
    item
}
async fn mock(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> impl IntoResponse {
    let model = body["model"].as_str().unwrap().to_owned();
    assert_eq!(headers["authorization"], format!("Bearer key-{model}"));
    m.requests.lock().unwrap().push(body.clone());
    let step = {
        let mut calls = m.calls.lock().unwrap();
        let step = *calls.entry(model.clone()).or_default();
        calls.insert(model.clone(), step + 1);
        step
    };
    let chat = body.get("messages").is_some();
    let mut items = vec![];
    if model == "mock-main" {
        let tasks = m
            .store
            .tasks(&m.session)
            .unwrap()
            .into_iter()
            .filter(|t| m.workers.contains(&t.request.agent_id))
            .collect::<Vec<_>>();
        match step {
            0=>for(i,worker)in m.workers.iter().enumerate(){items.push(call(&body,"collab_create_task",json!({"agentId":worker,"goal":format!("Create worker{i}.txt containing worker{i} using the shell tool. Do not commit."),"acceptance":"file exists with requested content","coding":true,"dependencies":[]}),i));},
            1=>for(i,t)in tasks.iter().enumerate(){items.push(call(&body,"collab_wait_task",json!({"taskId":t.id}),i));},
            2=>for(i,t)in tasks.iter().enumerate(){items.push(call(&body,"collab_review_diff",json!({"taskId":t.id}),i));},
            3=>for(i,t)in tasks.iter().enumerate(){assert_eq!(t.status,"completed","worker did not complete: {t:?}");items.push(call(&body,"collab_integrate",json!({"taskId":t.id,"expectedCommit":t.commit,"review":"Inspected requested single-file diff. Run configured verification before integration."}),i));},
            _=>items.push(json!({"id":"msg_main","type":"message","role":"assistant","content":[{"type":"output_text","text":"All three worker results reviewed, tested and integrated."}]})),
        }
    } else if step == 0 {
        let index = model.trim_start_matches("mock-worker");
        let text = format!("printf 'worker{index}' > worker{index}.txt");
        let names = body["tools"].to_string();
        let (name, args) = if names.contains("exec_command") {
            ("exec_command", json!({"cmd":text,"yield_time_ms":1000}))
        } else if names.contains("shell_command") {
            ("shell_command", json!({"command":text}))
        } else {
            ("shell", json!({"command":["/bin/sh","-c",text]}))
        };
        items.push(call(&body, name, args, 0));
    } else {
        items.push(json!({"id":"msg_worker","type":"message","role":"assistant","content":[{"type":"output_text","text":"Created and checked assigned file."}]}));
    }
    let data = if chat {
        let mut choices = vec![];
        let mut calls = vec![];
        for (i, item) in items.iter().enumerate() {
            if item["type"] == "function_call" {
                calls.push(json!({"index":i,"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":item["arguments"]}}));
            } else {
                choices.push(json!({"choices":[{"index":0,"delta":{"content":item["content"][0]["text"]},"finish_reason":"stop"}]}));
            }
        }
        if !calls.is_empty() {
            choices.push(json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]}));
        }
        choices.push(json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20}}));
        choices
            .into_iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n"
    } else {
        let rid = id();
        let mut events = vec![json!({"type":"response.created","response":{"id":rid}})];
        for (i, item) in items.iter().enumerate() {
            events.push(json!({"type":"response.output_item.added","output_index":i,"item":item}));
            if item["type"] == "message" {
                events.push(
                    json!({"type":"response.output_text.delta","delta":item["content"][0]["text"]}),
                );
            }
            events.push(json!({"type":"response.output_item.done","output_index":i,"item":item}));
        }
        events.push(json!({"type":"response.completed","response":{"id":rid,"output":items,"usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120}}}));
        events.iter().map(sse).collect()
    };
    ([("content-type", "text/event-stream")], data)
}
async fn git(path: &std::path::Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .await
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires installed Codex and local process execution; set CODEX_COLLAB_BIN to test a source-built executable"]
async fn coordinator_delegates_three_workers_across_both_protocols_and_integrates() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init"]).await;
    std::fs::write(repo.path().join("README.md"), "fixture").unwrap();
    git(repo.path(), &["add", "."]).await;
    git(
        repo.path(),
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "-m",
            "init",
        ],
    )
    .await;
    let root = TempDir::new().unwrap();
    let store = Arc::new(Store::open(root.path()).unwrap());
    let secrets = Arc::new(MemorySecrets::default());
    let rt = Runtime::new(
        store.clone(),
        secrets.clone(),
        PathBuf::from(std::env::var("CODEX_COLLAB_BIN").unwrap_or("codex".into())),
    );
    let session=rt.dispatch("session/create",json!({"repository":repo.path(),"validation":[["/usr/bin/python3","-c","from pathlib import Path\nfiles=list(Path('.').glob('worker*.txt'))\nassert files\nassert all(p.read_text()==p.stem for p in files)\nassert Path('README.md').read_text()=='fixture'"]],"requirement":"Keep original checkout unchanged; create one file per worker."})).await.unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    let main = id();
    let workers = (0..3).map(|_| id()).collect::<Vec<_>>();
    let mock_state = Arc::new(Mock {
        store: store.clone(),
        session: sid.clone(),
        workers: workers.clone(),
        calls: Mutex::new(HashMap::new()),
        requests: Mutex::new(vec![]),
    });
    // Distinct endpoints exercise independent provider connections.
    let mut servers = vec![];
    for (i, agent_id) in std::iter::once(&main).chain(workers.iter()).enumerate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/responses", post(mock))
            .route("/v1/chat/completions", post(mock))
            .with_state(mock_state.clone());
        servers.push(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        let model = if i == 0 {
            "mock-main".into()
        } else {
            format!("mock-worker{}", i - 1)
        };
        store
            .configure(&Agent {
                id: agent_id.clone(),
                session_id: sid.clone(),
                role: if i == 0 {
                    Role::Coordinator
                } else {
                    Role::Worker
                },
                model: model.clone(),
                base_url: base,
                protocol: if i >= 2 {
                    Protocol::ChatCompletions
                } else {
                    Protocol::Responses
                },
                context_bytes: 512_000,
            })
            .unwrap();
        secrets
            .set(agent_id, Secret::new(format!("key-{model}")).unwrap())
            .unwrap();
    }
    let task=store.create_task(TaskRequest{session_id:sid.clone(),agent_id:main,goal:"Delegate three independent coding tasks, review their diffs and integrate after validation.".into(),acceptance:"all three worker files integrated".into(),dependencies:vec![],coding:false}).unwrap();
    let mut updates = rt.updates.subscribe();
    let scheduler = rt.start();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            let t = store.task(&task.id).unwrap();
            if !matches!(t.status.as_str(), "queued" | "running") {
                break t;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    rt.stop().await;
    scheduler.await.unwrap();
    for s in servers {
        s.abort();
    }
    let mut saw_delta = false;
    while let Ok(v) = updates.try_recv() {
        saw_delta |= v["kind"] == "agent_delta";
    }
    assert!(saw_delta, "host must expose live public text deltas");
    let completed = completed.expect("end-to-end test timed out");
    assert_eq!(
        completed.status,
        "completed",
        "{completed:?}; tasks={:?}",
        store.tasks(&sid).unwrap()
    );
    let tasks = store.tasks(&sid).unwrap();
    assert_eq!(tasks.len(), 4);
    for t in tasks.iter().filter(|t| t.request.coding) {
        assert_eq!(t.status, "completed", "{t:?}");
        assert_eq!(t.integration_status, "integrated", "{t:?}");
    }
    let integration = store.session(&sid).unwrap().integration_path;
    for i in 0..3 {
        assert_eq!(
            std::fs::read_to_string(integration.join(format!("worker{i}.txt"))).unwrap(),
            format!("worker{i}")
        );
        assert!(!repo.path().join(format!("worker{i}.txt")).exists());
    }
    let events = store.events(&sid, 0, 1000).unwrap();
    assert!(
        events
            .iter()
            .filter(|e| e.kind == "integration" && e.data["status"] == "integrated")
            .count()
            == 3
    );
    assert!(
        !events
            .iter()
            .any(|e| e.data.to_string().contains("key-mock"))
    );
    assert!(mock_state.calls.lock().unwrap().len() == 4);
}
