//! Maps the collaboration host's line protocol onto the terminal UI.
//!
//! The client owns the host process, tracks in-flight requests by id, and applies responses and
//! `collab/update` notifications to the [`App`]. It never reads the database or model adapters.
use crate::app::{
    ACCENT, Action, AgentForm, App, ApprovalItem, CompactForm, CreateForm, MUTED, Pane, ReviewForm,
    WARN, line,
};
use crate::host::{Host, HostEvent};
use ratatui::style::Color;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Events fetched per page when catching up the session revision before compaction.
const EVENT_PAGE: u64 = 500;
/// Upper bound on catch-up pages, so a very active session cannot stall the UI.
const CATCH_UP_PAGES: u32 = 20;

/// Session selection and optional first-run creation.
#[derive(Default)]
pub struct Options {
    pub session: Option<String>,
    pub repository: Option<String>,
    pub validation: Vec<Vec<String>>,
}

enum Pending {
    SessionList,
    CreateSession,
    Agents,
    Tasks,
    Events,
    Approvals,
    Decided,
    CredentialSaved,
    CredentialDeleted,
    AgentConfigured,
    Review { pane: usize, task_id: String },
    Integrated,
    Revision { pages: u32 },
    Compacted { summary: String },
    Sent,
    Cancelled,
}

pub struct Client {
    host: Host,
    options: Options,
    pending: HashMap<u64, Pending>,
    session: Option<String>,
    after: i64,
    /// Latest task id per agent, used for cancellation and status display.
    task_of_agent: HashMap<String, String>,
    /// File-diff attachment path per task, read locally when reviewing.
    diff_artifacts: HashMap<String, std::path::PathBuf>,
    /// Summary and note to restore when a compaction retries after a revision change.
    compact_retry: Option<(String, String)>,
}

impl Client {
    pub fn new(host: Host, options: Options) -> Self {
        Self {
            host,
            options,
            pending: HashMap::new(),
            session: None,
            after: 0,
            task_of_agent: HashMap::new(),
            diff_artifacts: HashMap::new(),
            compact_retry: None,
        }
    }

    /// Attach to the requested session, or discover one from the host.
    pub fn start(&mut self, app: &mut App) {
        app.feedback = format!("已连接后台（{}），正在读取会话…", self.host.label());
        match self.options.session.clone() {
            Some(session) => self.attach(app, session),
            None => self.request(app, "session/list", json!({}), Pending::SessionList),
        }
    }

    fn attach(&mut self, app: &mut App, session: String) {
        app.session_label = format!("会话 {}", short(&session));
        self.after = 0;
        self.task_of_agent.clear();
        self.session = Some(session);
        self.load_agents(app);
    }

