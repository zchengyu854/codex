//! Maps the collaboration host's line protocol onto the terminal UI.
//!
//! The client owns the host process, tracks in-flight requests by id, and applies responses and
//! `collab/update` notifications to the [`App`]. It never reads the database or model adapters.
use crate::app::{
    ACCENT, Action, AgentForm, App, ApprovalItem, CompactForm, CreateForm, GREEN, MUTED,
    OutputForm, Pane, PublishForm, ReviewForm, WARN, line, publish_kind_label,
};
use crate::host::{Host, HostEvent};
use ratatui::style::Color;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Events fetched per page when catching up the session revision before compaction.
const EVENT_PAGE: u64 = 500;
/// Upper bound on catch-up pages, so a very active session cannot stall the UI.
const CATCH_UP_PAGES: u32 = 20;
/// How long to wait between `agent/connectionResult` checks while a probe runs.
const PROBE_INTERVAL: Duration = Duration::from_millis(900);
/// Upper bound on probe checks, so a queued probe cannot poll the host forever.
const PROBE_MAX_CHECKS: u32 = 100;
/// How long to wait between `task/output` refreshes while the viewed task runs.
const OUTPUT_INTERVAL: Duration = Duration::from_millis(800);

/// An in-flight capability probe: `agent/testConnection` then `agent/connectionResult` checks.
struct Probe {
    pane: usize,
    task_id: String,
    checks: u32,
    last: Instant,
}

