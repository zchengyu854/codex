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
async fn setup() -> (
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
    std::fs::write(&script, FIXTURE).unwrap();
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
        role: Role::Worker,
        model: "fixture".into(),
        base_url: "http://127.0.0.1:1/v1".into(),
        protocol: Protocol::Responses,
        context_bytes: 128_000,
    };
    store.configure(&a).unwrap();
    secrets
        .set(&a.id, Secret::new("upstream-test-secret".into()).unwrap())
        .unwrap();
    let t = store
        .create_task(TaskRequest {
            session_id: sid,
            agent_id: a.id,
            goal: "fixture".into(),
            acceptance: "fixture".into(),
            dependencies: vec![],
            coding: false,
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