    fn load_agents(&mut self, app: &mut App) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.request(
            app,
            "agent/list",
            json!({"sessionId": session}),
            Pending::Agents,
        );
    }

    fn request(&mut self, app: &mut App, method: &str, params: Value, pending: Pending) {
        match self.host.request(method, params) {
            Ok(id) => {
                self.pending.insert(id, pending);
            }
            Err(error) => {
                app.closed = true;
                app.feedback = format!("向后台发送请求失败：{error}");
            }
        }
    }

    /// Drain host messages and pending UI intents. Safe to call every frame.
    pub fn poll(&mut self, app: &mut App) {
        while let Some(event) = self.host.try_recv() {
            match event {
                HostEvent::Update(update) => self.on_update(app, &update),
                HostEvent::Response { id, result } => {
                    let Some(pending) = self.pending.remove(&id) else {
                        continue;
                    };
                    match result {
                        Ok(value) => self.on_result(app, pending, value),
                        Err(message) => self.on_error(app, pending, message),
                    }
                }
                HostEvent::Closed(reason) => {
                    app.closed = true;
                    app.feedback = format!("后台连接断开：{reason}");
                }
            }
        }
        let actions = std::mem::take(&mut app.outbox);
        for action in actions {
            self.on_action(app, action);
        }
    }

    fn on_result(&mut self, app: &mut App, pending: Pending, value: Value) {
        match pending {
            Pending::SessionList => {
                let sessions = value.as_array().cloned().unwrap_or_default();
                if sessions.is_empty() {
                    self.create_session(app);
                    return;
                }
                match sessions[0]["id"].as_str() {
                    Some(id) => self.attach(app, id.to_owned()),
                    None => app.feedback = "会话列表格式异常，缺少 id。".into(),
                }
            }
            Pending::CreateSession => match value["id"].as_str() {
                Some(id) => {
                    app.feedback = format!("已创建会话 {}。", short(id));
                    self.attach(app, id.to_owned());
                }
                None => app.feedback = "创建会话未返回 id。".into(),
            },
            Pending::Agents => {
                let agents = value.as_array().cloned().unwrap_or_default();
                if agents.is_empty() {
                    app.feedback = "该会话尚未配置 Agent。".into();
                }
                apply_agents(app, &agents);
                self.refresh_lists(app);
            }
            Pending::Tasks => {
                self.task_of_agent.clear();
                for task in value.as_array().into_iter().flatten() {
                    let (Some(agent), Some(id)) = (task["agentId"].as_str(), task["id"].as_str())
                    else {
                        continue;
                    };
                    self.task_of_agent.insert(agent.to_owned(), id.to_owned());
                    if let Some(index) = app.pane_of(agent) {
                        let status = status_label(task["status"].as_str().unwrap_or(""));
                        app.panes[index].status = status.into();
                    }
                }
            }
            Pending::Events => {
                for event in value.as_array().into_iter().flatten() {
                    self.on_event(app, event);
                }
            }
            Pending::Approvals => {
                apply_approvals(app, &value);
            }
            Pending::Decided => {
                app.feedback = "审批决定已提交。".into();
                self.request(app, "approval/list", json!({}), Pending::Approvals);
            }
            Pending::CredentialSaved => {
                app.feedback = "密钥已保存到后台凭证存储。".into();
            }
            Pending::CredentialDeleted => {
                app.feedback = "密钥删除请求已受理。".into();
            }
            Pending::AgentConfigured => {
                app.feedback = "已配置 Agent，正在刷新窗格…".into();
                self.load_agents(app);
            }
            Pending::Review { pane, task_id } => {
                self.open_review(app, pane, task_id, &value);
            }
            Pending::Integrated => {
                let status = value["status"].as_str().unwrap_or("failed");
                app.feedback = match status {
                    "integrated" => "已集成到协作分支。".into(),
                    "conflict" => "存在冲突，集成工作区已保留，请人工核查。".into(),
                    "failed" => "验证失败，候选提交已保留。".into(),
                    "interrupted" => "集成被中断，请核查集成工作区。".into(),
                    _ => "集成未完成。".into(),
                };
                self.refresh_lists(app);
            }
            Pending::Revision { pages } => {
                let events = value.as_array().cloned().unwrap_or_default();
                for event in &events {
                    self.on_event(app, event);
                }
                if events.len() as u64 >= EVENT_PAGE && pages > 1 {
                    self.probe_revision(app, pages - 1);
                } else {
                    let (input, note) = self.compact_retry.take().unwrap_or_default();
                    open_compact_form(app, self.after, input, note);
                }
            }
            Pending::Compacted { .. } => {
                let revision = value["revision"].as_i64().unwrap_or(self.after);
                self.after = self.after.max(revision);
                app.feedback = format!("已压缩上下文，检查点版本 {revision}。");
                self.refresh_lists(app);
            }
            Pending::Sent => {
                let status = status_label(value["status"].as_str().unwrap_or("queued"));
                app.feedback = format!("任务已提交（{status}）。");
                self.refresh_lists(app);
            }
            Pending::Cancelled => {
                app.feedback = "取消请求已受理；受影响的工作副本需人工核查。".into();
                self.refresh_lists(app);
            }
        }
    }

    fn create_session(&mut self, app: &mut App) {
        let Some(repository) = self.options.repository.clone() else {
            app.feedback = "尚无协作会话，请填写表单创建。".into();
            open_create_form(app, &self.options, "尚无协作会话，填写后创建。".into());
            return;
        };
        if self.options.validation.is_empty() {
            app.feedback = "创建会话需要至少一条验证命令，请填写表单。".into();
            open_create_form(
                app,
                &self.options,
                "需要至少一条验证命令（每行一条）。".into(),
            );
            return;
        }
        app.feedback = "未发现会话，正在按给定仓库创建…".into();
        self.request(
            app,
            "session/create",
            json!({"repository": repository, "validation": self.options.validation}),
            Pending::CreateSession,
        );
    }

    fn refresh_lists(&mut self, app: &mut App) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.request(
            app,
            "task/list",
            json!({"sessionId": session}),
            Pending::Tasks,
        );
        self.request(
            app,
            "event/list",
            json!({"sessionId": session, "after": self.after, "limit": EVENT_PAGE}),
            Pending::Events,
        );
        self.request(app, "approval/list", json!({}), Pending::Approvals);
    }

    /// Walk forward until the session revision is fully caught up, then open the compaction form.
    fn probe_revision(&mut self, app: &mut App, pages: u32) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.request(
            app,
            "event/list",
            json!({"sessionId": session, "after": self.after, "limit": EVENT_PAGE}),
            Pending::Revision {
                pages: pages.min(CATCH_UP_PAGES),
            },
        );
    }

    fn on_event(&mut self, app: &mut App, event: &Value) {
        if let Some(revision) = event["revision"].as_i64() {
            self.after = self.after.max(revision);
        }
        let kind = event["kind"].as_str().unwrap_or("");
        let agent = event["agentId"].as_str();
        let data = &event["data"];
        match kind {
            "public_message" => {
                if let Some(index) = agent.and_then(|a| app.pane_of(a)) {
                    let text = data["text"].as_str().unwrap_or("");
                    let role = app.panes[index].role.clone();
                    app.push_line(index, format!("{role} › {text}"), Color::Reset);
                    app.panes[index].draft.clear();
                }
            }
            "task_finished" => {
                if let Some(index) = agent.and_then(|a| app.pane_of(a)) {
                    let status = status_label(data["status"].as_str().unwrap_or(""));
                    let summary = event_text(data);
                    app.push_line(index, format!("[任务] {status} {summary}"), MUTED);
                }
            }
            "integration" => {
                if let Some(index) = agent.and_then(|a| app.pane_of(a)) {
                    let status = data["status"].as_str().unwrap_or("?");
                    app.push_line(index, format!("[集成] {status}"), WARN);
                }
            }
            "artifact" => {
                if let (Some(index), Some(task_id)) =
                    (agent.and_then(|a| app.pane_of(a)), event["taskId"].as_str())
                {
                    let label = data["label"].as_str().unwrap_or("attachment");
                    if label == "file diff"
                        && let Some(path) = data["path"].as_str()
                    {
                        self.diff_artifacts
                            .insert(task_id.to_owned(), std::path::PathBuf::from(path));
                    }
                    app.push_line(index, format!("[产物] {label}"), MUTED);
                }
            }
            "approval_resolved" => {
                if let Some(index) = agent.and_then(|a| app.pane_of(a)) {
                    let verb = if data["accepted"].as_bool().unwrap_or(false) {
                        "已批准"
                    } else {
                        "已拒绝"
                    };
                    app.push_line(index, format!("[审批] {verb}"), WARN);
                }
            }
            "requirement" | "user_override" => {
                push_shared(app, format!("[用户] {}", event_text(data)), ACCENT);
            }
            "decision" => {
                push_shared(app, format!("[决策] {}", event_text(data)), ACCENT);
            }
            "suggestion" => {
                push_shared(app, format!("[建议] {}", event_text(data)), MUTED);
            }
            "checkpoint" => {
                push_shared(app, format!("[摘要] {}", event_text(data)), MUTED);
            }
            _ => {}
        }
    }

    fn on_update(&mut self, app: &mut App, update: &Value) {
        match update["kind"].as_str().unwrap_or("") {
            "resync_required" => {
                app.feedback = "后台通知缓冲溢出，正在全量补读…".into();
                app.shared_records.clear();
                self.after = 0;
                self.load_agents(app);
            }
            "agent_delta" => {
                let (Some(agent), Some(delta)) =
                    (update["agentId"].as_str(), update["delta"].as_str())
                else {
                    return;
                };
                if let Some(index) = app.pane_of(agent) {
                    let draft = &mut app.panes[index].draft;
                    if draft.len() + delta.len() <= 4_000_000 {
                        draft.push_str(delta);
                    }
                    app.panes[index].status = "输出中".into();
                }
            }
            "task_changed" => {
                self.refresh_lists(app);
            }
            "approval_requested" => {
                app.feedback = "有新的操作审批待处理。".into();
                self.request(app, "approval/list", json!({}), Pending::Approvals);
            }
            _ => {}
        }
    }

    fn on_action(&mut self, app: &mut App, action: Action) {
        match action {
            Action::Send { pane, text, coding } => {
                let Some(session) = self.session.clone() else {
                    app.feedback = "尚未连接会话，无法派发任务。".into();
                    return;
                };
                let Some(agent) = app.panes[pane].agent_id.clone() else {
                    app.feedback = "该窗格尚未配置 Agent。".into();
                    return;
                };
                self.request(
                    app,
                    "task/create",
                    json!({
                        "sessionId": session,
                        "agentId": agent,
                        "goal": text,
                        "acceptance": "",
                        "dependencies": [],
                        "coding": coding
                    }),
                    Pending::Sent,
                );
            }
            Action::Cancel { pane } => {
                let Some(agent) = app.panes[pane].agent_id.clone() else {
                    return;
                };
                let Some(task) = self.task_of_agent.get(&agent).cloned() else {
                    app.feedback = "该窗格当前没有可取消的任务。".into();
                    return;
                };
                self.request(
                    app,
                    "task/cancel",
                    json!({"taskId": task}),
                    Pending::Cancelled,
                );
            }
            Action::Refresh => {
                app.feedback = "已请求刷新。".into();
                if self.session.is_none() {
                    self.request(app, "session/list", json!({}), Pending::SessionList);
                } else {
                    self.load_agents(app);
                }
            }
            Action::Decide { id, accept } => {
                self.request(
                    app,
                    "approval/decide",
                    json!({"approvalId": id, "accept": accept}),
                    Pending::Decided,
                );
            }
            Action::SaveCredential { pane, key } => {
                let Some(agent) = app.panes[pane].agent_id.clone() else {
                    return;
                };
                self.request(
                    app,
                    "agent/setCredential",
                    json!({"agentId": agent, "key": key}),
                    Pending::CredentialSaved,
                );
            }
            Action::DeleteCredential { pane } => {
                let Some(agent) = app.panes[pane].agent_id.clone() else {
                    return;
                };
                self.request(
                    app,
                    "agent/deleteCredential",
                    json!({"agentId": agent}),
                    Pending::CredentialDeleted,
                );
            }
            Action::OpenReview { pane } => {
                let Some(agent) = app.panes[pane].agent_id.clone() else {
                    return;
                };
                let Some(task_id) = self.task_of_agent.get(&agent).cloned() else {
                    app.feedback = "该窗格还没有可审查的任务。".into();
                    return;
                };
                self.request(
                    app,
                    "task/get",
                    json!({"taskId": task_id.clone()}),
                    Pending::Review { pane, task_id },
                );
            }
            Action::Integrate {
                task_id,
                expected_commit,
                review,
            } => {
                self.request(
                    app,
                    "task/integrate",
                    json!({"taskId": task_id, "expectedCommit": expected_commit, "review": review}),
                    Pending::Integrated,
                );
            }
            Action::OpenCompact => {
                if self.session.is_none() {
                    app.feedback = "尚未连接会话，无法压缩上下文。".into();
                    return;
                }
                self.compact_retry = None;
                app.feedback = "正在读取会话版本…".into();
                self.probe_revision(app, CATCH_UP_PAGES);
            }
            Action::Compact {
                summary,
                expected_revision,
            } => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.request(
                    app,
                    "context/compact",
                    json!({"sessionId": session, "summary": summary, "expectedRevision": expected_revision}),
                    Pending::Compacted { summary },
                );
            }
            Action::OpenCreate => {
                let note = if self.options.repository.is_some() {
                    "已按启动参数预填，可修改。Tab 切换字段，Enter 创建。".into()
                } else {
                    "填写仓库绝对路径与验证命令（每行一条），Tab 切换，Enter 创建。".into()
                };
                open_create_form(app, &self.options, note);
            }
            Action::OpenAgent { pane } => {
                app.agent = Some(build_agent_form(app, pane));
            }
            Action::ConfigureAgent {
                pane,
                id,
                role,
                model,
                base_url,
                protocol,
                context_bytes,
            } => {
                let Some(session) = self.session.clone() else {
                    app.feedback = "尚未连接会话，无法配置 Agent。".into();
                    return;
                };
                app.feedback = format!("正在配置窗格 {} 的 Agent…", pane + 1);
                self.request(
                    app,
                    "agent/configure",
                    json!({
                        "id": id,
                        "sessionId": session,
                        "role": role,
                        "model": model,
                        "baseUrl": base_url,
                        "protocol": protocol,
                        "contextBytes": context_bytes,
                    }),
                    Pending::AgentConfigured,
                );
            }
            Action::CreateSession {
                repository,
                validation,
                requirement,
            } => {
                app.feedback = "正在创建协作会话…".into();
                self.request(
                    app,
                    "session/create",
                    json!({"repository": repository, "validation": validation, "requirement": requirement}),
                    Pending::CreateSession,
                );
            }
        }
    }

    /// Surface a host error; a compaction revision clash refreshes and reopens the form.
    fn on_error(&mut self, app: &mut App, pending: Pending, message: String) {
        if let Pending::Compacted { summary } = pending
            && is_revision_conflict(&message)
        {
            app.feedback = "上下文在提交前发生变化，已刷新版本，请重新确认摘要。".into();
            self.compact_retry = Some((summary, "版本已刷新，请核对后再次提交。".into()));
            self.probe_revision(app, CATCH_UP_PAGES);
            return;
        }
        app.feedback = format!("后台错误：{message}");
    }

    /// Build the review form from a task snapshot, reading the captured diff attachment locally.
    fn open_review(&self, app: &mut App, pane: usize, task_id: String, task: &Value) {
        let diff = self
            .diff_artifacts
            .get(&task_id)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| clip(&text, 100_000))
            .unwrap_or_else(|| "（尚未捕获差异附件；仍可填写结论后集成，验证由后台执行。）".into());
        match build_review(pane, task_id.clone(), task, diff) {
            Ok(form) => {
                app.review = Some(form);
                app.feedback = format!(
                    "审查任务 {}：核对差异后填写结论，Enter 集成。",
                    short(&task_id)
                );
            }
            Err(message) => app.feedback = message,
        }
    }
}

