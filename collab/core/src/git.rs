use crate::model::*;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::Path;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

async fn command(path: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .current_dir(path)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn()?;
    let _group = crate::runtime::ProcessGroup(child.id());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        child.wait_with_output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.first().unwrap_or(&"operation"),
        clip(&String::from_utf8_lossy(&output.stderr), 3000)
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}
pub async fn create_session(
    repo: &Path,
    root: &Path,
    validation: Vec<Vec<String>>,
) -> Result<Session> {
    ensure!(
        !validation.is_empty()
            && validation
                .iter()
                .all(|c| !c.is_empty() && c.iter().all(|a| !a.contains('\0'))),
        "at least one explicit validation command is required"
    );
    let repo = std::fs::canonicalize(repo)?;
    ensure!(
        command(&repo, &["rev-parse", "--is-inside-work-tree"]).await? == "true",
        "select a Git repository"
    );
    let base = command(&repo, &["rev-parse", "HEAD"]).await?;
    let sid = id();
    let branch = format!("codex/collab-{sid}");
    let path = root.join("worktrees").join(&sid).join("integration");
    std::fs::create_dir_all(path.parent().unwrap())?;
    command(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid path"))?,
            &base,
        ],
    )
    .await?;
    Ok(Session {
        id: sid,
        repository: repo,
        base_commit: base,
        branch,
        integration_path: path,
        validation,
    })
}
pub async fn task_workspace(
    s: &Session,
    t: &Task,
    root: &Path,
) -> Result<(std::path::PathBuf, String)> {
    let base = command(&s.integration_path, &["rev-parse", "HEAD"]).await?;
    let path = root.join("worktrees").join(&s.id).join(&t.id);
    let branch = format!("codex/task-{}", t.id);
    command(
        &s.repository,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid path"))?,
            &base,
        ],
    )
    .await?;
    Ok((path, base))
}
pub async fn commit(path: &Path) -> Result<String> {
    command(path, &["add", "--all"]).await?;
    let changes = command(path, &["diff", "--cached", "--name-only"]).await?;
    if !changes.is_empty() {
        command(
            path,
            &[
                "-c",
                "user.name=Codex Collab",
                "-c",
                "user.email=codex-collab@localhost",
                "commit",
                "-m",
                "Collaborative task result",
            ],
        )
        .await?;
    }
    command(path, &["rev-parse", "HEAD"]).await
}
pub async fn diff(path: &Path, base: &str) -> Result<String> {
    command(
        path,
        &["diff", "--no-ext-diff", "--no-textconv", base, "--"],
    )
    .await
}
pub async fn integrate(s: &Session, t: &Task, cancel: &CancellationToken) -> Result<Value> {
    let base = t
        .base_commit
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing task base"))?;
    let task_commit = t
        .commit
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing task commit"))?;
    let clean = command(&s.integration_path, &["status", "--porcelain"]).await?;
    ensure!(
        clean.is_empty(),
        "integration worktree is dirty; inspect before retrying"
    );
    let original = command(&s.integration_path, &["rev-parse", "HEAD"]).await?;
    let git_dir = command(&s.integration_path, &["rev-parse", "--absolute-git-dir"]).await?;
    ensure!(
        !Path::new(&git_dir).join("MERGE_HEAD").exists(),
        "merge already in progress; resolve explicitly"
    );
    if base != task_commit {
        // Squash into the dedicated integration worktree. The collaboration branch only
        // advances after validation succeeds; a conflict is preserved for inspection.
        if command(
            &s.integration_path,
            &["merge", "--squash", "--no-commit", task_commit],
        )
        .await
        .is_err()
        {
            return Ok(
                json!({"status":"conflict","originalHead":original,"taskCommit":task_commit,"message":"merge failed; inspect integration worktree before retrying"}),
            );
        }
    }
    let candidate_status = command(&s.integration_path, &["status", "--porcelain"]).await?;
    let candidate_diff = diff(&s.integration_path, &original).await?;
    let mut checks = Vec::new();
    for args in &s.validation {
        if cancel.is_cancelled() {
            return Ok(json!({"status":"interrupted","originalHead":original}));
        }
        let mut check = Command::new(&args[0]);
        check
            .args(&args[1..])
            .current_dir(&s.integration_path)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        check.process_group(0);
        let child = check.spawn()?;
        let _group = crate::runtime::ProcessGroup(child.id());
        let output = tokio::select! { _=cancel.cancelled()=>return Ok(json!({"status":"interrupted","originalHead":original})), out=tokio::time::timeout(std::time::Duration::from_secs(300),child.wait_with_output())=>out?? };
        checks.push(json!({"command":args,"success":output.status.success(),"stdout":clip(&String::from_utf8_lossy(&output.stdout),16_000),"stderr":clip(&String::from_utf8_lossy(&output.stderr),16_000)}));
        if !output.status.success() {
            return Ok(
                json!({"status":"failed","originalHead":original,"checks":checks,"message":"failed candidate preserved in integration worktree; collaboration branch not advanced"}),
            );
        }
    }
    if candidate_status != command(&s.integration_path, &["status", "--porcelain"]).await?
        || candidate_diff != diff(&s.integration_path, &original).await?
    {
        return Ok(
            json!({"status":"failed","checks":checks,"message":"validation modified candidate files; inspect changes before retrying"}),
        );
    }
    let integrated = commit(&s.integration_path).await?;
    Ok(
        json!({"status":"integrated","previousHead":original,"head":integrated,"taskCommit":task_commit,"revalidated":true,"checks":checks}),
    )
}

/// Undo the half-applied merge a conflict left in the integration worktree.
///
/// `git merge --squash` that fails leaves the index unmerged and the worktree modified, which makes
/// every later integration refuse with "worktree is dirty". The conflicting content is already in
/// the integration event, so this restores the branch head and returns it.
pub async fn abort_integration(s: &Session) -> Result<String> {
    command(&s.integration_path, &["merge", "--abort"])
        .await
        .ok();
    command(&s.integration_path, &["reset", "--hard", "HEAD"]).await?;
    command(&s.integration_path, &["rev-parse", "HEAD"]).await
}

pub async fn committed_diff(path: &Path, base: &str, commit: &str) -> Result<String> {
    command(
        path,
        &["diff", "--no-ext-diff", "--no-textconv", base, commit, "--"],
    )
    .await
}
