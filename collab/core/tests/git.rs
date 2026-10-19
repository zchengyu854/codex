use codex_collab_core::{git, model::*};
use tempfile::TempDir;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
async fn cmd(p: &std::path::Path, a: &[&str]) -> String {
    let o = Command::new("git")
        .args(a)
        .current_dir(p)
        .output()
        .await
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().into()
}
async fn repo() -> TempDir {
    let t = TempDir::new().unwrap();
    cmd(t.path(), &["init"]).await;
    std::fs::write(t.path().join("file"), "base\n").unwrap();
    cmd(t.path(), &["add", "."]).await;
    cmd(
        t.path(),
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
    t
}
fn task(s: &Session) -> Task {
    Task {
        id: id(),
        request: TaskRequest {
            session_id: s.id.clone(),
            agent_id: id(),
            goal: "code".into(),
            acceptance: "check".into(),
            dependencies: vec![],
            coding: true,
        },
        status: "completed".into(),
        context_version: 0,
        worktree: None,
        base_commit: None,
        commit: None,
        result: None,
        integration_status: "pending".into(),
    }
}
async fn candidate(s: &Session, root: &std::path::Path, file: &str, text: &str) -> Task {
    let mut t = task(s);
    let (p, base) = git::task_workspace(s, &t, root).await.unwrap();
    std::fs::write(p.join(file), text).unwrap();
    t.worktree = Some(p.clone());
    t.base_commit = Some(base);
    t.commit = Some(git::commit(&p).await.unwrap());
    t
}
#[tokio::test]
async fn worktrees_exclude_dirty_original_and_revalidate_stale_results() {
    let repo = repo().await;
    std::fs::write(repo.path().join("uncommitted"), "private").unwrap();
    let root = TempDir::new().unwrap();
    let s = git::create_session(repo.path(), root.path(), vec![vec!["/usr/bin/true".into()]])
        .await
        .unwrap();
    assert!(!s.integration_path.join("uncommitted").exists());
    let a = candidate(&s, root.path(), "a", "a").await;
    let b = candidate(&s, root.path(), "b", "b").await;
    let token = CancellationToken::new();
    let first = git::integrate(&s, &a, &token).await.unwrap();
    assert_eq!(first["status"], "integrated");
    let second = git::integrate(&s, &b, &token).await.unwrap();
    assert_eq!(second["status"], "integrated");
    assert_eq!(second["revalidated"], true);
    assert!(s.integration_path.join("a").exists() && s.integration_path.join("b").exists());
    assert!(!repo.path().join("a").exists());
}
#[tokio::test]
async fn failed_validation_preserves_candidate_without_advancing_branch() {
    let repo = repo().await;
    let root = TempDir::new().unwrap();
    let s = git::create_session(
        repo.path(),
        root.path(),
        vec![vec!["/usr/bin/false".into()]],
    )
    .await
    .unwrap();
    let t = candidate(&s, root.path(), "file", "new").await;
    let r = git::integrate(&s, &t, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(r["status"], "failed");
    assert_eq!(
        cmd(&s.integration_path, &["rev-parse", "HEAD"]).await,
        s.base_commit
    );
    assert!(
        !cmd(&s.integration_path, &["status", "--porcelain"])
            .await
            .is_empty()
    );
}
#[tokio::test]
async fn conflicting_change_pauses_without_advancing_branch() {
    let repo = repo().await;
    let root = TempDir::new().unwrap();
    let s = git::create_session(repo.path(), root.path(), vec![vec!["/usr/bin/true".into()]])
        .await
        .unwrap();
    let a = candidate(&s, root.path(), "file", "first\n").await;
    let b = candidate(&s, root.path(), "file", "second\n").await;
    let token = CancellationToken::new();
    assert_eq!(
        git::integrate(&s, &a, &token).await.unwrap()["status"],
        "integrated"
    );
    let head = cmd(&s.integration_path, &["rev-parse", "HEAD"]).await;
    assert_eq!(
        git::integrate(&s, &b, &token).await.unwrap()["status"],
        "conflict"
    );
    assert_eq!(cmd(&s.integration_path, &["rev-parse", "HEAD"]).await, head);
}
#[tokio::test]
async fn non_git_and_missing_validation_are_rejected() {
    let tmp = TempDir::new().unwrap();
    assert!(
        git::create_session(tmp.path(), tmp.path(), vec![])
            .await
            .is_err()
    );
    assert!(
        git::create_session(tmp.path(), tmp.path(), vec![vec!["true".into()]])
            .await
            .is_err()
    );
}
#[tokio::test]
async fn validation_cannot_sneak_unreviewed_changes_into_commit() {
    let repo = repo().await;
    let root = TempDir::new().unwrap();
    let s = git::create_session(
        repo.path(),
        root.path(),
        vec![vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf unwanted > sneaky".into(),
        ]],
    )
    .await
    .unwrap();
    let t = candidate(&s, root.path(), "file", "new").await;
    let r = git::integrate(&s, &t, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(r["status"], "failed");
    assert_eq!(
        cmd(&s.integration_path, &["rev-parse", "HEAD"]).await,
        s.base_commit
    );
}