/// Open the compaction form with the probed revision, summary and any note.
fn open_compact_form(app: &mut App, revision: i64, input: String, note: String) {
    app.compact = Some(CompactForm {
        revision,
        input,
        note,
    });
    app.feedback =
        format!("会话版本 {revision}：填写摘要后 Enter 压缩上下文（保留审计与用户约束）。");
}

/// Open the interactive session creation form, prefilled from CLI options when present.
fn open_create_form(app: &mut App, options: &Options, note: String) {
    let validation = options
        .validation
        .iter()
        .map(|args| args.join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    app.create = Some(CreateForm {
        repository: options.repository.clone().unwrap_or_default(),
        validation,
        requirement: String::new(),
        field: 0,
        note,
    });
}

/// The host reports a revision clash this way; treat it as a soft retry.
fn is_revision_conflict(message: &str) -> bool {
    message.contains("refresh before compacting")
}

/// Build the Agent form for a pane: editing an existing agent reuses its id and role, while an
/// empty pane gets a client-generated UUID whose role follows the pane position.
fn build_agent_form(app: &App, pane: usize) -> AgentForm {
    let p = &app.panes[pane];
    let (id, model, base_url, protocol, context_bytes) = match p.agent_id.clone() {
        Some(id) => (
            id,
            p.model.clone(),
            if p.api == "—" {
                String::new()
            } else {
                p.api.clone()
            },
            protocol_value(&p.protocol).to_owned(),
            p.context_bytes
                .map(|bytes| bytes.to_string())
                .unwrap_or_default(),
        ),
        None => (
            uuid::Uuid::new_v4().to_string(),
            String::new(),
            String::new(),
            "responses".to_owned(),
            "65536".to_owned(),
        ),
    };
    AgentForm {
        pane,
        id,
        role: if pane == 0 { "coordinator" } else { "worker" },
        model,
        base_url,
        protocol,
        context_bytes,
        field: 0,
        note: if p.agent_id.is_some() {
            "修改当前 Agent；运行中的任务需先停止。".into()
        } else {
            format!("为窗格 {} 新增 Agent（角色由位置决定）。", pane + 1)
        },
    }
}

/// Map a display label back to the wire protocol value.
fn protocol_value(label: &str) -> &'static str {
    match label {
        "Chat Completions" => "chat_completions",
        _ => "responses",
    }
}