/// An attachment the host captured for a task: its label, local path and size.
struct Artifact {
    label: String,
    path: std::path::PathBuf,
    bytes: u64,
}

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
    TestConnection { pane: usize },
    ConnectionResult,
    Published { kind: &'static str },
    Output { pane: usize, task_id: String },
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
    /// Raw task status per task id, so the client can tell whether a task is still running.
    task_status: HashMap<String, String>,
    /// Attachments captured per task, read locally when reviewing a diff or a result.
    artifacts: HashMap<String, Vec<Artifact>>,
    /// Summary and note to restore when a compaction retries after a revision change.
    compact_retry: Option<(String, String)>,
    /// Capability probe currently being polled, if any.
    probe: Option<Probe>,
    /// Earliest next refresh of an open task output view.
    output_poll: Option<Instant>,
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
            task_status: HashMap::new(),
            artifacts: HashMap::new(),
            compact_retry: None,
            probe: None,
            output_poll: None,
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
        self.tick_probe(app);
        self.tick_output(app);
    }

    /// Refresh an open task output view while its task is still running.
    fn tick_output(&mut self, app: &mut App) {
        let Some((pane, task_id, live)) = app
            .output
            .as_ref()
            .map(|form| (form.pane, form.task_id.clone(), form.live))
        else {
            self.output_poll = None;
            return;
        };
        if !live {
            return;
        }
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::Output { .. }))
        {
            return;
        }
        if self
            .output_poll
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return;
        }
        self.output_poll = Some(Instant::now() + OUTPUT_INTERVAL);
        self.request(
            app,
            "task/output",
            json!({"taskId": task_id.clone()}),
            Pending::Output { pane, task_id },
        );
    }

    /// Poll a running capability probe at a fixed cadence until it reaches a terminal state.
    fn tick_probe(&mut self, app: &mut App) {
        let Some((task_id, checks, ready)) = self.probe.as_ref().map(|probe| {
            (
                probe.task_id.clone(),
                probe.checks,
                probe.checks == 0 || probe.last.elapsed() >= PROBE_INTERVAL,
            )
        }) else {
            return;
        };
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::ConnectionResult))
        {
            return;
        }
        if checks >= PROBE_MAX_CHECKS {
            app.feedback = "连接自检仍在进行（任务可能排队）；可稍后再次自检。".into();
            self.probe = None;
            return;
        }
        if !ready {
            return;
        }
        if let Some(probe) = self.probe.as_mut() {
            probe.checks += 1;
            probe.last = Instant::now();
        }
        self.request(
            app,
            "agent/connectionResult",
            json!({"taskId": task_id}),
            Pending::ConnectionResult,
        );
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
                self.task_status.clear();
                for task in value.as_array().into_iter().flatten() {
                    let (Some(agent), Some(id)) = (task["agentId"].as_str(), task["id"].as_str())
                    else {
                        continue;
                    };
                    self.task_of_agent.insert(agent.to_owned(), id.to_owned());
                    self.task_status.insert(
                        id.to_owned(),
                        task["status"].as_str().unwrap_or("").to_owned(),
                    );
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
            Pending::TestConnection { pane } => match value["id"].as_str() {
                Some(task_id) => {
                    app.push_line(
                        pane,
                        format!("[自检] 已排队连接探针任务 {}", short(task_id)),
                        MUTED,
                    );
                    app.feedback =
                        "连接自检已开始：需要该 Agent 有可用密钥，结果会写入窗格。".into();
                    self.probe = Some(Probe {
                        pane,
                        task_id: task_id.to_owned(),
                        checks: 0,
                        last: Instant::now(),
                    });
                    self.refresh_lists(app);
                }
                None => app.feedback = "连接自检未返回任务 id。".into(),
            },
            Pending::ConnectionResult => {
                let outcome = probe_outcome(&value);
                if let Some(pane) = self.probe.as_ref().map(|probe| probe.pane)
                    && !outcome.message.is_empty()
                {
                    let color = if outcome.verified { GREEN } else { WARN };
                    app.push_line(pane, outcome.message, color);
                }
                app.feedback = outcome.feedback;
                if outcome.terminal {
                    self.probe = None;
                    self.refresh_lists(app);
                }
            }
            Pending::Published { kind } => {
                let revision = value["revision"].as_i64().unwrap_or(0);
                app.feedback = format!(
                    "已发布 {} · 版本 {revision}，正在写入共享记录…",
                    publish_kind_label(kind)
                );
                self.refresh_lists(app);
            }
            Pending::Output { pane, task_id } => self.apply_output(app, pane, &task_id, &value),
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
                    let label = data["label"].as_str().unwrap_or("attachment").to_owned();
                    if let Some(path) = data["path"].as_str() {
                        let entry = self.artifacts.entry(task_id.to_owned()).or_default();
                        entry.retain(|artifact| artifact.label != label);
                        entry.push(Artifact {
                            label: label.clone(),
                            path: std::path::PathBuf::from(path),
                            bytes: data["bytes"].as_u64().unwrap_or(0),
                        });
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
            Action::OpenOutput { pane } => {
                let Some(agent) = app.panes[pane].agent_id.clone() else {
                    app.feedback = "该窗格尚未配置 Agent，没有任务输出。".into();
                    return;
                };
                let Some(task_id) = self.task_of_agent.get(&agent).cloned() else {
                    app.feedback = "该窗格还没有任务输出。".into();
                    return;
                };
                app.feedback = "正在读取任务输出…".into();
                self.request(
                    app,
                    "task/output",
                    json!({"taskId": task_id.clone()}),
                    Pending::Output { pane, task_id },
                );
            }
            Action::RefreshOutput => {
                let Some(form) = app.output.as_ref() else {
                    return;
                };
                let (pane, task_id) = (form.pane, form.task_id.clone());
                app.feedback = "正在刷新任务输出…".into();
                self.request(
                    app,
                    "task/output",
                    json!({"taskId": task_id.clone()}),
                    Pending::Output { pane, task_id },
                );
            }
            Action::OpenPublish => {
                if self.session.is_none() {
                    app.feedback = "尚未连接会话，无法发布用户约束。".into();
                    return;
                }
                app.publish = Some(PublishForm {
                    kind: 0,
                    input: String::new(),
                    note: "←/→ 切换类型。需求与用户覆盖会持续注入每次模型请求并优先于模型决策；建议仅作提示。Enter 发布，Esc 取消。".into(),
                });
            }
            Action::Publish { kind, data } => {
                let Some(session) = self.session.clone() else {
                    app.feedback = "尚未连接会话，无法发布用户约束。".into();
                    return;
                };
                self.request(
                    app,
                    "event/publish",
                    json!({"sessionId": session, "kind": kind, "data": data}),
                    Pending::Published { kind },
                );
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
            Action::TestConnection { pane } => {
                let Some(agent) = app.panes[pane].agent_id.clone() else {
                    app.feedback = "该窗格尚未配置 Agent，无法自检。".into();
                    return;
                };
                app.feedback = "已请求连接自检：会真实调用该 Agent 的模型。".into();
                self.request(
                    app,
                    "agent/testConnection",
                    json!({"agentId": agent}),
                    Pending::TestConnection { pane },
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
        if matches!(pending, Pending::Output { .. }) {
            // Stop refreshing a view whose task the host no longer serves.
            self.output_poll = None;
            if let Some(form) = app.output.as_mut() {
                form.live = false;
            }
            app.feedback = format!("后台错误：{message}");
            return;
        }
        if matches!(pending, Pending::ConnectionResult) {
            // Stop polling a probe the host no longer knows about.
            self.probe = None;
            app.feedback = format!("后台错误：{message}");
            return;
        }
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
        let diff = match self.read_attachment(&task_id, "file diff") {
            body if body.is_empty() => {
                "（尚未捕获差异附件；仍可填写结论后集成，验证由后台执行。）".into()
            }
            body => body,
        };
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

    /// Apply a `task/output` payload: open the view on the first read, refresh it afterwards.
    fn apply_output(&mut self, app: &mut App, pane: usize, task_id: &str, value: &Value) {
        let raw = self
            .task_status
            .get(task_id)
            .map(String::as_str)
            .unwrap_or("");
        let active = matches!(raw, "queued" | "running");
        let previous = app.output.as_ref().filter(|form| form.task_id == task_id);
        // The full-result attachment only appears once the task finishes, so keep the read (and the
        // user's scroll position) from the existing view until it does.
        let body = match previous.map(|form| form.body.clone()) {
            Some(body) if !body.is_empty() => body,
            _ => self.read_attachment(task_id, "full result"),
        };
        let scroll = previous.map(|form| form.scroll).unwrap_or(0);
        app.output = Some(OutputForm {
            pane,
            task_id: task_id.to_owned(),
            status: status_label(raw).into(),
            result: value["result"].as_str().unwrap_or("").to_owned(),
            draft: value["draft"]["text"].as_str().unwrap_or("").to_owned(),
            body,
            artifacts: self.artifact_inventory(task_id),
            scroll,
            live: active,
        });
        if active {
            self.output_poll = Some(Instant::now() + OUTPUT_INTERVAL);
            app.feedback = format!("任务 {} 运行中，输出会自动刷新。", short(task_id));
        } else {
            self.output_poll = None;
            app.feedback = format!(
                "任务 {} 输出已加载：PgUp/PgDn 滚动，Esc 关闭。",
                short(task_id)
            );
        }
    }

    /// The attachment the host captured for a task, if this client has seen its artifact event.
    fn artifact(&self, task_id: &str, label: &str) -> Option<&Artifact> {
        self.artifacts
            .get(task_id)?
            .iter()
            .find(|artifact| artifact.label == label)
    }

    /// Read an attachment read-only; the terminal never writes or moves these files.
    fn read_attachment(&self, task_id: &str, label: &str) -> String {
        self.artifact(task_id, label)
            .and_then(|artifact| std::fs::read_to_string(&artifact.path).ok())
            .map(|text| clip(&text, 100_000))
            .unwrap_or_default()
    }

    /// One line per attachment, so the user can open the full result outside the terminal.
    fn artifact_inventory(&self, task_id: &str) -> String {
        let Some(list) = self.artifacts.get(task_id) else {
            return String::new();
        };
        list.iter()
            .map(|artifact| {
                format!(
                    "{} · {} 字节 · {}",
                    artifact.label,
                    artifact.bytes,
                    clip(&artifact.path.display().to_string(), 120)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
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

/// A summarized `agent/connectionResult` payload.
struct ProbeOutcome {
    terminal: bool,
    verified: bool,
    /// Pane history line; empty while the probe is still running.
    message: String,
    feedback: String,
}

/// Summarize a connection probe payload: only a completed task that actually produced the probe
/// tool result and a streamed usage record counts as verified.
fn probe_outcome(value: &Value) -> ProbeOutcome {
    let status = value["status"].as_str().unwrap_or("");
    let verified = value["verified"].as_bool().unwrap_or(false);
    let capabilities = value["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" / ");
    match status {
        "completed" if verified => ProbeOutcome {
            terminal: true,
            verified: true,
            message: format!("[自检] 通过：{capabilities}"),
            feedback: "连接自检通过：文本 / 流式 / 函数工具均可用。".into(),
        },
        "completed" => ProbeOutcome {
            terminal: true,
            verified: false,
            message: "[自检] 未通过：没有收到探针工具结果或流式用量".into(),
            feedback: "连接自检未通过：模型未按要求调用 collab_publish 或缺少流式用量。".into(),
        },
        "failed" | "cancelled" => ProbeOutcome {
            terminal: true,
            verified: false,
            message: format!("[自检] 未通过（状态 {}）", status_label(status)),
            feedback: "连接自检未通过，详见窗格记录与任务结果。".into(),
        },
        _ => ProbeOutcome {
            terminal: false,
            verified: false,
            message: String::new(),
            feedback: format!(
                "连接自检中（{}）…",
                if status.is_empty() {
                    "等待调度"
                } else {
                    status_label(status)
                }
            ),
        },
    }
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
    fn probe_outcome_reports_progress_and_verdict() {
        let running =
            probe_outcome(&json!({"status":"running","verified":false,"capabilities":[]}));
        assert!(!running.terminal);
        assert!(running.message.is_empty());
        assert!(running.feedback.contains("连接自检中"));

        let verified = probe_outcome(&json!({
            "status":"completed","verified":true,"capabilities":["text","stream","function_tools"]
        }));
        assert!(verified.terminal && verified.verified);
        assert!(verified.message.contains("text / stream / function_tools"));

        let incomplete =
            probe_outcome(&json!({"status":"completed","verified":false,"capabilities":[]}));
        assert!(incomplete.terminal && !incomplete.verified);
        assert!(incomplete.message.contains("未通过"));

        let failed = probe_outcome(&json!({"status":"failed","verified":false}));
        assert!(failed.terminal && !failed.verified);
        assert!(failed.message.contains("失败"));

        let queued = probe_outcome(&json!({"taskId":"t"}));
        assert!(!queued.terminal);
        assert!(queued.feedback.contains("等待调度"));
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
