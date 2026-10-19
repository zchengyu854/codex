use codex_collab_core::{model::*, store::Store};
use serde_json::json;
use tempfile::TempDir;
fn fixture() -> (TempDir, Store, Session, Vec<Agent>) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let session = Session {
        id: id(),
        repository: tmp.path().into(),
        base_commit: "base".into(),
        branch: "branch".into(),
        integration_path: tmp.path().join("integration"),
        validation: vec![vec!["true".into()]],
    };
    store.add_session(&session).unwrap();
    let agents = (0..4)
        .map(|i| Agent {
            id: id(),
            session_id: session.id.clone(),
            role: if i == 0 {
                Role::Coordinator
            } else {
                Role::Worker
            },
            model: format!("model{i}"),
            base_url: "http://127.0.0.1:9000/v1".into(),
            protocol: Protocol::Responses,
            context_bytes: 128_000,
        })
        .collect::<Vec<_>>();
    for a in &agents {
        store.configure(a).unwrap();
    }
    (tmp, store, session, agents)
}
fn req(s: &Session, a: &Agent, deps: Vec<String>, coding: bool) -> TaskRequest {
    TaskRequest {
        session_id: s.id.clone(),
        agent_id: a.id.clone(),
        goal: "goal".into(),
        acceptance: "check".into(),
        dependencies: deps,
        coding,
    }
}
#[test]
fn enforces_roles_and_four_agents() {
    let (_, s, session, a) = fixture();
    let mut extra = a[1].clone();
    extra.id = id();
    assert!(s.configure(&extra).is_err());
    let mut second = a[0].clone();
    second.id = id();
    assert!(s.configure(&second).is_err());
    assert!(
        s.publish(&session.id, Some(&a[1].id), None, "decision", &json!("bad"))
            .is_err()
    );
    assert!(
        s.publish(
            &session.id,
            Some(&a[1].id),
            None,
            "user_override",
            &json!("bad")
        )
        .is_err()
    );
}
#[test]
fn queue_limits_and_one_task_per_agent() {
    let (_, s, session, a) = fixture();
    for agent in &a {
        for _ in 0..2 {
            s.create_task(req(&session, agent, vec![], false)).unwrap();
        }
    }
    let mut running = vec![];
    for _ in 0..4 {
        running.push(s.claim().unwrap().unwrap());
    }
    assert!(s.claim().unwrap().is_none());
    assert_eq!(
        running
            .iter()
            .map(|t| &t.request.agent_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    s.finish(&running[0].id, "completed", "ok", None).unwrap();
    assert!(s.claim().unwrap().is_some());
}
#[test]
fn dependencies_require_coding_integration() {
    let (_, s, session, a) = fixture();
    let first = s.create_task(req(&session, &a[1], vec![], true)).unwrap();
    let next = s
        .create_task(req(&session, &a[2], vec![first.id.clone()], false))
        .unwrap();
    assert_eq!(s.claim().unwrap().unwrap().id, first.id);
    s.finish(&first.id, "completed", "ok", Some("commit"))
        .unwrap();
    assert!(s.claim().unwrap().is_none());
    s.integration(&first.id, "checking", &json!({})).unwrap();
    s.integration(&first.id, "integrated", &json!({})).unwrap();
    assert_eq!(s.claim().unwrap().unwrap().id, next.id);
}
#[test]
fn failed_dependencies_block_and_cancel_wins_over_late_completion() {
    let (_, s, session, a) = fixture();
    let first = s.create_task(req(&session, &a[1], vec![], false)).unwrap();
    let next = s
        .create_task(req(&session, &a[2], vec![first.id.clone()], false))
        .unwrap();
    s.claim().unwrap();
    assert!(s.finish(&first.id, "cancelled", "user", None).unwrap());
    assert!(!s.finish(&first.id, "completed", "late", None).unwrap());
    assert!(s.claim().unwrap().is_none());
    assert_eq!(s.task(&next.id).unwrap().status, "blocked");
}
#[test]
fn recovery_does_not_replay_running_tasks() {
    let (tmp, s, session, a) = fixture();
    let t = s.create_task(req(&session, &a[1], vec![], false)).unwrap();
    s.claim().unwrap();
    drop(s);
    let s = Store::open(tmp.path()).unwrap();
    assert_eq!(s.recover().unwrap(), 1);
    assert_eq!(s.task(&t.id).unwrap().status, "interrupted");
    assert!(s.claim().unwrap().is_none());
}
#[test]
fn cannot_reconfigure_running_or_move_agent() {
    let (_, s, session, a) = fixture();
    s.create_task(req(&session, &a[1], vec![], false)).unwrap();
    s.claim().unwrap();
    assert!(s.configure(&a[1]).is_err());
    let mut moved = a[2].clone();
    moved.session_id = id();
    assert!(s.configure(&moved).is_err());
}
#[test]
fn versioned_context_retains_user_overrides_and_checkpoint_audit() {
    let (_, s, session, a) = fixture();
    let t = s.create_task(req(&session, &a[1], vec![], false)).unwrap();
    s.publish(
        &session.id,
        None,
        None,
        "requirement",
        &json!("keep original files"),
    )
    .unwrap();
    s.publish(
        &session.id,
        Some(&a[0].id),
        None,
        "decision",
        &json!("old decision"),
    )
    .unwrap();
    let rev = s
        .publish(
            &session.id,
            None,
            None,
            "user_override",
            &json!("user wins"),
        )
        .unwrap();
    assert!(s.compact(&session.id, Some(&a[1].id), "bad", rev).is_err());
    assert!(s.compact(&session.id, None, "summary", rev - 1).is_err());
    s.compact(&session.id, None, "current decision summary", rev)
        .unwrap();
    let (v, c) = s.context(&a[1], &t).unwrap();
    let value: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert_eq!(value["checkpoint"]["summary"], "current decision summary");
    assert_eq!(value["formalRecords"].as_array().unwrap().len(), 2);
    assert!(c.contains("user wins"));
    s.acknowledge(&a[1].id, &t.id, v).unwrap();
    let (_, again) = s.context(&a[1], &t).unwrap();
    assert!(again.contains("user wins"));
    assert!(
        s.events(&session.id, 0, 100)
            .unwrap()
            .iter()
            .any(|e| e.kind == "decision")
    );
    assert!(s.events(&session.id, v, 100).unwrap().is_empty());
}
#[test]
fn cross_session_dependencies_rejected() {
    let (_, s, session, a) = fixture();
    let mut other = session.clone();
    other.id = id();
    s.add_session(&other).unwrap();
    let mut agent = a[1].clone();
    agent.id = id();
    agent.session_id = other.id.clone();
    s.configure(&agent).unwrap();
    let t = s.create_task(req(&other, &agent, vec![], false)).unwrap();
    assert!(
        s.create_task(req(&session, &a[1], vec![t.id], false))
            .is_err()
    );
}
#[test]
fn budget_does_not_silently_drop_constraints() {
    let (_, s, session, a) = fixture();
    let t = s.create_task(req(&session, &a[1], vec![], false)).unwrap();
    s.publish(
        &session.id,
        None,
        None,
        "requirement",
        &json!("a".repeat(10_000)),
    )
    .unwrap();
    let mut tiny = a[1].clone();
    tiny.context_bytes = 4096;
    assert!(s.context(&tiny, &t).is_err());
}
#[test]
fn api_url_rejects_embedded_keys_and_remote_plaintext() {
    let (_, _, _, a) = fixture();
    for url in [
        "https://key@example.com/v1",
        "https://example.com/v1?key=secret",
        "http://example.com/v1",
        "file:///tmp/key",
    ] {
        let mut agent = a[1].clone();
        agent.base_url = url.into();
        assert!(agent.validate().is_err());
    }
}
#[test]
fn prevents_two_hosts_recovering_same_state() {
    let (tmp, s, _, _) = fixture();
    assert!(Store::open(tmp.path()).is_err());
    drop(s);
    assert!(Store::open(tmp.path()).is_ok());
}
#[test]
fn approval_rejects_stale_user_choice_after_cancel() {
    let (_, s, session, a) = fixture();
    let t = s.create_task(req(&session, &a[1], vec![], false)).unwrap();
    s.claim().unwrap();
    let approval = Approval {
        id: id(),
        session_id: session.id,
        agent_id: a[1].id.clone(),
        task_id: t.id.clone(),
        method: "item/commandExecution/requestApproval".into(),
        params: json!({"command":"test"}),
        decision: None,
    };
    s.add_approval(&approval).unwrap();
    s.finish(&t.id, "cancelled", "user", None).unwrap();
    assert!(s.decide(&approval.id, true).is_err());
}