/// Validate a task snapshot for integration and build its review form.
fn build_review(
    pane: usize,
    task_id: String,
    task: &Value,
    diff: String,
) -> Result<ReviewForm, String> {
    let status = task["status"].as_str().unwrap_or("");
    if status != "completed" {
        return Err(format!(
            "任务状态为{}，需完成后才能审查。",
            status_label(status)
        ));
    }
    if !task["coding"].as_bool().unwrap_or(false) {
        return Err("只有编码任务可以审查并集成。".into());
    }
    let integration = task["integrationStatus"]
        .as_str()
        .unwrap_or("pending")
        .to_owned();
    if integration == "integrated" {
        return Err("该任务已集成。".into());
    }
    let commit = task["commit"].as_str().unwrap_or("").to_owned();
    if commit.is_empty() {
        return Err("任务没有可审查的提交。".into());
    }
    Ok(ReviewForm {
        pane,
        task_id,
        commit,
        status: status.into(),
        integration,
        summary: task["result"].as_str().unwrap_or("").to_owned(),
        diff,
        input: String::new(),
        scroll: 0,
    })
}

fn short(id: &str) -> String {
    let mut end = id.len().min(8);
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &id[..end])
}

/// Replace the pending approvals with the host's current list.
fn apply_approvals(app: &mut App, value: &Value) {
    app.approvals = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|approval| {
            Some(ApprovalItem {
                id: approval["id"].as_str()?.to_owned(),
                method: approval["method"].as_str().unwrap_or("").to_owned(),
                agent_id: approval["agentId"].as_str().unwrap_or("").to_owned(),
                task_id: approval["taskId"].as_str().unwrap_or("").to_owned(),
                detail: approval_detail(&approval["params"]),
            })
        })
        .collect();
    if app.approval_index >= app.approvals.len() {
        app.approval_index = 0;
    }
}

