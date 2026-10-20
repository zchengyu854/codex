use codex_collab_core::{
    model::*,
    runtime::Runtime,
    secrets::{MemorySecrets, Secret, Secrets},
    store::Store,
};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::process::Command;
const FIXTURE: &str = r#"#!/usr/bin/python3
import sys,json,os
assert 'COLLAB_GATEWAY_KEY' in os.environ
assert 'OPENAI_API_KEY' not in os.environ
assert not any(v == 'upstream-test-secret' for v in os.environ.values())
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    value=json.loads(line)
    method=value.get('method')
    if method=='initialize': emit({'id':value['id'],'result':{}})
    elif method=='thread/start': emit({'id':value['id'],'result':{'thread':{'id':'fixture-thread'}}})
    elif method=='turn/start':
        emit({'id':value['id'],'result':{'turn':{'id':'fixture-turn'}}})
        emit({'id':'approve-fixture','method':'item/commandExecution/requestApproval','params':{'threadId':'fixture-thread','turnId':'fixture-turn','command':'printf fixture'}})
    elif value.get('id')=='approve-fixture':
        assert value['result']['decision'] in ('accept','decline')
        emit({'id':'publish-fixture','method':'item/tool/call','params':{'tool':'collab_publish','arguments':{'kind':'result','data':os.environ['COLLAB_GATEWAY_KEY']}}})
    elif value.get('id')=='publish-fixture':
        emit({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'fixture completed '+os.environ['COLLAB_GATEWAY_KEY']}}})
        emit({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
"#;
const FAILING_FIXTURE: &str = r#"#!/usr/bin/python3
import sys,json
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    value=json.loads(line)
    method=value.get('method')
    if method=='initialize': emit({'id':value['id'],'result':{}})
    elif method=='thread/start': emit({'id':value['id'],'result':{'thread':{'id':'fixture-thread'}}})
    elif method=='turn/start':
        emit({'id':value['id'],'result':{'turn':{'id':'fixture-turn'}}})
        emit({'method':'turn/completed','params':{'turn':{'status':'failed','error':{'message':'unexpected status 401 Unauthorized: upstream authentication failed; update this agent credential'}}}})
"#;
async fn setup() -> (
    TempDir,
    TempDir,
    Arc<Runtime>,
    Task,
    tokio::task::JoinHandle<()>,
) {
    setup_with(FIXTURE).await
}
const CONTINUATION_FIXTURE: &str = r#"#!/usr/bin/python3
import sys, json
turn = 0
worker = 'aaaaaaaa-0000-4000-8000-000000000002'
def emit(v): print(json.dumps(v), flush=True)
for line in sys.stdin:
    value = json.loads(line)
    method = value.get('method')
    if method == 'initialize': emit({'id': value['id'], 'result': {}})
    elif method == 'thread/start': emit({'id': value['id'], 'result': {'thread': {'id': 'fixture-thread'}}})
    elif method == 'turn/start':
        turn += 1
        emit({'id': value['id'], 'result': {'turn': {'id': 'fixture-turn-%d' % turn}}})
        if turn == 1:
            emit({'id': 'create-1', 'method': 'item/tool/call', 'params': {'tool': 'collab_create_task', 'arguments': {'agentId': worker, 'goal': 'phantom delegated work', 'acceptance': 'not really checked', 'coding': True, 'dependencies': []}}})
        emit({'method': 'item/completed', 'params': {'item': {'type': 'agentMessage', 'id': 'msg-%d' % turn, 'text': 'turn-%d' % turn}}})
        emit({'method': 'turn/completed', 'params': {'turn': {'status': 'completed'}}})
"#;
async fn setup_with(
    fixture: &str,
) -> (
    TempDir,
    TempDir,
    Arc<Runtime>,
    Task,
    tokio::task::JoinHandle<()>,
) {
    setup_as(fixture, Role::Worker).await
}
async fn setup_as(
    fixture: &str,
    role: Role,
) -> (
    TempDir,
    TempDir,
    Arc<Runtime>,
    Task,
    tokio::task::JoinHandle<()>,
) {
    let repo = TempDir::new().unwrap();
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let o = Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        assert!(o.status.success());
    }
    let root = TempDir::new().unwrap();
    let script = root.path().join("fixture-codex");
    std::fs::write(&script, fixture).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let store = Arc::new(Store::open(&root.path().join("state")).unwrap());
    let secrets = Arc::new(MemorySecrets::default());
    let rt = Runtime::new(store.clone(), secrets.clone(), script);
    let session = rt
        .dispatch(
            "session/create",
            json!({"repository":repo.path(),"validation":[["/usr/bin/true"]]}),
        )
        .await
        .unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    let a = Agent {
        id: id(),
        session_id: sid.clone(),
        role: role.clone(),
        model: "fixture".into(),
        base_url: "http://127.0.0.1:1/v1".into(),
        protocol: Protocol::Responses,
        context_bytes: 128_000,
    };
    store.configure(&a).unwrap();
    secrets
        .set(&a.id, Secret::new("upstream-test-secret".into()).unwrap())
        .unwrap();
    if role == Role::Coordinator {
        let worker = Agent {
            id: "aaaaaaaa-0000-4000-8000-000000000002".into(),
            session_id: sid.clone(),
            role: Role::Worker,
            model: "fixture".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            protocol: Protocol::Responses,
            context_bytes: 128_000,
        };
        store.configure(&worker).unwrap();
        secrets
            .set(
                &worker.id,
                Secret::new("upstream-test-secret".into()).unwrap(),
            )
            .unwrap();
    }
    let t = store
        .create_task(TaskRequest {
            session_id: sid,
            agent_id: a.id,
            goal: "fixture".into(),
            acceptance: "fixture".into(),
            dependencies: vec![],
            coding: false,
            created_by: None,
        })
        .unwrap();
    let schedule = rt.start();
    (repo, root, rt, t, schedule)
}
async fn pending(rt: &Runtime) -> Approval {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(a) = rt.store.approvals().unwrap().into_iter().next() {
                break a;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn routes_approval_to_exact_agent_and_redacts_local_credentials() {
    let (_repo, _root, rt, t, schedule) = setup().await;
    let a = pending(&rt).await;
    assert_eq!(a.agent_id, t.request.agent_id);
    assert_eq!(a.task_id, t.id);
    rt.dispatch("approval/decide", json!({"approvalId":a.id,"accept":true}))
        .await
        .unwrap();
    let done = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let t = rt.store.task(&t.id).unwrap();
            if t.status != "running" && t.status != "queued" {
                break t;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(done.status, "completed", "{done:?}");
    assert!(done.result.unwrap().contains("[REDACTED]"));
    let events = rt.store.events(&t.request.session_id, 0, 1000).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "result" && e.data == "[REDACTED]")
    );
    assert!(
        !events
            .iter()
            .any(|e| e.data.to_string().contains("upstream-test-secret"))
    );
    assert!(rt.store.approvals().unwrap().is_empty());
    rt.stop().await;
    schedule.await.unwrap();
}
#[tokio::test]
async fn failed_turn_surfaces_the_protocol_error_message() {
    let (_repo, _root, rt, t, _schedule) = setup_with(FAILING_FIXTURE).await;
    let done = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let task = rt.store.task(&t.id).unwrap();
            if task.status != "running" && task.status != "queued" {
                break task;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(done.status, "failed", "{done:?}");
    // Dropping the turn payload made every upstream error look the same.
    let result = done.result.unwrap();
    assert!(
        result.contains("Agent turn failed or interrupted"),
        "{result}"
    );
    assert!(
        result.contains("upstream authentication failed"),
        "the protocol error text is not surfaced: {result}"
    );
}
#[test]
fn the_commit_rule_reaches_both_prompts() {
    use codex_collab_core::runtime::{COMMIT_RULE, DEVELOPER_INSTRUCTIONS};
    assert!(DEVELOPER_INSTRUCTIONS.contains(COMMIT_RULE));
    // The line breaks in the constant must not swallow the spaces around them.
    assert!(DEVELOPER_INSTRUCTIONS.contains("collab tools for coordination"));
    assert!(DEVELOPER_INSTRUCTIONS.contains("the host commits the workspace"));
    assert!(DEVELOPER_INSTRUCTIONS.contains("before every model request"));
}
#[tokio::test]
async fn a_turn_that_leaves_delegated_work_is_continued_then_flagged() {
    let (_repo, _root, rt, t, _schedule) = setup_as(CONTINUATION_FIXTURE, Role::Coordinator).await;
    let done = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let task = rt.store.task(&t.id).unwrap();
            if task.status != "running" && task.status != "queued" {
                break task;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the coordinator task never reached a terminal state");
    assert_eq!(done.status, "completed", "{done:?}");

    let events = rt.store.events(&t.request.session_id, 0, 500).unwrap();
    let texts: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "public_message")
        .map(|e| e.data["text"].as_str().unwrap_or("").to_owned())
        .collect();
    // Two nudges really reached the model: each continuation produced another turn.
    assert!(texts.iter().any(|t| t == "turn-2"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "turn-3"), "{texts:?}");
    let continued: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.kind == "task_continued")
        .map(|e| &e.data)
        .collect();
    // At least one nudge reached the model; the exact count depends on whether the delegated task
    // changed status between turns, which the stall guard counts differently.
    assert!(
        !continued.is_empty() && continued.len() <= 6,
        "{continued:?}"
    );
    assert_eq!(continued[0]["attempt"], 1);
    for (index, nudge) in continued.iter().enumerate() {
        assert_eq!(nudge["attempt"], index as u64 + 1, "{continued:?}");
    }
    let flagged: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.kind == "task_incomplete")
        .map(|e| &e.data)
        .collect();
    assert_eq!(flagged.len(), 1, "{flagged:?}");
    assert!(
        matches!(
            flagged[0]["reason"].as_str(),
            Some("no progress between nudges" | "continuation budget exhausted")
        ),
        "{flagged:?}"
    );
    // It is unfinished whichever side of the queue it is on: never integrated, so the nudge was
    // right to fire.
    assert!(matches!(
        flagged[0]["tasks"][0]["status"].as_str(),
        Some("queued" | "running" | "completed")
    ));
    assert_eq!(flagged[0]["tasks"][0]["integrationStatus"], "pending");

    // The delegated task carries host-authored provenance.
    let delegated = rt.store.tasks(&t.request.session_id).unwrap();
    let child = delegated
        .iter()
        .find(|task| task.request.agent_id != t.request.agent_id)
        .expect("no delegated task");
    assert_eq!(child.request.created_by.as_deref(), Some(t.id.as_str()));
}
#[tokio::test]
async fn cancel_while_awaiting_approval_cleans_up_and_rejects_late_choice() {
    let (_repo, _root, rt, t, schedule) = setup().await;
    let a = pending(&rt).await;
    rt.cancel(&t.id).await.unwrap();
    assert_eq!(rt.store.task(&t.id).unwrap().status, "cancelled");
    assert!(
        rt.dispatch("approval/decide", json!({"approvalId":a.id,"accept":true}))
            .await
            .is_err()
    );
    rt.stop().await;
    schedule.await.unwrap();
    assert!(rt.store.approvals().unwrap().is_empty());
}
