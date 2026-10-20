use codex_collab_core::{
    git,
    model::*,
    runtime::Runtime,
    secrets::{MemorySecrets, Secret, Secrets},
    store::Store,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use tokio::process::Command;

#[path = "support/project.rs"]
mod project;

const WAITING_CODEX: &str = r#"#!/usr/bin/python3
import json, os, subprocess, sys
from pathlib import Path
def emit(v): print(json.dumps(v), flush=True)
for line in sys.stdin:
    v = json.loads(line)
    if v.get('method') == 'initialize': emit({'id': v['id'], 'result': {}})
    elif v.get('method') == 'thread/start': emit({'id': v['id'], 'result': {'thread': {'id': 'waiting'}}})
    elif v.get('method') == 'turn/start':
        Path('partial.py').write_text('# incomplete task\n')
        child = subprocess.Popen(['/usr/bin/python3', '-c', 'import time; time.sleep(120)'])
        Path('processes.json').write_text(json.dumps([os.getpid(), child.pid]))
        emit({'id': v['id'], 'result': {'turn': {'id': 'waiting-turn'}}})
"#;

async fn command(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct Project {
    repo: TempDir,
    root: TempDir,
    rt: Arc<Runtime>,
    session: Session,
    agent: String,
}
impl Project {
    async fn new() -> Self {
        let repo = TempDir::new().unwrap();
        project::overlay(&project::fixture().join("base"), repo.path());
        command(repo.path(), &["init"]).await;
        command(repo.path(), &["add", "."]).await;
        command(
            repo.path(),
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@localhost",
                "commit",
                "-m",
                "baseline",
            ],
        )
        .await;
        let root = TempDir::new().unwrap();
        let script = root.path().join("waiting-codex");
        std::fs::write(&script, WAITING_CODEX).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Arc::new(Store::open(&root.path().join("state")).unwrap());
        let secrets = Arc::new(MemorySecrets::default());
        let rt = Runtime::new(store.clone(), secrets.clone(), script);
        let session: Session = serde_json::from_value(
            rt.dispatch(
                "session/create",
                json!({"repository":repo.path(),"validation":[project::CHECK]}),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        let agent = id();
        store
            .configure(&Agent {
                id: agent.clone(),
                session_id: session.id.clone(),
                role: Role::Worker,
                model: "fixture".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                protocol: Protocol::Responses,
                context_bytes: 128_000,
            })
            .unwrap();
        secrets
            .set(
                &agent,
                Secret::new("local-recovery-fixture".into()).unwrap(),
            )
            .unwrap();
        Self {
            repo,
            root,
            rt,
            session,
            agent,
        }
    }
    fn task(&self, goal: &str) -> Task {
        self.rt
            .store
            .create_task(TaskRequest {
                session_id: self.session.id.clone(),
                agent_id: self.agent.clone(),
                goal: goal.into(),
                acceptance: "project tests pass".into(),
                dependencies: vec![],
                coding: true,
                created_by: None,
            })
            .unwrap()
    }
    async fn candidate(&self, goal: &str, files: Value) -> Task {
        let task = self.task(goal);
        assert_eq!(self.rt.store.claim().unwrap().unwrap().id, task.id);
        let (path, base) = git::task_workspace(&self.session, &task, &self.rt.store.root)
            .await
            .unwrap();
        self.rt.store.workspace(&task.id, &path, &base).unwrap();
        for (name, text) in files.as_object().unwrap() {
            std::fs::write(path.join(name), text.as_str().unwrap()).unwrap();
        }
        let commit = git::commit(&path).await.unwrap();
        self.rt
            .store
            .finish(&task.id, "completed", "candidate committed", Some(&commit))
            .unwrap();
        self.rt.store.task(&task.id).unwrap()
    }
    async fn integrate(&self, task: &Task) -> anyhow::Result<Value> {
        self.rt.dispatch("task/integrate", json!({"taskId":task.id,"expectedCommit":task.commit,"review":"Reviewed exact code and unittest diff; run project validation."})).await
    }
    async fn abort(&self, task: &Task) -> anyhow::Result<Value> {
        self.rt
            .dispatch("task/abortIntegration", json!({"taskId":task.id}))
            .await
    }
    async fn head(&self) -> String {
        command(&self.session.integration_path, &["rev-parse", "HEAD"]).await
    }
    async fn clean(&self) -> bool {
        command(&self.session.integration_path, &["status", "--porcelain"])
            .await
            .is_empty()
    }
    async fn source_clean(&self) -> bool {
        command(self.repo.path(), &["status", "--porcelain"])
            .await
            .is_empty()
    }
}
fn record(name: &str, evidence: Value) {
    if let Ok(dir) = std::env::var("CODEX_COLLAB_RECOVERY_EVIDENCE") {
        std::fs::write(
            Path::new(&dir).join(name),
            format!("{}\n", serde_json::to_string_pretty(&evidence).unwrap()),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn failed_candidate_can_be_dropped_without_losing_it_before_rework() {
    let p = Project::new().await;
    let bad = p
        .candidate(
            "break total",
            json!({"total.py":"def total(values):\n    return 0\n"}),
        )
        .await;
    let failed = p.integrate(&bad).await.unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["checks"][0]["exitCode"], 1);
    assert_eq!(p.head().await, p.session.base_commit);
    let repair = p
        .candidate(&format!("Repair failed task {}", bad.id), project::patch(1))
        .await;
    assert!(
        p.integrate(&repair).await.is_err(),
        "dirty workspace must reject before replacing its integration owner"
    );
    assert_eq!(
        p.rt.store.task(&repair.id).unwrap().integration_status,
        "pending"
    );
    let diagnostic = p.session.integration_path.join("diagnostic.txt");
    std::fs::write(&diagnostic, "validator output to inspect").unwrap();
    assert!(
        p.abort(&bad).await.is_err(),
        "untracked output requires inspection, never silent deletion"
    );
    assert!(diagnostic.exists());
    assert!(
        std::fs::read_to_string(p.session.integration_path.join("total.py"))
            .unwrap()
            .contains("return 0")
    );
    assert_eq!(
        p.rt.store.task(&bad.id).unwrap().integration_status,
        "failed"
    );
    std::fs::remove_file(diagnostic).unwrap();
    let dropped = p.abort(&bad).await.unwrap();
    assert_eq!(dropped["status"], "pending");
    assert!(p.clean().await);
    let after = p.rt.store.task(&bad.id).unwrap();
    assert_eq!(after.commit, bad.commit);
    assert_eq!(after.result, bad.result);
    assert_eq!(after.status, "completed");
    let repaired = p.integrate(&repair).await.unwrap();
    assert_eq!(repaired["status"], "integrated");
    assert_eq!(repaired["previousHead"], p.session.base_commit);
    assert_eq!(
        repair.base_commit.as_deref(),
        Some(p.session.base_commit.as_str())
    );
    assert!(p.source_clean().await);
    assert!(
        p.abort(&repair).await.is_err(),
        "cannot discard integrated work"
    );
    record(
        "stage-8-failure.json",
        json!({"mode":"local_runtime_and_real_git","failedTask":bad,"failure":failed,"abort":dropped,"taskAfterAbort":after,"replacementTask":repair,"replacementIntegration":repaired,"events":p.rt.store.events(&p.session.id,0,1000).unwrap(),"sourceClean":true,"verdict":{"ok":true}}),
    );
}

#[tokio::test]
async fn code_conflict_can_be_dropped_and_reworked_from_current_head() {
    let p = Project::new().await;
    let first = p.candidate("normalize whitespace", project::patch(0)).await;
    let mut patch = project::patch(0);
    patch["slug.py"] =
        json!("def slug(text):\n    return '-'.join(text.strip().lower().split())\n");
    let second = p
        .candidate("alternative whitespace implementation", patch)
        .await;
    assert_eq!(first.base_commit, second.base_commit);
    let integrated = p.integrate(&first).await.unwrap();
    assert_eq!(integrated["status"], "integrated");
    let conflict = p.integrate(&second).await.unwrap();
    assert_eq!(conflict["status"], "conflict");
    assert_eq!(p.head().await, integrated["head"].as_str().unwrap());
    assert!(!p.clean().await);
    let dropped = p.abort(&second).await.unwrap();
    assert!(p.clean().await);
    assert_eq!(dropped["head"], integrated["head"]);
    assert_eq!(p.rt.store.task(&second.id).unwrap().commit, second.commit);
    let mut patch = project::patch(0);
    patch["slug.py"] = json!("def slug(text):\n    return '-'.join(text.casefold().split())\n");
    let test = patch["tests/test_slug.py"].as_str().unwrap().to_owned()
        + "\n    def test_casefold(self):\n        self.assertEqual(slug('Straße'), 'strasse')\n";
    patch["tests/test_slug.py"] = json!(test);
    let rework = p
        .candidate(&format!("Rework conflicting task {}", second.id), patch)
        .await;
    assert_ne!(rework.id, second.id);
    assert_eq!(rework.base_commit.as_deref(), integrated["head"].as_str());
    let result = p.integrate(&rework).await.unwrap();
    assert_eq!(result["status"], "integrated");
    assert!(
        result["checks"][0]["stderr"]
            .as_str()
            .unwrap()
            .contains("Ran 7 tests")
    );
    assert!(p.source_clean().await);
    record(
        "stage-8-conflict.json",
        json!({"mode":"local_runtime_and_real_git","firstTask":first,"firstIntegration":integrated,"conflictingTask":second,"conflict":conflict,"abort":dropped,"reworkTask":rework,"reworkIntegration":result,"events":p.rt.store.events(&p.session.id,0,1000).unwrap(),"sourceClean":true,"verdict":{"ok":true}}),
    );
}

#[tokio::test]
async fn stale_conflict_cannot_discard_another_tasks_failed_candidate() {
    let p = Project::new().await;
    let first = p.candidate("first slug", project::patch(0)).await;
    let integration = &p.session.integration_path;
    command(integration, &["checkout", "-b", "manual-empty-merge"]).await;
    command(
        integration,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "--allow-empty",
            "-m",
            "empty branch",
        ],
    )
    .await;
    command(integration, &["checkout", &p.session.branch]).await;
    command(
        integration,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "merge",
            "--no-ff",
            "--no-commit",
            "manual-empty-merge",
        ],
    )
    .await;
    assert_eq!(command(integration, &["status", "--porcelain"]).await, "");
    assert!(
        p.integrate(&first).await.is_err(),
        "an in-flight merge must not acquire a task owner"
    );
    assert_eq!(p.rt.store.integration_task(&p.session.id).unwrap(), None);
    command(integration, &["merge", "--abort"]).await;

    let old = p.candidate("old slug",json!({"slug.py":"def slug(text):\n    return '-'.join(text.strip().lower().split())\n"})).await;
    assert_eq!(p.integrate(&first).await.unwrap()["status"], "integrated");
    assert_eq!(p.integrate(&old).await.unwrap()["status"], "conflict");
    // Simulate an operator restoring files outside the host while the old task still says conflict.
    git::abort_integration(&p.session).await.unwrap();
    let bad = p
        .candidate(
            "broken total",
            json!({"total.py":"def total(values):\n    return 0\n"}),
        )
        .await;
    assert_eq!(p.integrate(&bad).await.unwrap()["status"], "failed");
    assert!(
        p.abort(&old).await.is_err(),
        "old conflict must not reset the latest candidate"
    );
    assert!(
        std::fs::read_to_string(p.session.integration_path.join("total.py"))
            .unwrap()
            .contains("return 0")
    );
    assert_eq!(p.abort(&bad).await.unwrap()["status"], "pending");
}

#[tokio::test]
async fn cancellation_kills_the_process_group_and_restart_keeps_committed_work() {
    let p = Project::new().await;
    let committed = p
        .candidate("completed total change", project::patch(1))
        .await;
    let waiting = p.task("wait without finishing");
    let scheduler = p.rt.start();
    let (worktree, pids) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let task = p.rt.store.task(&waiting.id).unwrap();
            if let Some(path) = task.worktree
                && let Ok(bytes) = std::fs::read(path.join("processes.json"))
                && let Ok(pids) = serde_json::from_slice::<Vec<i32>>(&bytes)
            {
                break (path, pids);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("fixture process did not start");
    p.rt.cancel(&waiting.id).await.unwrap();
    p.rt.stop().await;
    scheduler.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10),async {
        loop {
            if pids.iter().all(|pid| unsafe {libc::kill(*pid,0)} == -1 && std::io::Error::last_os_error().raw_os_error()==Some(libc::ESRCH)) {break;}
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("cancelled task left a process running");
    assert_eq!(p.rt.store.task(&waiting.id).unwrap().status, "cancelled");
    assert!(worktree.join("partial.py").exists());
    assert!(p.rt.store.approvals().unwrap().is_empty());
    // Seed the persisted states a crash leaves; no process is running when the store is reopened.
    let crashed = p.task("crashed before completion");
    assert_eq!(p.rt.store.claim().unwrap().unwrap().id, crashed.id);
    let (partial, base) = git::task_workspace(&p.session, &crashed, &p.rt.store.root)
        .await
        .unwrap();
    p.rt.store.workspace(&crashed.id, &partial, &base).unwrap();
    std::fs::write(partial.join("partial.py"), "# crash partial work\n").unwrap();
    p.rt.store
        .integration(
            &committed.id,
            "checking",
            &json!({"review":"simulated in-flight integration"}),
        )
        .unwrap();
    let state = p.root.path().join("state");
    let script = p.root.path().join("waiting-codex");
    let original_head = p.head().await;
    let Project {
        repo,
        root,
        rt,
        session,
        agent: _,
    } = p;
    drop(rt);
    // Gateway shutdown releases its Store clone when the aborted server task is polled.
    let store = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match Store::open(&state) {
                Ok(store) => break Arc::new(store),
                Err(error) if error.to_string().contains("already open by another host") => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => panic!("could not reopen state: {error}"),
            }
        }
    })
    .await
    .expect("stopped host retained its state lock");
    assert_eq!(store.recover().unwrap(), 2);
    let preserved = store.task(&committed.id).unwrap();
    assert_eq!(preserved.commit, committed.commit);
    assert_eq!(preserved.result, committed.result);
    assert_eq!(preserved.integration_status, "interrupted");
    assert_eq!(store.task(&crashed.id).unwrap().status, "interrupted");
    assert_eq!(store.task(&waiting.id).unwrap().status, "cancelled");
    let rt = Runtime::new(store.clone(), Arc::new(MemorySecrets::default()), script);
    let scheduler = rt.start();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    rt.stop().await;
    scheduler.await.unwrap();
    assert_eq!(store.task(&crashed.id).unwrap().status, "interrupted");
    assert!(
        !partial.join("processes.json").exists(),
        "restart must not replay interrupted work"
    );
    assert!(partial.join("partial.py").exists());
    let dropped = rt
        .dispatch("task/abortIntegration", json!({"taskId":committed.id}))
        .await
        .unwrap();
    assert_eq!(dropped["head"], original_head);
    assert_eq!(dropped["status"], "pending");
    assert_eq!(command(repo.path(), &["status", "--porcelain"]).await, "");
    let events = store.events(&session.id, 0, 1000).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "recovery_required")
            .count(),
        2
    );
    assert_eq!(
        store.recover().unwrap(),
        0,
        "recovery must not duplicate markers"
    );
    record(
        "stage-8-interruption.json",
        json!({"mode":"fixture_app_server_and_simulated_crash_state","cancelledTask":store.task(&waiting.id).unwrap(),"interruptedTask":store.task(&crashed.id).unwrap(),"committedTaskBeforeRecovery":committed,"committedTaskAfterRecovery":preserved,"abortInterruptedIntegration":dropped,"processIds":pids,"processGroupStopped":true,"partialWorkPreserved":true,"replayed":false,"sourceClean":true,"events":events,"verdict":{"ok":true}}),
    );
    drop(rt);
    drop(store);
    drop(root);
}