/// Human-readable summary of an approval's parameters; never includes the credential.
fn approval_detail(params: &Value) -> String {
    let text = if let Some(command) = params["command"].as_array() {
        command
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    } else if let Some(reason) = params["reason"].as_str() {
        reason.to_owned()
    } else if params.is_null() {
        String::new()
    } else {
        params.to_string()
    };
    clip(&text, 400)
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Fill the four panes from a session's agents: one coordinator, up to three workers.
fn apply_agents(app: &mut App, agents: &[Value]) {
    app.panes = [
        Pane::placeholder("主控"),
        Pane::placeholder("工作1"),
        Pane::placeholder("工作2"),
        Pane::placeholder("工作3"),
    ];
    let mut worker = 0usize;
    for agent in agents {
        let coordinator = agent["role"].as_str() == Some("coordinator");
        let index = if coordinator {
            0
        } else {
            let index = 1 + worker;
            worker += 1;
            index
        };
        if index > 3 {
            continue;
        }
        let pane = &mut app.panes[index];
        pane.agent_id = agent["id"].as_str().map(str::to_owned);
        pane.role = if coordinator {
            "主控".into()
        } else {
            format!("工作{worker}")
        };
        pane.model = agent["model"].as_str().unwrap_or("—").into();
        pane.api = agent["baseUrl"].as_str().unwrap_or("—").into();
        pane.protocol = protocol_label(agent["protocol"].as_str().unwrap_or("")).into();
        pane.context_bytes = agent["contextBytes"].as_u64().map(|bytes| bytes as usize);
        pane.status = "空闲".into();
        pane.history
            .push(line(format!("$ codex --model {}", pane.model), MUTED));
    }
}

fn push_shared(app: &mut App, text: impl Into<String>, color: Color) {
    app.shared_records.push(line(text, color));
    if app.shared_records.len() > 200 {
        let excess = app.shared_records.len() - 200;
        app.shared_records.drain(..excess);
    }
}

fn event_text(data: &Value) -> String {
    if let Some(text) = data.as_str() {
        return text.to_owned();
    }
    for key in ["text", "summary", "message", "reason"] {
        if let Some(text) = data[key].as_str() {
            return text.to_owned();
        }
    }
    data.to_string()
}

fn status_label(status: &str) -> &'static str {
    match status {
        "queued" => "排队",
        "running" => "工作中",
        "completed" => "已完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        "interrupted" => "已中断",
        "blocked" => "等待依赖",
        _ => "空闲",
    }
}

