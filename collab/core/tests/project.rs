//! The stage 8 fixture must detect broken code, not merely produce a green transcript.
use codex_collab_core::{git, model::*};
use std::path::Path;
use tempfile::TempDir;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[path = "support/project.rs"]
mod project;

async fn check(path: &Path) -> std::process::Output {
    Command::new(project::CHECK[0])
        .args(&project::CHECK[1..])
        .current_dir(path)
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn project_tests_detect_regressions_in_each_feature() {
    let root = TempDir::new().unwrap();
    project::overlay(&project::fixture().join("base"), root.path());
    for i in 0..3 {
        for (name, text) in project::patch(i).as_object().unwrap() {
            std::fs::write(root.path().join(name), text.as_str().unwrap()).unwrap();
        }
    }
    let result = check(root.path()).await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("Ran 11 tests"));
    for (index, name, broken) in [
        (
            0,
            "slug.py",
            "def slug(text):\n    return text.strip().lower().replace(' ', '-')\n",
        ),
        (
            1,
            "total.py",
            "def total(values):\n    return sum(values)\n",
        ),
        (
            2,
            "report.py",
            "def render(title, values):\n    return 'wrong'\n",
        ),
    ] {
        std::fs::write(root.path().join(name), broken).unwrap();
        let result = check(root.path()).await;
        assert!(!result.status.success(), "tests accepted broken {name}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("FAILED"));
        std::fs::write(
            root.path().join(name),
            project::patch(index)[name].as_str().unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn real_project_test_failure_preserves_candidate_and_branch() {
    let repo = TempDir::new().unwrap();
    project::overlay(&project::fixture().join("base"), repo.path());
    for args in [
        vec!["init"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "-m",
            "baseline",
        ],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(check(repo.path()).await.status.success());
    let root = TempDir::new().unwrap();
    let s = git::create_session(
        repo.path(),
        root.path(),
        vec![project::CHECK.iter().map(|v| (*v).into()).collect()],
    )
    .await
    .unwrap();
    let mut task = Task {
        id: id(),
        request: TaskRequest {
            session_id: s.id.clone(),
            agent_id: id(),
            goal: "change total".into(),
            acceptance: "project tests pass".into(),
            dependencies: vec![],
            coding: true,
            created_by: None,
        },
        status: "completed".into(),
        context_version: 0,
        worktree: None,
        base_commit: None,
        commit: None,
        result: None,
        integration_status: "pending".into(),
    };
    let (worktree, base) = git::task_workspace(&s, &task, root.path()).await.unwrap();
    std::fs::write(
        worktree.join("total.py"),
        "def total(values):\n    return 0\n",
    )
    .unwrap();
    task.base_commit = Some(base);
    task.worktree = Some(worktree.clone());
    task.commit = Some(git::commit(&worktree).await.unwrap());
    let result = git::integrate(&s, &task, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(
        result["checks"][0]["command"],
        serde_json::json!(project::CHECK)
    );
    assert_eq!(result["checks"][0]["success"], false);
    assert_eq!(result["checks"][0]["exitCode"], 1);
    assert!(
        result["checks"][0]["stderr"]
            .as_str()
            .unwrap()
            .contains("FAILED")
    );
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&s.integration_path)
        .output()
        .await
        .unwrap();
    assert!(head.status.success());
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), s.base_commit);
    assert_eq!(
        std::fs::read_to_string(s.integration_path.join("total.py")).unwrap(),
        "def total(values):\n    return 0\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("total.py")).unwrap(),
        "def total(values):\n    return 0\n"
    );
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo.path())
        .output()
        .await
        .unwrap();
    assert!(status.status.success() && status.stdout.is_empty());
}
