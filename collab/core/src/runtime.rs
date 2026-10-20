use crate::{
    gateway::Gateway,
    git,
    model::*,
    secrets::{Secret, Secrets, redact},
    store::Store,
};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

pub struct Runtime {
    pub store: Arc<Store>,
    secrets: Arc<dyn Secrets>,
    codex: PathBuf,
    active: Mutex<HashMap<String, CancellationToken>>,
    integration: Mutex<()>,
    pub shutdown: CancellationToken,
    pub updates: tokio::sync::broadcast::Sender<Value>,
    drafts: Mutex<HashMap<String, Value>>,
}
impl Runtime {
    pub fn new(store: Arc<Store>, secrets: Arc<dyn Secrets>, codex: PathBuf) -> Arc<Self> {
        let (updates, _) = tokio::sync::broadcast::channel(256);
        Arc::new(Self {
            updates,
            drafts: Mutex::new(HashMap::new()),
            store,
            secrets,
            codex,
            active: Mutex::new(HashMap::new()),
            integration: Mutex::new(()),
            shutdown: CancellationToken::new(),
        })
    }
    pub fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let rt = self.clone();
        tokio::spawn(async move {
            loop {
                if rt.shutdown.is_cancelled() {
                    break;
                }
                loop {
                    let task = match rt.store.claim() {
                        Ok(Some(t)) => t,
                        _ => break,
                    };
                    let token = rt.shutdown.child_token();
                    rt.active
                        .lock()
                        .await
                        .insert(task.id.clone(), token.clone());
                    let rt = rt.clone();
                    tokio::spawn(async move {
                        let id = task.id.clone();
                        let result = tokio::select! {_=token.cancelled()=>Err(anyhow::anyhow!("task cancelled")),r=rt.run(task.clone(),&token)=>r};
                        if let Err(e) = result {
                            let status = if token.is_cancelled() {
                                "cancelled"
                            } else {
                                "failed"
                            };
                            let _ = rt.store.finish(&id, status, &e.to_string(), None);
                        }
                        rt.active.lock().await.remove(&id);
                        rt.drafts.lock().await.remove(&id);
                        let _=rt.updates.send(json!({"kind":"task_changed","sessionId":task.request.session_id,"agentId":task.request.agent_id,"taskId":id}));
                    });
                }
                tokio::select! {_=rt.shutdown.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(50))=>{}}
            }
        })
    }
    pub async fn cancel(&self, id: &str) -> Result<()> {
        self.store.finish(
            id,
            "cancelled",
            "cancelled by user or coordinator; inspect worktree for partial effects",
            None,
        )?;
        if let Some(t) = self.active.lock().await.get(id) {
            t.cancel();
        }
        Ok(())
    }
    pub async fn stop(&self) {
        self.shutdown.cancel();
        for token in self.active.lock().await.values() {
            token.cancel();
        }
        for _ in 0..100 {
            if self.active.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn run(self: &Arc<Self>, mut task: Task, cancel: &CancellationToken) -> Result<()> {
        let agent = self.store.agent(&task.request.agent_id)?;
        let session = self.store.session(&task.request.session_id)?;
        let key = self.secrets.get(&agent.id)?;
        let (path, base) = {
            let _guard = self.integration.lock().await;
            git::task_workspace(&session, &task, &self.store.root).await?
        };
        self.store.workspace(&task.id, &path, &base)?;
        task.worktree = Some(path.clone());
        task.base_commit = Some(base.clone());
        let redactions = [zeroize::Zeroizing::new(key.expose().to_owned())];
        let gateway =
            Gateway::start(self.store.clone(), agent.clone(), task.clone(), key, cancel).await?;
        let home = self.store.root.join("homes").join(&task.id);
        std::fs::create_dir_all(&home)?;
        let mut command = Command::new(&self.codex);
        for config in [
            "model_provider=\"collab\"".into(),
            "model_providers.collab.name=\"Collaboration gateway\"".into(),
            format!(
                "model_providers.collab.base_url={}",
                serde_json::to_string(&gateway.url)?
            ),
            "model_providers.collab.env_key=\"COLLAB_GATEWAY_KEY\"".into(),
            "model_providers.collab.wire_api=\"responses\"".into(),
            "model_providers.collab.request_max_retries=0".into(),
            "model_providers.collab.stream_max_retries=0".into(),
            "features.multi_agent=false".into(),
            "features.multi_agent_v2=false".into(),
            "features.code_mode=false".into(),
            "features.code_mode_only=false".into(),
            "web_search=\"disabled\"".into(),
            "check_for_update_on_startup=false".into(),
        ] {
            command.arg("-c").arg(config);
        }
        command
            .args(["app-server", "--listen", "stdio://"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("CODEX_HOME", &home)
            .env("COLLAB_GATEWAY_KEY", &gateway.token)
            .current_dir(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let _group = ProcessGroup(child.id());
        let mut stdin = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
            stdin.write_all(format!("{value}\n").as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        }
        send(&mut stdin,json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex_collab","version":"0.1.0"},"capabilities":{"experimentalApi":true}}})).await?;
        let mut initialized = false;
        let mut started = false;
        let mut thread: Option<String> = None;
        let mut next_id = 4u64;
        let mut continuations = 0u32;
        let mut stalled = 0u32;
        let mut previous: Option<String> = None;
        let mut output = String::new();
        let mut stream_redactor = crate::secrets::StreamRedactor::new(vec![
            gateway.token.clone(),
            redactions[0].to_string(),
        ]);
        loop {
            let line = tokio::select! {_=cancel.cancelled()=>bail!("task cancelled"),r=tokio::time::timeout(Duration::from_secs(600),lines.next_line())=>r??};
            let Some(line) = line else {
                bail!("App Server disconnected; inspect task effects before retrying")
            };
            ensure!(line.len() <= 8_000_000, "App Server message too large");
            let value: Value = serde_json::from_str(&line)
                .map_err(|_| anyhow::anyhow!("invalid App Server JSON"))?;
            if value.get("error").is_some() {
                bail!("App Server RPC rejected request; check model and configuration");
            }
            if value["id"] == 1 && !initialized {
                initialized = true;
                send(&mut stdin, json!({"method":"initialized"})).await?;
                send(&mut stdin,json!({"id":2,"method":"thread/start","params":{"model":agent.model,"modelProvider":"collab","cwd":path,"approvalPolicy":"on-request","sandbox":if task.request.coding{"workspace-write"}else{"read-only"},"ephemeral":false,"dynamicTools":tools(agent.role==Role::Coordinator),"developerInstructions":DEVELOPER_INSTRUCTIONS}})).await?;
                continue;
            }
            if value["id"] == 2 && !started {
                started = true;
                let started_thread = value["result"]["thread"]["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("thread/start returned no thread"))?
                    .to_owned();
                thread = Some(started_thread.clone());
                send(&mut stdin,json!({"id":3,"method":"turn/start","params":{"threadId":started_thread,"input":[{"type":"text","text":format!("Task: {}\nAcceptance: {}\n{}",task.request.goal,task.request.acceptance,COMMIT_RULE),"text_elements":[]}]}})).await?;
                continue;
            }
            let method = value["method"].as_str().unwrap_or("");
            let p = &value["params"];
            if method == "item/agentMessage/delta" {
                let text = stream_redactor.push(p["delta"].as_str().unwrap_or(""));
                let item_id = p["itemId"].as_str().unwrap_or("message");
                let mut drafts = self.drafts.lock().await;
                let draft = drafts
                    .entry(task.id.clone())
                    .or_insert_with(|| json!({"itemId":item_id,"text":""}));
                if draft["itemId"] != item_id {
                    *draft = json!({"itemId":item_id,"text":""});
                }
                let current = draft["text"].as_str().unwrap_or("");
                ensure!(
                    current.len() + text.len() <= 4_000_000,
                    "Agent output exceeded limit"
                );
                draft["text"] = json!(format!("{current}{text}"));
                let _=self.updates.send(json!({"kind":"agent_delta","sessionId":agent.session_id,"agentId":agent.id,"taskId":task.id,"itemId":item_id,"delta":text}));
            } else if method == "item/tool/call" {
                let safe_args: Value = serde_json::from_str(&redact(
                    &p["arguments"].to_string(),
                    &[&gateway.token, redactions[0].as_str()],
                ))?;
                let result = self
                    .tool(
                        &agent,
                        &task,
                        p["tool"].as_str().unwrap_or(""),
                        &safe_args,
                        cancel,
                    )
                    .await;
                let response = match result {
                    Ok(v) => {
                        json!({"contentItems":[{"type":"inputText","text":v.to_string()}],"success":true})
                    }
                    Err(e) => {
                        json!({"contentItems":[{"type":"inputText","text":redact(&e.to_string(),&[&gateway.token,redactions[0].as_str()])}],"success":false})
                    }
                };
                send(&mut stdin, json!({"id":value["id"],"result":response})).await?;
            } else if matches!(
                method,
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
            ) {
                let a = Approval {
                    id: id(),
                    session_id: agent.session_id.clone(),
                    agent_id: agent.id.clone(),
                    task_id: task.id.clone(),
                    method: method.into(),
                    params: serde_json::from_str(&redact(
                        &p.to_string(),
                        &[&gateway.token, redactions[0].as_str()],
                    ))?,
                    decision: None,
                };
                self.store.add_approval(&a)?;
                let _=self.updates.send(json!({"kind":"approval_requested","sessionId":agent.session_id,"agentId":agent.id,"taskId":task.id,"approvalId":a.id}));
                let _approval_guard = ApprovalGuard {
                    store: self.store.clone(),
                    id: a.id.clone(),
                };
                self.store.publish(
                    &agent.session_id,
                    Some(&agent.id),
                    Some(&task.id),
                    "approval_requested",
                    &json!({"approvalId":a.id,"method":method}),
                )?;
                let accepted = loop {
                    if let Some(v) = self.store.approval_decision(&a.id)? {
                        break v;
                    }
                    tokio::select! {_=cancel.cancelled()=>{let _=self.store.delete_approval(&a.id);bail!("cancelled while awaiting approval");},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
                };
                self.store.delete_approval(&a.id)?;
                send(&mut stdin,json!({"id":value["id"],"result":{"decision":if accepted{"accept"}else{"decline"}}})).await?;
            } else if value.get("id").is_some() && value.get("method").is_some() {
                send(&mut stdin,json!({"id":value["id"],"error":{"code":-32601,"message":"Unsupported client request; requires explicit client capability"}})).await?;
            } else if method == "item/completed" && p["item"]["type"] == "agentMessage" {
                let tail = stream_redactor.finish();
                if !tail.is_empty() {
                    let _=self.updates.send(json!({"kind":"agent_delta","sessionId":agent.session_id,"agentId":agent.id,"taskId":task.id,"itemId":p["item"]["id"],"delta":tail}));
                }
                self.drafts.lock().await.remove(&task.id);
                let text = p["item"]["text"].as_str().unwrap_or("");
                let text = redact(text, &[&gateway.token, redactions[0].as_str()]);
                output.push_str(&text);
                output.push('\n');
                ensure!(output.len() <= 4_000_000, "Agent output exceeded limit");
                self.store.publish(
                    &agent.session_id,
                    Some(&agent.id),
                    Some(&task.id),
                    "public_message",
                    &json!({"text":clip(&text,16_000)}),
                )?;
            } else if method == "turn/completed" {
                if p["turn"]["status"] != "completed" {
                    // The protocol carries the failure text (for example the gateway's 401/429
                    // message); dropping it left every upstream error indistinguishable.
                    let detail = p["turn"]["error"]["message"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    let detail = redact(&detail, &[&gateway.token, redactions[0].as_str()]);
                    // A failed turn cannot nudge itself, but whatever it delegated is now
                    // orphaned: report it once so the session does not look finished and the
                    // operator hears about the queued work instead of discovering it later.
                    let orphaned =
                        unfinished_delegated(&self.store, &task.request.session_id, &task.id)?;
                    if !orphaned.is_empty() {
                        let summary: Vec<Value> = orphaned.iter().map(delegated_summary).collect();
                        let reason = if detail.is_empty() {
                            "turn failed".to_owned()
                        } else {
                            format!("turn failed: {}", clip(&detail, 200))
                        };
                        self.store.note(
                            &task.request.session_id,
                            Some(&task.request.agent_id),
                            Some(&task.id),
                            "task_incomplete",
                            &json!({"tasks": summary, "reason": reason}),
                        )?;
                        let _ = self.updates.send(json!({
                            "kind": "task_changed",
                            "sessionId": task.request.session_id,
                            "agentId": task.request.agent_id,
                            "taskId": task.id,
                        }));
                    }
                    if detail.is_empty() {
                        bail!("Agent turn failed or interrupted");
                    }
                    bail!("Agent turn failed or interrupted: {}", clip(&detail, 500));
                }
                // A coordinator turn can end while work it delegated is still queued, running or
                // unintegrated. The model then reports success and the operator has to notice the
                // half-done session by eye, so nudge the same thread instead of accepting it.
                let pending =
                    unfinished_delegated(&self.store, &task.request.session_id, &task.id)?;
                if pending.is_empty() {
                    break;
                }
                let summary: Vec<Value> = pending.iter().map(delegated_summary).collect();
                let signature = pending
                    .iter()
                    .map(|t| format!("{}:{}:{}", t.id, t.status, t.integration_status))
                    .collect::<Vec<_>>()
                    .join(",");
                stalled = if previous.as_deref() == Some(signature.as_str()) {
                    stalled + 1
                } else {
                    0
                };
                previous = Some(signature);
                if continuations >= MAX_CONTINUATIONS || stalled >= MAX_STALLED_CONTINUATIONS {
                    self.store.note(
                        &agent.session_id,
                        Some(&agent.id),
                        Some(&task.id),
                        "task_incomplete",
                        &json!({
                            "tasks": summary,
                            "reason": if stalled >= MAX_STALLED_CONTINUATIONS {
                                "no progress between nudges"
                            } else {
                                "continuation budget exhausted"
                            },
                        }),
                    )?;
                    let _ = self.updates.send(json!({"kind":"task_changed","sessionId":agent.session_id,"agentId":agent.id,"taskId":task.id}));
                    break;
                }
                continuations += 1;
                self.store.note(
                    &agent.session_id,
                    Some(&agent.id),
                    Some(&task.id),
                    "task_continued",
                    &json!({"attempt": continuations, "tasks": summary}),
                )?;
                let mut listed = String::new();
                for t in &pending {
                    listed.push_str(&format!(
                        "- {} status={} integration={} goal={}\n",
                        t.id,
                        t.status,
                        t.integration_status,
                        clip(&t.request.goal, 160)
                    ));
                }
                let nudge = format!(
                    "Your turn ended, but work you created is still unfinished:\n{listed}\
Continue now: wait for it, review its diff, integrate it when it is coding, and only then report. Do not create replacement tasks unless one failed."
                );
                let id = next_id;
                next_id += 1;
                let thread = thread
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("continuation without a thread"))?;
                send(&mut stdin,json!({"id":id,"method":"turn/start","params":{"threadId":thread,"input":[{"type":"text","text":nudge,"text_elements":[]}]}})).await?;
                continue;
            }
        }
        // Terminate the app-server before collecting files so it cannot race the commit.
        let _ = child.kill().await;
        let _ = child.wait().await;
        if output.is_empty() {
            output = "Task completed without a public message. Inspect task evidence.".into();
        }
        self.store.attachment(&task, "full result", &output)?;
        let commit = if task.request.coding {
            Some(git::commit(&path).await?)
        } else {
            None
        };
        if task.request.coding {
            let diff = git::diff(&path, &base).await?;
            let safe_diff = redact(&diff, &[&gateway.token, redactions[0].as_str()]);
            self.store.attachment(&task, "file diff", &safe_diff)?;
        }
        self.store
            .finish(&task.id, "completed", &output, commit.as_deref())?;
        Ok(())
    }
    async fn tool(
        &self,
        agent: &Agent,
        task: &Task,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        match name {
            "collab_list_tasks" => Ok(serde_json::to_value(self.store.tasks(&agent.session_id)?)?),
            "collab_publish" => {
                let kind = args["kind"].as_str().unwrap_or("suggestion");
                ensure!(
                    matches!(kind, "suggestion" | "result" | "decision"),
                    "unsupported publication kind"
                );
                Ok(
                    json!({"revision":self.store.publish(&agent.session_id,Some(&agent.id),Some(&task.id),kind,&args["data"])?}),
                )
            }
            "collab_create_task" => {
                ensure!(
                    agent.role == Role::Coordinator,
                    "only coordinator may assign tasks"
                );
                let mut args = args.clone();
                args["sessionId"] = json!(agent.session_id);
                let mut request: TaskRequest = serde_json::from_value(args)?;
                ensure!(
                    request.agent_id != agent.id,
                    "coordinator cannot queue work to itself while running"
                );
                request.created_by = Some(task.id.clone());
                Ok(serde_json::to_value(self.store.create_task(request)?)?)
            }
            "collab_cancel_task" => {
                ensure!(
                    agent.role == Role::Coordinator,
                    "only coordinator may cancel another task"
                );
                let id = required(args, "taskId")?;
                ensure!(id != task.id, "cannot cancel self through a tool");
                ensure!(
                    self.store.task(id)?.request.session_id == agent.session_id,
                    "wrong session"
                );
                self.cancel(id).await?;
                Ok(json!({"cancelled":true}))
            }
            "collab_wait_task" => {
                let id = required(args, "taskId")?;
                ensure!(id != task.id, "cannot wait on self");
                let other = self.store.task(id)?;
                ensure!(
                    other.request.session_id == agent.session_id,
                    "wrong session"
                );
                ensure!(
                    other.request.agent_id != agent.id,
                    "cannot wait on queued work for same agent"
                );
                let start = tokio::time::Instant::now();
                loop {
                    let t = self.store.task(id)?;
                    if !matches!(t.status.as_str(), "running" | "queued")
                        || start.elapsed() > Duration::from_secs(60)
                    {
                        return Ok(serde_json::to_value(t)?);
                    }
                    tokio::select! {_=cancel.cancelled()=>bail!("cancelled"),_=tokio::time::sleep(Duration::from_millis(100))=>{}}
                }
            }
            "collab_review_diff" => {
                let id = required(args, "taskId")?;
                let other = self.store.task(id)?;
                ensure!(
                    other.request.session_id == agent.session_id,
                    "wrong session"
                );
                let path = other
                    .worktree
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("workspace not ready"))?;
                let base = other
                    .base_commit
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("base missing"))?;
                let diff = if let Some(commit) = other.commit.as_deref() {
                    git::committed_diff(path, base, commit).await?
                } else {
                    git::diff(path, base).await?
                };
                Ok(
                    json!({"commit":other.commit,"diff":clip(&diff,48_000),"truncated":diff.len()>48_000}),
                )
            }
            "collab_integrate" => {
                ensure!(agent.role == Role::Coordinator, "coordinator required");
                let other = self.store.task(required(args, "taskId")?)?;
                ensure!(
                    other.request.session_id == agent.session_id,
                    "wrong session"
                );
                self.integrate(
                    &other,
                    required(args, "expectedCommit")?,
                    required(args, "review")?,
                    cancel,
                )
                .await
            }
            "collab_compact" => {
                ensure!(agent.role == Role::Coordinator, "coordinator required");
                Ok(
                    json!({"revision":self.store.compact(&agent.session_id,Some(&agent.id),required(args,"summary")?,args["expectedRevision"].as_i64().ok_or_else(||anyhow::anyhow!("expectedRevision required"))?)?}),
                )
            }
            _ => bail!("unknown collaboration tool"),
        }
    }
    pub async fn integrate(
        &self,
        task: &Task,
        expected: &str,
        review: &str,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        ensure!(!review.trim().is_empty(), "review evidence is required");
        let _guard = self.integration.lock().await;
        let fresh = self.store.task(&task.id)?;
        ensure!(
            fresh.commit.as_deref() == Some(expected),
            "reviewed commit changed"
        );
        ensure!(
            fresh.integration_status != "integrated",
            "already integrated"
        );
        let s = self.store.session(&fresh.request.session_id)?;
        self.store.integration(
            &task.id,
            "checking",
            &json!({"review":clip(review,8192),"reviewedCommit":expected}),
        )?;
        let mut guard = IntegrationGuard {
            store: self.store.clone(),
            id: task.id.clone(),
            finished: false,
        };
        let result = match git::integrate(&s, &fresh, cancel).await {
            Ok(v) => v,
            Err(_) => {
                json!({"status":"failed","message":"integration could not finish; inspect candidate worktree before retrying"})
            }
        };
        self.store.integration(
            &task.id,
            result["status"].as_str().unwrap_or("failed"),
            &result,
        )?;
        guard.finished = true;
        Ok(result)
    }
    /// Drop a conflicted candidate so the session can keep integrating.
    ///
    /// A conflict is not resolved here: the operator (or the coordinator) decides what the correct
    /// content is. What this guarantees is that the integration worktree is back on the branch
    /// head, so the next reviewed task is not blocked by the previous conflict.
    pub async fn abort_integration(&self, task_id: &str) -> Result<Value> {
        let _guard = self.integration.lock().await;
        let fresh = self.store.task(task_id)?;
        ensure!(
            fresh.integration_status == "conflict",
            "task is not in an integration conflict"
        );
        let s = self.store.session(&fresh.request.session_id)?;
        let head = git::abort_integration(&s).await?;
        let result = json!({"status":"pending","aborted":true,"head":head,"message":"conflicted candidate dropped; integrate again after the change is reworked"});
        self.store.integration(task_id, "pending", &result)?;
        Ok(result)
    }
    /// JSON RPC facade shared by the headless host and the future Tauri command layer.
    pub async fn dispatch(self: &Arc<Self>, method: &str, args: Value) -> Result<Value> {
        match method {
            "session/create" => {
                let _guard = self.integration.lock().await;
                let validation: Vec<Vec<String>> =
                    serde_json::from_value(args["validation"].clone())?;
                let session = git::create_session(
                    Path::new(required(&args, "repository")?),
                    &self.store.root,
                    validation,
                )
                .await?;
                self.store.add_session(&session)?;
                if let Some(req) = args.get("requirement") {
                    self.store
                        .publish(&session.id, None, None, "requirement", req)?;
                }
                Ok(serde_json::to_value(session)?)
            }
            "session/list" => Ok(serde_json::to_value(self.store.sessions()?)?),
            "agent/configure" => {
                let agent: Agent = serde_json::from_value(args)?;
                ensure!(
                    uuid::Uuid::parse_str(&agent.id).is_ok(),
                    "agent id must be a UUID"
                );
                self.store.configure(&agent)?;
                Ok(serde_json::to_value(agent)?)
            }
            "agent/setCredential" => {
                let agent = required(&args, "agentId")?;
                self.store.agent(agent)?;
                ensure!(
                    !self
                        .store
                        .tasks(&self.store.agent(agent)?.session_id)?
                        .iter()
                        .any(|t| t.request.agent_id == agent && t.status == "running"),
                    "stop current task before changing credential"
                );
                self.secrets
                    .set(agent, Secret::new(required(&args, "key")?.into())?)?;
                Ok(json!({"saved":true}))
            }
            "agent/deleteCredential" => {
                self.secrets.delete(required(&args, "agentId")?)?;
                Ok(json!({"deleted":true}))
            }
            "agent/list" => Ok(serde_json::to_value(
                self.store.agents(required(&args, "sessionId")?)?,
            )?),
            "agent/testConnection" => {
                let agent = self.store.agent(required(&args, "agentId")?)?;
                let t=self.store.create_task(TaskRequest{session_id:agent.session_id,agent_id:agent.id,goal:"Connection capability test: first call collab_publish with kind=result and data=connection_probe_ok, then answer connection_probe_ok. Do not read files or run shell commands.".into(),acceptance:"Successful streaming response and collab_publish tool call; inspect task evidence and shared result event.".into(),dependencies:vec![],coding:false,created_by:None})?;
                Ok(serde_json::to_value(t)?)
            }
            "agent/connectionResult" => {
                let task = self.store.task(required(&args, "taskId")?)?;
                ensure!(
                    task.request.goal.starts_with("Connection capability test:"),
                    "task is not a connection probe"
                );
                let tool = self.store.has_event(
                    &task.id,
                    "result",
                    Some(&json!("connection_probe_ok")),
                )?;
                let stream = self.store.has_event(&task.id, "usage", None)?;
                let verified = task.status == "completed" && tool && stream;
                Ok(
                    json!({"taskId":task.id,"status":task.status,"verified":verified,"capabilities":if verified{vec!["text","stream","function_tools"]}else{vec![]}}),
                )
            }
            "task/create" => {
                let mut request: TaskRequest = serde_json::from_value(args)?;
                // Delegation provenance is host-authored: a client cannot claim a task on behalf
                // of another task.
                request.created_by = None;
                Ok(serde_json::to_value(self.store.create_task(request)?)?)
            }
            "task/list" => Ok(serde_json::to_value(
                self.store.tasks(required(&args, "sessionId")?)?,
            )?),
            "task/output" => {
                let task = self.store.task(required(&args, "taskId")?)?;
                Ok(
                    json!({"taskId":task.id,"result":task.result,"draft":self.drafts.lock().await.get(&task.id).cloned()}),
                )
            }
            "task/get" => Ok(serde_json::to_value(
                self.store.task(required(&args, "taskId")?)?,
            )?),
            "task/cancel" => {
                self.cancel(required(&args, "taskId")?).await?;
                Ok(json!({"cancelled":true}))
            }
            "task/abortIntegration" => self.abort_integration(required(&args, "taskId")?).await,
            "task/integrate" => {
                let task = self.store.task(required(&args, "taskId")?)?;
                self.integrate(
                    &task,
                    required(&args, "expectedCommit")?,
                    required(&args, "review")?,
                    &self.shutdown,
                )
                .await
            }
            "event/list" => Ok(serde_json::to_value(self.store.events(
                required(&args, "sessionId")?,
                args["after"].as_i64().unwrap_or(0),
                args["limit"].as_u64().unwrap_or(100) as usize,
            )?)?),
            "event/publish" => {
                let kind = required(&args, "kind")?;
                ensure!(
                    matches!(kind, "requirement" | "user_override" | "suggestion"),
                    "user publications must be requirement, user_override, or suggestion"
                );
                Ok(
                    json!({"revision":self.store.publish(required(&args,"sessionId")?,None,None,kind,&args["data"])?}),
                )
            }
            "context/compact" => Ok(
                json!({"revision":self.store.compact(required(&args,"sessionId")?,None,required(&args,"summary")?,args["expectedRevision"].as_i64().ok_or_else(||anyhow::anyhow!("expectedRevision required"))?)?}),
            ),
            "approval/list" => Ok(serde_json::to_value(self.store.approvals()?)?),
            "approval/decide" => {
                self.store.decide(
                    required(&args, "approvalId")?,
                    args["accept"]
                        .as_bool()
                        .ok_or_else(|| anyhow::anyhow!("accept boolean required"))?,
                )?;
                Ok(json!({"saved":true}))
            }
            _ => bail!("unknown method"),
        }
    }
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{key} must be a string"))
}
pub(crate) struct ProcessGroup(pub(crate) Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}
fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({"type":"function","name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}
/// The host commits every workspace after the turn, so a worker that commits on its own spends
/// approvals and turns on work that is already done. The sentence is repeated in every task prompt
/// because the standing instruction alone was not enough for the model this was built against.
pub const COMMIT_RULE: &str =
    "Do not run git commit, git add or git config: the host commits the workspace after your turn.";

pub const DEVELOPER_INSTRUCTIONS: &str = "You are part of a shared coding session. Use collab \
tools for coordination. Worker output is a proposal until reviewed. Never spawn additional agents. \
Never push or modify another task's worktree. Do not run git commit, git add or git config: the \
host commits the workspace after your turn. Complete the assigned goal and report evidence. The \
gateway supplies current shared records before every model request.";

/// How many times a turn may be restarted because delegated work is still unfinished.
///
/// The model this was built against drops the mutating tool call at the end of a turn roughly once
/// per step, so a two-task delegation needs more than two nudges. The budget is paired with the
/// stall guard below: every nudge must change the unfinished set.
pub const MAX_CONTINUATIONS: u32 = 6;
/// Stop after this many nudges in a row that saw the same unfinished set: the model is stuck rather
/// than one step behind, and more turns would only spend tokens.
pub const MAX_STALLED_CONTINUATIONS: u32 = 2;

/// Tasks created by `parent` that still need something: they are queued or running, or they are a
/// finished coding task that was never integrated. Failed, cancelled and blocked tasks count too:
/// the coordinator has to report them rather than let them disappear.
fn unfinished_delegated(store: &Store, session: &str, parent: &str) -> Result<Vec<Task>> {
    Ok(store
        .tasks(session)?
        .into_iter()
        .filter(|t| t.request.created_by.as_deref() == Some(parent))
        .filter(|t| match t.status.as_str() {
            "completed" => t.request.coding && t.integration_status != "integrated",
            _ => true,
        })
        .collect())
}

fn delegated_summary(task: &Task) -> Value {
    json!({
        "taskId": task.id,
        "agentId": task.request.agent_id,
        "status": task.status,
        "integrationStatus": task.integration_status,
        "goal": clip(&task.request.goal, 200),
    })
}

pub fn tools(coordinator: bool) -> Vec<Value> {
    let mut list = vec![
        tool(
            "collab_list_tasks",
            "List session task status, agent IDs, result summaries and commits.",
            json!({}),
            vec![],
        ),
        tool(
            "collab_publish",
            "Publish a suggestion or result. Only coordinator may publish a decision.",
            json!({"kind":{"type":"string","enum":["suggestion","result","decision"]},"data":{"type":"string"}}),
            vec!["kind", "data"],
        ),
        tool(
            "collab_review_diff",
            "Read task diff and exact commit before integration.",
            json!({"taskId":{"type":"string"}}),
            vec!["taskId"],
        ),
    ];
    if coordinator {
        list.extend([
        tool("collab_create_task","Assign a worker task. Coding dependencies wait for integration. Use agent IDs from shared records.",json!({"agentId":{"type":"string"},"goal":{"type":"string"},"acceptance":{"type":"string"},"coding":{"type":"boolean"},"dependencies":{"type":"array","items":{"type":"string"}}}),vec!["agentId","goal","acceptance","coding","dependencies"]),
        tool("collab_wait_task","Wait up to 60 seconds for a different worker task; returns current state.",json!({"taskId":{"type":"string"}}),vec!["taskId"]),
        tool("collab_cancel_task","Cancel a worker task; do not replay uncertain actions.",json!({"taskId":{"type":"string"}}),vec!["taskId"]),
        tool("collab_integrate","After reviewing the exact diff, validate and integrate task into collaboration branch. Conflicts pause for user inspection.",json!({"taskId":{"type":"string"},"expectedCommit":{"type":"string"},"review":{"type":"string"}}),vec!["taskId","expectedCommit","review"]),
        tool("collab_compact","Summarize formal decisions; keeps original audit history and user constraints. Requires current revision.",json!({"summary":{"type":"string"},"expectedRevision":{"type":"integer"}}),vec!["summary","expectedRevision"]),
    ]);
    }
    list
}

struct ApprovalGuard {
    store: Arc<Store>,
    id: String,
}
impl Drop for ApprovalGuard {
    fn drop(&mut self) {
        let _ = self.store.delete_approval(&self.id);
    }
}

struct IntegrationGuard {
    store: Arc<Store>,
    id: String,
    finished: bool,
}
impl Drop for IntegrationGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _=self.store.integration(&self.id,"interrupted",&json!({"message":"integration interrupted; inspect candidate worktree before retrying"}));
        }
    }
}