fn protocol_label(protocol: &str) -> &'static str {
    match protocol {
        "chat_completions" => "Chat Completions",
        "responses" => "Responses",
        _ => "—",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    #[test]
    fn agents_map_to_one_coordinator_and_three_workers() {
        let mut app = App::live();
        let agents = vec![
            json!({"id":"c","role":"coordinator","model":"planner","baseUrl":"https://a","protocol":"responses"}),
            json!({"id":"w1","role":"worker","model":"code","baseUrl":"https://b","protocol":"chat_completions"}),
            json!({"id":"w2","role":"worker","model":"docs","baseUrl":"https://c","protocol":"responses"}),
            json!({"id":"w3","role":"worker","model":"extra","baseUrl":"https://d","protocol":"responses"}),
            json!({"id":"w4","role":"worker","model":"ignored","baseUrl":"https://e","protocol":"responses"}),
        ];
        apply_agents(&mut app, &agents);
        assert_eq!(app.panes[0].role, "主控");
        assert_eq!(app.panes[0].model, "planner");
        assert_eq!(app.panes[1].model, "code");
        assert_eq!(app.panes[1].protocol, "Chat Completions");
        assert_eq!(app.panes[3].model, "extra");
        assert_eq!(app.panes[3].agent_id.as_deref(), Some("w3"));
        // The fifth agent has no pane.
        assert_eq!(app.pane_of("w4"), None);
    }

    #[test]
    fn status_and_protocol_labels_are_bounded() {
        assert_eq!(status_label("running"), "工作中");
        assert_eq!(status_label("mystery"), "空闲");
        assert_eq!(protocol_label("chat_completions"), "Chat Completions");
        assert_eq!(protocol_label("x"), "—");
    }

    #[test]
    fn event_text_prefers_known_fields_and_falls_back_to_json() {
        assert_eq!(event_text(&json!("plain")), "plain");
        assert_eq!(event_text(&json!({"text":"hi"})), "hi");
        assert_eq!(event_text(&json!({"summary":"sum"})), "sum");
        assert_eq!(event_text(&json!({"other":1})), "{\"other\":1}");
    }

    #[test]
    fn short_id_is_utf8_safe_for_ascii_uuids() {
        assert_eq!(short("1234567890"), "12345678…");
        assert_eq!(short("abc"), "abc…");
    }

    #[test]
    fn approvals_apply_and_clamp_index() {
        let mut app = App::live();
        app.approval_index = 5;
        apply_approvals(
            &mut app,
            &json!([
                {"id":"a1","method":"item/commandExecution/requestApproval","agentId":"x","taskId":"t","params":{"command":["cargo","test","--all"]}},
                {"id":"a2","method":"item/fileChange/requestApproval","agentId":"y","taskId":"u","params":{"reason":"edit"}}
            ]),
        );
        assert_eq!(app.approvals.len(), 2);
        assert_eq!(app.approvals[0].detail, "cargo test --all");
        assert_eq!(app.approvals[1].detail, "edit");
        assert_eq!(app.approval_index, 0);
    }

    #[test]
    fn approval_detail_handles_missing_and_long_values() {
        assert_eq!(approval_detail(&json!(null)), "");
        assert_eq!(approval_detail(&json!({"other":true})), "{\"other\":true}");
        let clipped = approval_detail(&json!({"reason": "中".repeat(500)}));
        assert!(clipped.ends_with('…'));
        assert!(clipped.len() < 500 * 3);
    }

    #[test]
    fn build_review_requires_a_completed_coding_task() {
        let running = json!({"status":"running","coding":true,"commit":"c"});
        assert!(build_review(0, "t".into(), &running, String::new()).is_err());

        let non_coding = json!({"status":"completed","coding":false,"commit":"c"});
        assert!(build_review(0, "t".into(), &non_coding, String::new()).is_err());

        let integrated = json!({"status":"completed","coding":true,"commit":"c","integrationStatus":"integrated"});
        assert!(build_review(0, "t".into(), &integrated, String::new()).is_err());

        let no_commit = json!({"status":"completed","coding":true,"commit":null});
        assert!(build_review(0, "t".into(), &no_commit, String::new()).is_err());

        let ready = json!({
            "status":"completed","coding":true,"commit":"abc123",
            "integrationStatus":"pending","result":"summary"
        });
        let form = build_review(2, "task-1".into(), &ready, "diff --git".into()).unwrap();
        assert_eq!(form.pane, 2);
        assert_eq!(form.commit, "abc123");
        assert_eq!(form.summary, "summary");
        assert_eq!(form.diff, "diff --git");
    }

    #[test]
    fn revision_conflict_is_classified() {
        assert!(is_revision_conflict(
            "context changed; refresh before compacting"
        ));
        assert!(!is_revision_conflict("summary must contain 1..8192 bytes"));
    }

    #[test]
    fn create_form_prefills_from_options() {
        let options = Options {
            session: None,
            repository: Some("/tmp/repo".into()),
            validation: vec![
                vec!["cargo".into(), "test".into()],
                vec!["cargo".into(), "clippy".into()],
            ],
        };
        let mut app = App::live();
        open_create_form(&mut app, &options, "note".into());
        let form = app.create.unwrap();
        assert_eq!(form.repository, "/tmp/repo");
        assert_eq!(form.validation, "cargo test\ncargo clippy");
        assert_eq!(form.field, 0);
    }

    #[test]
    fn agent_form_generates_a_uuid_for_an_empty_pane() {
        let mut app = App::live();
        app.panes[1].model = "worker-model".into();
        let form = build_agent_form(&app, 1);
        assert_eq!(form.role, "worker");
        assert_eq!(form.pane, 1);
        assert!(
            uuid::Uuid::parse_str(&form.id).is_ok(),
            "a new agent needs a UUID the host accepts"
        );
        assert_eq!(form.model, "");
        assert_eq!(form.protocol, "responses");
        assert_eq!(form.context_bytes, "65536");
    }

    #[test]
    fn agent_form_prefills_an_existing_agent_and_pane_zero_is_the_coordinator() {
        let mut app = App::live();
        let agents = vec![json!({
            "id":"22222222-2222-4222-8222-222222222222","role":"worker","model":"code",
            "baseUrl":"https://b","protocol":"chat_completions","contextBytes":32768
        })];
        apply_agents(&mut app, &agents);
        let form = build_agent_form(&app, 1);
        assert_eq!(form.id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(form.role, "worker");
        assert_eq!(form.model, "code");
        assert_eq!(form.base_url, "https://b");
        assert_eq!(form.protocol, "chat_completions");
        assert_eq!(form.context_bytes, "32768");
        assert_eq!(app.panes[1].context_bytes, Some(32768));
        assert_eq!(build_agent_form(&app, 0).role, "coordinator");
    }

    #[test]
    fn compact_form_reopens_with_summary_and_note() {
        let mut app = App::live();
        open_compact_form(&mut app, 9, "keep".into(), "retry".into());
        let form = app.compact.unwrap();
        assert_eq!(form.revision, 9);
        assert_eq!(form.input, "keep");
        assert_eq!(form.note, "retry");
    }
}
