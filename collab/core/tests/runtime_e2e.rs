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
#[path = "support/project.rs"]
mod project;

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
        let independent: Vec<_> = tasks
            .iter()
            .filter(|t| t.request.dependencies.is_empty())
            .collect();
        let dependent = tasks.iter().find(|t| !t.request.dependencies.is_empty());
        match step {
            0 => for (i, goal) in ["Normalize repeated whitespace in slug.py and extend its tests", "Accept integer strings in total.py and extend its tests"].iter().enumerate() {
                items.push(call(&body, "collab_create_task", json!({"agentId":m.workers[i],"goal":goal,"acceptance":"existing and new unittest cases pass","coding":true,"dependencies":[]}), i));
            },
            1 => {
                assert_eq!(independent.len(), 2);
                items.push(call(&body, "collab_create_task", json!({"agentId":m.workers[2],"goal":"Add report.py CLI using slug and total, with tests","acceptance":"CLI prints hello-world:5 and rejects invalid integers","coding":true,"dependencies":independent.iter().map(|t| &t.id).collect::<Vec<_>>()}), 0));
            },
            2 => {
                assert_eq!(dependent.unwrap().status, "queued", "dependency must wait for integration");
                for (i,t) in independent.iter().enumerate() { items.push(call(&body,"collab_wait_task",json!({"taskId":t.id}),i)); }
            },
            3 | 6 => {
                let targets = if step == 3 { independent } else { vec![dependent.unwrap()] };
                for (i,t) in targets.iter().enumerate() {
                    assert_eq!(t.status, "completed", "{t:?}");
                    items.push(call(&body,"collab_review_diff",json!({"taskId":t.id}),i));
                }
            },
            4 | 7 => {
                let targets = if step == 4 { independent } else { vec![dependent.unwrap()] };
                for (i,t) in targets.iter().enumerate() {
                    items.push(call(&body,"collab_integrate",json!({"taskId":t.id,"expectedCommit":t.commit,"review":"Reviewed implementation and unittest changes; run the project's full suite."}),i));
                }
            },
            5 => items.push(call(&body,"collab_wait_task",json!({"taskId":dependent.unwrap().id}),0)),
            _ => items.push(json!({"id":"msg_main","type":"message","role":"assistant","content":[{"type":"output_text","text":"All code changes reviewed, tested and integrated."}]})),
        }
    } else if step == 0 {
        let index: usize = model.trim_start_matches("mock-worker").parse().unwrap();
        let text = format!(
            "/usr/bin/python3 -B - <<'PY'\nfrom pathlib import Path\nimport subprocess\nfor name, content in {}.items():\n    Path(name).write_text(content)\nsubprocess.run({}, check=True)\nPY",
            project::patch(index),
            serde_json::to_string(&project::CHECK).unwrap()
        );
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
        items.push(json!({"id":"msg_worker","type":"message","role":"assistant","content":[{"type":"output_text","text":"Changed implementation and passed the project unittest suite."}]}));
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
async fn coordinator_integrates_a_tested_code_project_across_both_protocols() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init"]).await;
    project::overlay(&project::fixture().join("base"), repo.path());
    let baseline = Command::new(project::CHECK[0])
        .args(&project::CHECK[1..])
        .current_dir(repo.path())
        .output()
        .await
        .unwrap();
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    assert!(String::from_utf8_lossy(&baseline.stderr).contains("Ran 4 tests"));
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
    let session = rt.dispatch("session/create", json!({"repository":repo.path(),"validation":[project::CHECK],"requirement":"Keep the source checkout unchanged. Preserve existing tests and add coverage for each change."})).await.unwrap();
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
    let task=store.create_task(TaskRequest{session_id:sid.clone(),agent_id:main,goal:"Delegate two independent code changes, then a CLI task depending on both. Review diffs and integrate after unittest validation.".into(),acceptance:"all three code changes integrated and project tests pass".into(),dependencies:vec![],coding:false,created_by:None}).unwrap();
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
    let checked = Command::new(project::CHECK[0])
        .args(&project::CHECK[1..])
        .current_dir(&integration)
        .output()
        .await
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(String::from_utf8_lossy(&checked.stderr).contains("Ran 11 tests"));
    let source = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo.path())
        .output()
        .await
        .unwrap();
    assert!(source.status.success() && source.stdout.is_empty());
    assert!(!repo.path().join("report.py").exists());
    for t in tasks.iter().filter(|t| t.request.coding) {
        assert_eq!(t.request.created_by.as_deref(), Some(task.id.as_str()));
    }
    let events = store.events(&sid, 0, 1000).unwrap();
    let integrations: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "integration" && e.data["status"] == "integrated")
        .collect();
    assert_eq!(integrations.len(), 3);
    for event in &integrations {
        let check = &event.data["evidence"]["checks"][0];
        assert_eq!(check["command"], json!(project::CHECK));
        assert_eq!(check["success"], true);
        assert_eq!(check["exitCode"], 0);
        assert!(check["stderr"].as_str().unwrap().contains("tests"));
    }
    let dependent = tasks
        .iter()
        .find(|t| t.request.dependencies.len() == 2)
        .unwrap();
    let started = events
        .iter()
        .find(|e| e.kind == "task_started" && e.task_id.as_deref() == Some(&dependent.id))
        .unwrap();
    for dependency in &dependent.request.dependencies {
        let integrated = integrations
            .iter()
            .find(|e| e.task_id.as_ref() == Some(dependency))
            .unwrap();
        assert!(integrated.revision < started.revision);
    }
    let previous = &integrations[1].data["evidence"]["head"];
    assert_eq!(dependent.base_commit.as_deref(), previous.as_str());
    let mut head = session["baseCommit"].clone();
    for event in &integrations {
        assert_eq!(event.data["evidence"]["previousHead"], head);
        head = event.data["evidence"]["head"].clone();
    }
    assert!(
        !events
            .iter()
            .any(|e| e.data.to_string().contains("key-mock"))
    );
    assert!(mock_state.calls.lock().unwrap().len() == 4);
    if let Ok(path) = std::env::var("CODEX_COLLAB_EVIDENCE") {
        let evidence = json!({"mode":"local_mock_upstream_real_codex","real_provider_tested":false,"baseline_tests":4,"baselineCheck":{"command":project::CHECK,"exitCode":baseline.status.code(),"stdout":String::from_utf8_lossy(&baseline.stdout),"stderr":String::from_utf8_lossy(&baseline.stderr)},"final_tests":11,"finalCheck":{"command":project::CHECK,"exitCode":checked.status.code(),"stdout":String::from_utf8_lossy(&checked.stdout),"stderr":String::from_utf8_lossy(&checked.stderr)},"validation":project::CHECK,"tasks":tasks,"events":events,"source_repository_clean":true,"verdict":{"ok":true,"problems":[]}});
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&evidence).unwrap()),
        )
        .unwrap();
    }
}
