use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use codex_collab_core::{gateway::Gateway, model::*, protocol::sse, secrets::Secret, store::Store};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
#[derive(Default)]
struct Mock {
    bodies: Mutex<Vec<Value>>,
    auth: Mutex<Vec<String>>,
    failures: std::sync::atomic::AtomicUsize,
    slow: bool,
}
async fn mock(
    State(s): State<Arc<Mock>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    s.bodies.lock().unwrap().push(body.clone());
    s.auth
        .lock()
        .unwrap()
        .push(headers["authorization"].to_str().unwrap().into());
    if s.failures
        .fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |n| n.checked_sub(1),
        )
        .is_ok()
    {
        // A real provider counts the limit per credential; `Retry-After: 0` keeps the suite fast
        // while still taking the honour-the-header path.
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "0")],
            "secret-upstream-key in provider error",
        )
            .into_response();
    }
    if s.slow {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    let data = if body.get("messages").is_some() {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"chat reply"},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3}})
        )
    } else {
        [json!({"type":"response.created","response":{"id":"r"}}),json!({"type":"response.output_text.delta","delta":"responses reply"}),json!({"type":"response.completed","response":{"id":"r","usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}})].iter().map(sse).collect::<String>()
    };
    ([("content-type", "text/event-stream")], data).into_response()
}
async fn upstream(state: Arc<Mock>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/responses", post(mock))
        .route("/v1/chat/completions", post(mock))
        .with_state(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, handle)
}
fn state(tmp: &TempDir, url: String, protocol: Protocol) -> (Arc<Store>, Agent, Task) {
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let s = Session {
        id: id(),
        repository: tmp.path().into(),
        base_commit: "base".into(),
        branch: "branch".into(),
        integration_path: tmp.path().into(),
        validation: vec![vec!["true".into()]],
    };
    store.add_session(&s).unwrap();
    let a = Agent {
        id: id(),
        session_id: s.id.clone(),
        role: Role::Coordinator,
        model: "chosen-model".into(),
        base_url: url,
        protocol,
        context_bytes: 128_000,
    };
    store.configure(&a).unwrap();
    store
        .create_task(TaskRequest {
            session_id: s.id,
            agent_id: a.id.clone(),
            goal: "task".into(),
            acceptance: "check".into(),
            dependencies: vec![],
            coding: false,
            created_by: None,
        })
        .unwrap();
    let t = store.claim().unwrap().unwrap();
    (store, a, t)
}
fn body() -> Value {
    json!({"model":"wrong-model","stream":true,"input":[{"role":"user","content":"question"}],"tools":[]})
}
#[tokio::test]
async fn both_protocols_inject_current_records_every_model_request() {
    for protocol in [Protocol::Responses, Protocol::ChatCompletions] {
        let mock = Arc::new(Mock::default());
        let (url, server) = upstream(mock.clone()).await;
        let tmp = TempDir::new().unwrap();
        let (store, a, t) = state(&tmp, url, protocol.clone());
        store
            .publish(
                &a.session_id,
                None,
                None,
                "requirement",
                &json!("shared requirement"),
            )
            .unwrap();
        let gateway = Gateway::start(
            store.clone(),
            a.clone(),
            t.clone(),
            Secret::new("secret-upstream-key".into()).unwrap(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let endpoint = format!("{}/responses", gateway.url);
        let first = client
            .post(&endpoint)
            .bearer_auth(&gateway.token)
            .json(&body())
            .send()
            .await
            .unwrap();
        assert!(first.status().is_success());
        let text = first.text().await.unwrap();
        assert!(text.contains("response.completed"), "{text}");
        assert!(!text.contains("secret-upstream-key"));
        let rev = store
            .publish(
                &a.session_id,
                None,
                None,
                "user_override",
                &json!("new decision while tool ran"),
            )
            .unwrap();
        let second = client
            .post(&endpoint)
            .bearer_auth(&gateway.token)
            .json(&body())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(second.contains("response.completed"));
        let requests = mock.bodies.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].to_string().contains("shared requirement"));
        assert!(
            requests[1]
                .to_string()
                .contains("new decision while tool ran")
        );
        assert_eq!(requests[0]["model"], "chosen-model");
        assert!(
            mock.auth
                .lock()
                .unwrap()
                .iter()
                .all(|h| h == "Bearer secret-upstream-key")
        );
        assert!(store.task(&t.id).unwrap().context_version >= rev);
        assert!(
            !store
                .events(&a.session_id, 0, 100)
                .unwrap()
                .iter()
                .any(|e| e.data.to_string().contains("secret-upstream-key"))
        );
        server.abort();
    }
}
#[tokio::test]
async fn local_auth_rejects_cross_agent_access_and_bounded_retry() {
    let started = std::time::Instant::now();
    let mock = Arc::new(Mock::default());
    mock.failures.store(2, std::sync::atomic::Ordering::SeqCst);
    let (url, server) = upstream(mock.clone()).await;
    let tmp = TempDir::new().unwrap();
    let (store, a, t) = state(&tmp, url, Protocol::Responses);
    let gateway = Gateway::start(
        store,
        a,
        t,
        Secret::new("secret-upstream-key".into()).unwrap(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let c = reqwest::Client::new();
    let endpoint = format!("{}/responses", gateway.url);
    assert_eq!(
        c.post(&endpoint)
            .json(&body())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = c
        .post(&endpoint)
        .bearer_auth(&gateway.token)
        .json(&body())
        .send()
        .await
        .unwrap();
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("response.completed")
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "Retry-After was ignored: {:?}",
        started.elapsed()
    );
    assert_eq!(mock.bodies.lock().unwrap().len(), 3);
    server.abort();
}
#[tokio::test]
async fn cancellation_aborts_waiting_upstream_request() {
    let mock = Arc::new(Mock {
        slow: true,
        ..Default::default()
    });
    let (url, server) = upstream(mock).await;
    let tmp = TempDir::new().unwrap();
    let (store, a, t) = state(&tmp, url, Protocol::Responses);
    let token = CancellationToken::new();
    let gateway = Gateway::start(
        store,
        a,
        t,
        Secret::new("secret-upstream-key".into()).unwrap(),
        &token,
    )
    .await
    .unwrap();
    let c = reqwest::Client::new();
    let request = c
        .post(format!("{}/responses", gateway.url))
        .bearer_auth(&gateway.token)
        .json(&body())
        .send();
    tokio::pin!(request);
    tokio::select! {_=tokio::time::sleep(std::time::Duration::from_millis(50))=>{},_= &mut request=>panic!("request should still be waiting")};
    token.cancel();
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), request)
        .await
        .unwrap();
    if let Ok(r) = response {
        assert_eq!(r.status(), 409);
    }
    server.abort();
}
#[tokio::test]
async fn unsupported_chat_tool_fails_before_contacting_upstream() {
    let mock = Arc::new(Mock::default());
    let (url, server) = upstream(mock.clone()).await;
    let tmp = TempDir::new().unwrap();
    let (store, a, t) = state(&tmp, url, Protocol::ChatCompletions);
    let gateway = Gateway::start(
        store,
        a,
        t,
        Secret::new("secret-upstream-key".into()).unwrap(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut request = body();
    request["tools"] = json!([{"type":"web_search"}]);
    let response = reqwest::Client::new()
        .post(format!("{}/responses", gateway.url))
        .bearer_auth(&gateway.token)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    assert!(mock.bodies.lock().unwrap().is_empty());
    server.abort();
}
