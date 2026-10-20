use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Line,
};

pub const ACCENT: Color = Color::Rgb(145, 178, 206);
pub const MUTED: Color = Color::Rgb(141, 155, 173);
pub const GREEN: Color = Color::Rgb(152, 188, 166);
pub const WARN: Color = Color::Rgb(214, 178, 132);

/// Build a single styled history line.
pub fn line(text: impl Into<String>, color: Color) -> Line<'static> {
    Line::styled(text.into(), Style::default().fg(color))
}

/// Shorten an id for display without splitting a UTF-8 sequence.
pub fn short_id(id: &str) -> String {
    let mut end = id.len().min(8);
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &id[..end])
}

/// The text field currently focused in the session creation form.
fn field_mut(form: &mut CreateForm) -> &mut String {
    match form.field {
        0 => &mut form.repository,
        1 => &mut form.validation,
        _ => &mut form.requirement,
    }
}

/// Parse one validation command per non-empty line into argument arrays.
fn parse_validation(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.split_whitespace().map(str::to_owned).collect())
        .filter(|args: &Vec<String>| !args.is_empty())
        .collect()
}

/// A text buffer with a byte-offset caret that always sits on a UTF-8 boundary.
///
/// The pane input line and the task editor both edit text at a caret, so the arithmetic lives in
/// one place. `\n` is ordinary content: line-level Home/End and newline insertion work for both the
/// single-line pane input and the multi-line task editor.
#[derive(Clone, Default, Debug)]
pub struct TextBuf {
    pub text: String,
    pub cursor: usize,
}

impl TextBuf {
    /// A buffer whose caret starts at the end, like a shell prompt.
    pub fn end(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self { text, cursor }
    }

    /// Replace the contents and put the caret at the end.
    pub fn set(&mut self, text: impl Into<String>) {
        *self = Self::end(text);
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Clamp the caret into the text, so public field writes can never desync the two.
    pub fn sync_cursor(&mut self) {
        self.cursor = self.cursor.min(self.text.len());
        while !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    /// Insert one character at the caret; returns false once the byte cap is reached.
    pub fn insert_char(&mut self, character: char, limit: usize) -> bool {
        if self.text.len() + character.len_utf8() > limit {
            return false;
        }
        self.sync_cursor();
        self.text.insert(self.cursor, character);
        self.cursor += character.len_utf8();
        true
    }

    /// Move the caret one character left.
    pub fn move_left(&mut self) {
        self.sync_cursor();
        if let Some((index, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = index;
        }
    }

    /// Move the caret one character right.
    pub fn move_right(&mut self) {
        self.sync_cursor();
        if let Some(character) = self.text[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
    }

    /// Move the caret to the start of the caret's line.
    pub fn move_line_start(&mut self) {
        self.sync_cursor();
        let start = self.text[..self.cursor].rfind('\n').map_or(0, |at| at + 1);
        self.cursor = start;
    }

    /// Move the caret to the end of the caret's line.
    pub fn move_line_end(&mut self) {
        self.sync_cursor();
        let end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |at| self.cursor + at);
        self.cursor = end;
    }

    /// Delete the character before the caret.
    pub fn backspace(&mut self) {
        self.sync_cursor();
        if let Some((index, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.remove(index);
            self.cursor = index;
        }
    }

    /// Delete the character under the caret.
    pub fn delete(&mut self) {
        self.sync_cursor();
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }
}

impl PartialEq<&str> for TextBuf {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

/// Where the pane data comes from: built-in demonstration data, or the live collaboration host.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Demo,
    Live,
}

/// One terminal pane backed by a configured agent (live mode) or example data (demo mode).
pub struct Pane {
    pub agent_id: Option<String>,
    pub role: String,
    pub model: String,
    pub api: String,
    pub protocol: String,
    pub status: String,
    /// Configured UTF-8 request byte budget (`contextBytes`), when the pane has an agent.
    pub context_bytes: Option<usize>,
    pub history: Vec<Line<'static>>,
    pub input: TextBuf,
    pub scroll: usize,
    pub stopped: bool,
    /// Public text streamed for the running task and not yet finalized by a public message.
    pub draft: String,
    /// Lines dispatched from this pane, newest last; recalled with Up/Down.
    sent: Vec<String>,
    /// Position within `sent` during a recall, plus the draft stashed when the recall started.
    recall: Option<(usize, TextBuf)>,
}

impl Pane {
    pub fn placeholder(role: &str) -> Self {
        Self {
            agent_id: None,
            role: role.into(),
            model: "—".into(),
            api: "—".into(),
            protocol: "—".into(),
            status: "未配置".into(),
            context_bytes: None,
            history: Vec::new(),
            input: TextBuf::default(),
            scroll: 0,
            stopped: false,
            draft: String::new(),
            sent: Vec::new(),
            recall: None,
        }
    }

    /// Remember a dispatched line so Up/Down can recall it, skipping immediate repeats.
    fn remember(&mut self, text: &str) {
        if self.sent.last().map(String::as_str) == Some(text) {
            return;
        }
        self.sent.push(text.to_owned());
        // shortcut: 每个窗格只保留最近 100 条输入历史，够用且不会无界增长。
        if self.sent.len() > 100 {
            self.sent.remove(0);
        }
    }

    /// Recall an older dispatched line, stashing the in-progress draft on the first step.
    fn recall_previous(&mut self) {
        if self.sent.is_empty() {
            return;
        }
        let index = match self.recall.take() {
            None => {
                let index = self.sent.len() - 1;
                self.recall = Some((index, self.input.clone()));
                index
            }
            Some((0, stash)) => {
                self.recall = Some((0, stash));
                return;
            }
            Some((index, stash)) => {
                let previous = index - 1;
                self.recall = Some((previous, stash));
                previous
            }
        };
        self.input.set(self.sent[index].clone());
    }

    /// Walk forward through the recalled lines, restoring the stashed draft past the newest.
    fn recall_next(&mut self) {
        let Some((index, stash)) = self.recall.take() else {
            return;
        };
        if index + 1 < self.sent.len() {
            self.recall = Some((index + 1, stash));
            self.input.set(self.sent[index + 1].clone());
        } else {
            self.input = stash;
        }
    }
}

/// A pending operation approval surfaced from the host.
#[derive(Clone, Debug)]
pub struct ApprovalItem {
    pub id: String,
    pub method: String,
    pub agent_id: String,
    pub task_id: String,
    pub detail: String,
}

/// The masked credential entry form for one agent.
pub struct CredentialForm {
    pub pane: usize,
    pub input: String,
}

/// One `task/list` entry offered as a dependency in the task editor.
///
/// The host accepts any task id of the session, but a dependency that already failed can never
/// complete, so the editor marks those as unusable instead of letting them hard-block the task.
pub struct Dependency {
    pub id: String,
    /// Display label: pane role, progress and a goal excerpt.
    pub label: String,
    /// Whether selecting it can still be satisfied.
    pub selectable: bool,
    pub note: String,
    pub selected: bool,
}

/// Task editor (`task/create`): goal, acceptance criteria, dependencies and the coding switch.
///
/// The plain Enter dispatch on a pane keeps sending an empty acceptance and no dependencies; this
/// editor is the path that fills the parameters the host already supports.
pub struct TaskForm {
    pub pane: usize,
    pub goal: TextBuf,
    pub acceptance: TextBuf,
    pub dependencies: Vec<Dependency>,
    /// Highlighted dependency row.
    pub index: usize,
    pub coding: bool,
    /// Focused field: 0 goal, 1 acceptance, 2 dependencies, 3 coding.
    pub field: usize,
    pub note: String,
}

impl TaskForm {
    /// The focused text field, if the focus is on one.
    fn text_field(&mut self) -> Option<&mut TextBuf> {
        match self.field {
            0 => Some(&mut self.goal),
            1 => Some(&mut self.acceptance),
            _ => None,
        }
    }

    /// The selected, still satisfiable dependencies, in list order.
    pub fn selected(&self) -> Vec<String> {
        self.dependencies
            .iter()
            .filter(|dependency| dependency.selected && dependency.selectable)
            .map(|dependency| dependency.id.clone())
            .collect()
    }
}

/// Review-and-integrate form for a completed coding task.
pub struct ReviewForm {
    pub pane: usize,
    pub task_id: String,
    pub commit: String,
    pub status: String,
    pub integration: String,
    pub summary: String,
    pub diff: String,
    pub input: String,
    pub scroll: usize,
}

/// Byte caps the host enforces on `task/create`, mirrored so the editor cannot build a rejected task.
pub const TASK_GOAL_LIMIT: usize = 64_000;
pub const TASK_ACCEPTANCE_LIMIT: usize = 16_000;
/// How many dependencies the editor lets one task declare.
pub const MAX_DEPENDENCIES: usize = 8;

/// A user intent raised by the UI and resolved by the live client against the host.
#[derive(Debug)]
pub enum Action {
    Send {
        pane: usize,
        text: String,
        coding: bool,
    },
    /// A task edited in the composer, carrying the parameters the plain send path leaves empty.
    CreateTask {
        pane: usize,
        goal: String,
        acceptance: String,
        dependencies: Vec<String>,
        coding: bool,
    },
    OpenTask {
        pane: usize,
    },
    Cancel {
        pane: usize,
    },
    Refresh,
    Decide {
        id: String,
        accept: bool,
    },
    SaveCredential {
        pane: usize,
        key: String,
    },
    DeleteCredential {
        pane: usize,
    },
    OpenReview {
        pane: usize,
    },
    Integrate {
        task_id: String,
        expected_commit: String,
        review: String,
    },
    OpenCompact,
    Compact {
        summary: String,
        expected_revision: i64,
    },
    OpenCreate,
    CreateSession {
        repository: String,
        validation: Vec<Vec<String>>,
        requirement: String,
    },
    OpenAgent {
        pane: usize,
    },
    ConfigureAgent {
        pane: usize,
        id: String,
        role: &'static str,
        model: String,
        base_url: String,
        protocol: String,
        context_bytes: usize,
    },
    TestConnection {
        pane: usize,
    },
    OpenPublish,
    Publish {
        kind: &'static str,
        data: String,
    },
    OpenOutput {
        pane: usize,
    },
    RefreshOutput,
    /// Read `task/get` for the focused pane's latest task.
    OpenDetail {
        pane: usize,
    },
    /// Re-read the open detail view.
    RefreshDetail,
    /// Read `session/list` and open the picker.
    OpenSessions,
    /// Rebuild the client against another persisted session.
    SwitchSession {
        id: String,
    },
}

/// Context compaction form: a summary plus the exact session revision it is based on.
pub struct CompactForm {
    pub revision: i64,
    pub input: String,
    pub note: String,
}

/// Interactive session creation form (repository + validation commands + optional requirement).
pub struct CreateForm {
    pub repository: String,
    pub validation: String,
    pub requirement: String,
    /// Focused field: 0 repository, 1 validation, 2 requirement.
    pub field: usize,
    pub note: String,
}

/// One `session/list` row in the session picker.
pub struct SessionItem {
    pub id: String,
    pub repository: String,
    pub branch: String,
    pub base_commit: String,
    /// Number of configured validation commands.
    pub validations: usize,
    /// `requirement` / `user_override` excerpts found in this session's log, newest last.
    pub requirements: Vec<String>,
    /// Whether the requirement scan came back; false while it is still in flight.
    pub scanned: bool,
    pub current: bool,
}

/// Session picker (`session/list`): switch between persisted sessions without restarting.
///
/// Switching re-reads the whole session (agents, tasks, events, approvals) and drops the previous
/// one's view state, so panes, records and command history never mix across sessions.
pub struct SessionList {
    pub items: Vec<SessionItem>,
    pub index: usize,
    pub note: String,
}

/// One task's `task/get` snapshot: status, workspace, commits, dependencies and result metadata.
///
/// Every value comes from the host; a missing field is rendered empty rather than guessed. Only the
/// full-result attachment is read from disk, and only through the path the host already published.
pub struct DetailForm {
    pub pane: usize,
    pub task_id: String,
    pub status: String,
    pub coding: bool,
    pub context_version: i64,
    pub goal: String,
    pub acceptance: String,
    pub worktree: String,
    pub base_commit: String,
    pub commit: String,
    pub integration: String,
    /// Dependencies as (short id, status label) pairs.
    pub dependencies: Vec<(String, String)>,
    /// Reason from the `task_blocked` event, when this client has seen one.
    pub blocked: String,
    /// What the host did about a coordinator that stopped early (continuations, unfinished work).
    pub delegation: String,
    pub result: String,
    /// Attachment inventory line (label, size, path).
    pub artifacts: String,
    /// Locally read full-result attachment, clipped.
    pub body: String,
    pub scroll: usize,
}

/// Task output view (`task/output`): the persisted result summary, the running public draft and the
/// full-result attachment the client reads locally.
pub struct OutputForm {
    pub pane: usize,
    pub task_id: String,
    pub status: String,
    pub result: String,
    pub draft: String,
    /// Locally read attachment body, clipped; empty while the attachment does not exist yet.
    pub body: String,
    /// Locally read file-diff attachment, clipped; empty for non-coding tasks.
    pub diff: String,
    /// Attachment inventory line (label, size, path).
    pub artifacts: String,
    pub scroll: usize,
    /// Whether the client keeps refreshing while the task is still running.
    pub live: bool,
}

/// The user publication kinds accepted by `event/publish`, in toggle order.
pub const PUBLISH_KINDS: [&str; 3] = ["requirement", "user_override", "suggestion"];

/// Human label for a user publication kind.
pub fn publish_kind_label(kind: &str) -> &'static str {
    match kind {
        "requirement" => "需求（requirement）",
        "user_override" => "用户覆盖（user_override）",
        _ => "建议（suggestion）",
    }
}

/// User-publication form (`event/publish`): a kind plus one line of text.
///
/// `requirement` and `user_override` are authoritative records the host keeps injecting into every
/// model request; `suggestion` is advisory.
pub struct PublishForm {
    /// Index into `PUBLISH_KINDS`.
    pub kind: usize,
    pub input: String,
    pub note: String,
}

/// Agent configuration form (`agent/configure`): identity, endpoint and budget for one pane.
///
/// The role follows the pane position (pane 0 is the coordinator) and the id is either the
/// existing agent or a client-generated UUID, so neither is typed by the user.
pub struct AgentForm {
    pub pane: usize,
    pub id: String,
    pub role: &'static str,
    pub model: String,
    pub base_url: String,
    /// Provider wire protocol: `responses` or `chat_completions`.
    pub protocol: String,
    pub context_bytes: String,
    /// Focused field: 0 model, 1 baseUrl, 2 protocol, 3 contextBytes.
    pub field: usize,
    pub note: String,
}

/// The text field currently focused in the agent configuration form.
fn agent_field_mut(form: &mut AgentForm) -> &mut String {
    match form.field {
        0 => &mut form.model,
        1 => &mut form.base_url,
        2 => &mut form.protocol,
        _ => &mut form.context_bytes,
    }
}

/// Friendly pre-check of the host's URL rule: HTTPS, or HTTP on loopback, no credentials.
fn check_base_url(url: &str) -> Result<(), &'static str> {
    if url.is_empty() {
        return Err("需要填写 Base URL。");
    }
    let (scheme, rest) = url
        .strip_prefix("https://")
        .map(|rest| ("https", rest))
        .or_else(|| url.strip_prefix("http://").map(|rest| ("http", rest)))
        .ok_or("Base URL 需以 https:// 开头（本机可用 http://）。")?;
    if rest.is_empty() {
        return Err("Base URL 缺少主机名。");
    }
    if url.contains('?') {
        return Err("Base URL 不能包含查询串。");
    }
    if url.contains('#') {
        return Err("Base URL 不能包含片段。");
    }
    let authority = rest.split('/').next().unwrap_or("");
    if authority.contains('@') {
        return Err("Base URL 不能包含账号或密码。");
    }
    let host = if let Some(end) = authority.find(']') {
        &authority[..=end]
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return Err("Base URL 缺少主机名。");
    }
    if scheme == "http" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return Err("http 只允许 localhost、127.0.0.1 或 [::1]。");
    }
    Ok(())
}

pub struct App {
    pub mode: Mode,
    pub panes: [Pane; 4],
    pub focused: usize,
    pub zoom: bool,
    pub shared: bool,
    pub confirm_quit: bool,
    pub feedback: String,
    pub areas: [Rect; 4],
    pub shared_records: Vec<Line<'static>>,
    pub session_label: String,
    pub closed: bool,
    pub outbox: Vec<Action>,
    pub approvals: Vec<ApprovalItem>,
    pub approval_index: usize,
    pub credential: Option<CredentialForm>,
    pub review: Option<ReviewForm>,
    pub compact: Option<CompactForm>,
    pub create: Option<CreateForm>,
    pub agent: Option<AgentForm>,
    pub publish: Option<PublishForm>,
    pub task: Option<TaskForm>,
    pub output: Option<OutputForm>,
    pub sessions: Option<SessionList>,
    pub detail: Option<DetailForm>,
    pub coding: bool,
}

const PANE_HEADER: Color = Color::Reset;

impl App {
    /// Empty live shell: four unconfigured panes that the client fills from the host.
    pub fn live() -> Self {
        Self {
            mode: Mode::Live,
            panes: [
                Pane::placeholder("主控"),
                Pane::placeholder("工作1"),
                Pane::placeholder("工作2"),
                Pane::placeholder("工作3"),
            ],
            focused: 0,
            zoom: false,
            shared: false,
            confirm_quit: false,
            feedback: "正在连接协作后台…".into(),
            areas: [Rect::default(); 4],
            shared_records: Vec::new(),
            session_label: "未连接".into(),
            closed: false,
            outbox: Vec::new(),
            approvals: Vec::new(),
            approval_index: 0,
            credential: None,
            review: None,
            compact: None,
            create: None,
            agent: None,
            publish: None,
            task: None,
            output: None,
            sessions: None,
            detail: None,
            coding: false,
        }
    }

    pub fn demo() -> Self {
        let specs = [
            (
                "主控",
                "planner-model",
                "api-a",
                "Responses",
                "协调中",
                vec![
                    (MUTED, "$ codex --model planner-model"),
                    (ACCENT, "你 › 给项目增加请求重试，并行完成实现与审查。"),
                    (PANE_HEADER, "planner › 已拆分三个任务："),
                    (ACCENT, "  → 编码：实现重试策略与单元测试"),
                    (ACCENT, "  → 审查：检查取消、超时和错误边界"),
                    (ACCENT, "  → 文档：在行为确认后更新说明"),
                    (PANE_HEADER, "planner › 已收到编码模型的工作结果。"),
                    (GREEN, "[shared v18] retry.ts / 8 tests passed"),
                    (MUTED, "等待审查结论，再验证并合入协作分支。"),
                ],
            ),
            (
                "编码",
                "code-model",
                "api-b",
                "Responses",
                "已完成",
                vec![
                    (MUTED, "$ codex --model code-model"),
                    (ACCENT, "[tool] edit src/retry.ts"),
                    (GREEN, "[test] 8 passed · 0 failed"),
                    (PANE_HEADER, "coder › 已加入次数上限和退避等待。"),
                    (PANE_HEADER, "取消信号会中止计时器和后续请求。"),
                    (GREEN, "[commit] a31fc42 / 独立工作副本"),
                    (MUTED, "工作结果已共享，等待主控审查。"),
                ],
            ),
            (
                "审查",
                "review-model",
                "api-c",
                "Chat Completions",
                "工作中",
                vec![
                    (MUTED, "$ codex --model review-model"),
                    (ACCENT, "[tool] git diff 7e42a85 a31fc42"),
                    (PANE_HEADER, "reviewer › 正在检查取消与重试的交界。"),
                    (ACCENT, "[tool] run cancellation.spec.ts"),
                    (MUTED, "测试运行中…"),
                ],
            ),
            (
                "文档",
                "fast-model",
                "api-d",
                "Chat Completions",
                "等待依赖",
                vec![
                    (MUTED, "$ codex --model fast-model"),
                    (PANE_HEADER, "任务 › 更新重试策略与取消行为说明。"),
                    (MUTED, "[dependency] 等待实现与审查结果"),
                    (PANE_HEADER, "writer › 已读取共享需求与接口草案。"),
                    (MUTED, "行为确认后补充参数说明和使用示例。"),
                ],
            ),
        ];
        let panes = specs.map(|(role, model, api, protocol, status, lines)| {
            let mut pane = Pane::placeholder(role);
            pane.model = model.into();
            pane.api = api.into();
            pane.protocol = protocol.into();
            pane.status = status.into();
            pane.history = lines
                .into_iter()
                .map(|(color, text)| line(text, color))
                .collect();
            pane
        });
        Self {
            mode: Mode::Demo,
            panes,
            focused: 0,
            zoom: false,
            shared: false,
            confirm_quit: false,
            feedback: "模拟记录；不会调用模型、读取密钥或修改项目。".into(),
            areas: [Rect::default(); 4],
            shared_records: vec![
                line(
                    "[user/v12] 增加有上限的请求重试，取消后不得继续发送请求。",
                    ACCENT,
                ),
                line("[planner/v15] 实现、审查并行；文档等待行为确认。", ACCENT),
                line(
                    "[coder/v18] retry.ts 已提交，8 项测试通过，等待主控审查。",
                    ACCENT,
                ),
            ],
            session_label: "~/Projects/atlas  [演示 / 模拟记录]".into(),
            closed: false,
            outbox: Vec::new(),
            approvals: Vec::new(),
            approval_index: 0,
            credential: None,
            review: None,
            compact: None,
            create: None,
            agent: None,
            publish: None,
            task: None,
            output: None,
            sessions: None,
            detail: None,
            coding: false,
        }
    }

    /// Append a line to a pane's history, keeping the buffer bounded.
    pub fn push_line(&mut self, pane: usize, text: impl Into<String>, color: Color) {
        let history = &mut self.panes[pane].history;
        history.push(line(text, color));
        if history.len() > 500 {
            let excess = history.len() - 500;
            history.drain(..excess);
        }
    }

    /// Index of the pane for an agent id, if configured.
    pub fn pane_of(&self, agent_id: &str) -> Option<usize> {
        self.panes
            .iter()
            .position(|p| p.agent_id.as_deref() == Some(agent_id))
    }

    pub fn key(&mut self, key: KeyEvent) -> bool {
        if self.confirm_quit {
            match key.code {
                KeyCode::Enter => return true,
                KeyCode::Esc => self.confirm_quit = false,
                _ => {}
            }
            return false;
        }
        // Quit must work even while a modal owns the keyboard.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            self.confirm_quit = true;
            return false;
        }
        if self.detail.is_some() {
            self.detail_key(key);
            return false;
        }
        if self.sessions.is_some() {
            self.sessions_key(key);
            return false;
        }
        if self.output.is_some() {
            self.output_key(key);
            return false;
        }
        if self.task.is_some() {
            self.task_key(key);
            return false;
        }
        if self.publish.is_some() {
            self.publish_key(key);
            return false;
        }
        if self.agent.is_some() {
            self.agent_key(key);
            return false;
        }
        if self.create.is_some() {
            self.create_key(key);
            return false;
        }
        if self.compact.is_some() {
            self.compact_key(key);
            return false;
        }
        if self.review.is_some() {
            self.review_key(key);
            return false;
        }
        if self.credential.is_some() {
            self.credential_key(key);
            return false;
        }
        if !self.approvals.is_empty() {
            self.approval_key(key);
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('r') if self.mode == Mode::Live => {
                    self.outbox.push(Action::Refresh);
                    self.feedback = "已请求刷新会话、任务与共享记录。".into();
                }
                KeyCode::Char('k') if self.mode == Mode::Live => self.open_credential(),
                KeyCode::Char('a') if self.mode == Mode::Live => self.open_agent(),
                KeyCode::Char('t') if self.mode == Mode::Live => self.open_probe(),
                KeyCode::Char('u') if self.mode == Mode::Live => self.open_publish(),
                KeyCode::Char('o') if self.mode == Mode::Live => self.open_output(),
                // Ctrl+F opens the task editor. Ctrl+I would be indistinguishable from Tab (both
                // arrive as 0x09), so it can never be a distinct binding in a terminal.
                KeyCode::Char('f') if self.mode == Mode::Live => self.open_task(),
                KeyCode::Char('e') if self.mode == Mode::Live => {
                    self.coding = !self.coding;
                    self.feedback = if self.coding {
                        "后续输入将作为编码任务提交（产物提交后需集成）。".into()
                    } else {
                        "后续输入按普通任务提交。".into()
                    };
                }
                KeyCode::Char('g') if self.mode == Mode::Live => self.open_review(),
                KeyCode::Char('p') if self.mode == Mode::Live => self.open_compact(),
                KeyCode::Char('n') if self.mode == Mode::Live => self.open_create(),
                KeyCode::Char('l') if self.mode == Mode::Live => self.open_sessions(),
                KeyCode::Char('d') if self.mode == Mode::Live => self.open_detail(),
                KeyCode::Char('c') => self.stop(),
                _ => {}
            }
            return false;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Char(c @ '1'..='4') => self.focused = c as usize - '1' as usize,
                KeyCode::Enter => self.zoom = !self.zoom,
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Tab => self.focused = (self.focused + 1) % 4,
            KeyCode::BackTab => self.focused = (self.focused + 3) % 4,
            KeyCode::F(2) => self.zoom = !self.zoom,
            KeyCode::F(3) => self.shared = !self.shared,
            KeyCode::Esc => {
                self.zoom = false;
                self.shared = false;
            }
            KeyCode::PageUp => {
                self.panes[self.focused].scroll = self.panes[self.focused].scroll.saturating_add(8)
            }
            KeyCode::PageDown => {
                self.panes[self.focused].scroll = self.panes[self.focused].scroll.saturating_sub(8)
            }
            KeyCode::End => {
                let pane = &mut self.panes[self.focused];
                // While browsing history, End still jumps back to the live tail; otherwise it moves
                // the caret to the end of the line.
                if pane.scroll > 0 {
                    pane.scroll = 0;
                } else {
                    pane.input.move_line_end();
                }
            }
            KeyCode::Home => self.panes[self.focused].input.move_line_start(),
            KeyCode::Left => self.panes[self.focused].input.move_left(),
            KeyCode::Right => self.panes[self.focused].input.move_right(),
            KeyCode::Up => self.panes[self.focused].recall_previous(),
            KeyCode::Down => self.panes[self.focused].recall_next(),
            KeyCode::Delete => self.panes[self.focused].input.delete(),
            KeyCode::Backspace => self.panes[self.focused].input.backspace(),
            KeyCode::Enter => self.send(),
            KeyCode::Char(c) => self.paste(&c.to_string()),
            _ => {}
        }
        false
    }

    /// Stop the focused pane. Demo mode marks it locally; live mode asks the host to cancel.
    fn stop(&mut self) {
        let index = self.focused;
        if self.mode == Mode::Demo {
            let p = &mut self.panes[index];
            if !p.stopped {
                p.stopped = true;
                p.status = "已停止".into();
                p.history.push(line("[模拟] 当前任务已停止。", MUTED));
            }
            let role = p.role.clone();
            self.feedback = format!("已停止{role}任务，其他窗格不受影响。");
        } else {
            self.panes[index].status = "停止中".into();
            self.outbox.push(Action::Cancel { pane: index });
            self.feedback = "已请求取消该窗格当前任务。".into();
        }
    }

    /// Open the context compaction flow: probe the exact session revision first.
    fn open_compact(&mut self) {
        self.outbox.push(Action::OpenCompact);
        self.feedback = "正在读取会话版本，准备压缩上下文…".into();
    }

    /// Keys while the compaction form is open. Enter submits the exact revision.
    fn compact_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let ready = self
                    .compact
                    .as_ref()
                    .is_some_and(|f| !f.input.trim().is_empty());
                if !ready {
                    if let Some(form) = self.compact.as_mut() {
                        form.note = "先填写摘要，再压缩。".into();
                    }
                    return;
                }
                let form = self.compact.take().expect("form is open");
                self.outbox.push(Action::Compact {
                    summary: form.input,
                    expected_revision: form.revision,
                });
                self.feedback = "已提交上下文压缩，等待后台写入检查点…".into();
            }
            KeyCode::Esc => {
                self.compact = None;
                self.feedback = "已取消上下文压缩。".into();
            }
            KeyCode::Backspace => {
                if let Some(form) = self.compact.as_mut() {
                    form.input.pop();
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(form) = self.compact.as_mut()
                    && form.input.len() + c.len_utf8() <= 8192
                {
                    form.input.push(c);
                }
            }
            _ => {}
        }
    }

    /// Ask the client to prepare the interactive session creation form.
    fn open_create(&mut self) {
        self.outbox.push(Action::OpenCreate);
        self.feedback = "正在准备会话创建表单…".into();
    }

    /// Keys while the session creation form is open. Tab cycles repository/validation/requirement.
    fn create_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                if let Some(form) = self.create.as_mut() {
                    form.field = (form.field + 1) % 3;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Some(form) = self.create.as_mut() {
                    form.field = (form.field + 2) % 3;
                }
            }
            KeyCode::Esc => {
                self.create = None;
                self.feedback = "已取消创建会话。".into();
            }
            KeyCode::Backspace => {
                if let Some(form) = self.create.as_mut() {
                    field_mut(form).pop();
                }
            }
            KeyCode::Enter => {
                let Some(form) = self.create.as_ref() else {
                    return;
                };
                let repository = form.repository.trim().to_owned();
                if repository.is_empty() {
                    if let Some(form) = self.create.as_mut() {
                        form.field = 0;
                        form.note = "需要填写 Git 仓库的绝对路径。".into();
                    }
                    return;
                }
                let validation = parse_validation(&form.validation);
                if validation.is_empty() {
                    if let Some(form) = self.create.as_mut() {
                        form.field = 1;
                        form.note = "至少填写一条验证命令（每行一条）。".into();
                    }
                    return;
                }
                let requirement = form.requirement.trim().to_owned();
                self.create = None;
                self.outbox.push(Action::CreateSession {
                    repository,
                    validation,
                    requirement,
                });
                self.feedback = "正在创建协作会话…".into();
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(form) = self.create.as_mut()
                    && field_mut(form).len() + c.len_utf8() <= 4096
                {
                    field_mut(form).push(c);
                }
            }
            _ => {}
        }
    }

    /// Ask the client to read `task/get` for the focused pane's latest task (live mode).
    fn open_detail(&mut self) {
        if self.panes[self.focused].agent_id.is_none() {
            self.feedback = "该窗格尚未配置 Agent，没有任务详情。".into();
            return;
        }
        self.outbox.push(Action::OpenDetail { pane: self.focused });
        self.feedback = "正在读取任务详情…".into();
    }

    /// Keys while the task detail view is open. Page keys scroll, Ctrl+R re-reads the snapshot.
    fn detail_key(&mut self, key: KeyEvent) {
        if self.detail.is_none() {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.detail = None;
                self.feedback = "已关闭任务详情。".into();
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.outbox.push(Action::RefreshDetail);
                self.feedback = "正在刷新任务详情…".into();
            }
            KeyCode::PageUp => {
                if let Some(form) = self.detail.as_mut() {
                    form.scroll = form.scroll.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if let Some(form) = self.detail.as_mut() {
                    form.scroll = form.scroll.saturating_add(10);
                }
            }
            _ => {}
        }
    }

    /// Ask the client to read `session/list` and open the picker (live mode).
    fn open_sessions(&mut self) {
        self.outbox.push(Action::OpenSessions);
        self.feedback = "正在读取会话列表…".into();
    }

    /// Keys while the session picker is open. Enter rebuilds the client against another session.
    fn sessions_key(&mut self, key: KeyEvent) {
        let Some(list) = self.sessions.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up => list.index = list.index.saturating_sub(1),
            KeyCode::Down if list.index + 1 < list.items.len() => list.index += 1,
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.outbox.push(Action::OpenSessions);
                self.feedback = "正在重新读取会话列表…".into();
            }
            KeyCode::Enter => {
                let Some(item) = list.items.get(list.index) else {
                    list.note = "没有可切换的会话；Ctrl+N 新建。".into();
                    return;
                };
                let (id, current) = (item.id.clone(), item.current);
                self.sessions = None;
                if current {
                    self.feedback = "已在该会话中。".into();
                    return;
                }
                self.outbox.push(Action::SwitchSession { id: id.clone() });
                self.feedback = format!("正在切换到会话 {}…", short_id(&id));
            }
            KeyCode::Esc => {
                self.sessions = None;
                self.feedback = "已关闭会话列表。".into();
            }
            _ => {}
        }
    }

    /// Drop every per-session view before attaching to another session.
    ///
    /// Keeps global user choices (focus, zoom, shared panel, coding toggle) and replaces the panes
    /// with empty placeholders, so a session with no agents cannot show the previous session's
    /// panes, records or command history.
    pub fn reset_session_view(&mut self) {
        self.panes = [
            Pane::placeholder("主控"),
            Pane::placeholder("工作1"),
            Pane::placeholder("工作2"),
            Pane::placeholder("工作3"),
        ];
        self.shared_records.clear();
        self.approvals.clear();
        self.approval_index = 0;
        self.output = None;
        self.detail = None;
        self.task = None;
        self.review = None;
        self.compact = None;
        self.create = None;
        self.agent = None;
        self.publish = None;
        self.credential = None;
    }

    /// Ask the client to open the task output view for the focused pane (live mode).
    fn open_output(&mut self) {
        self.outbox.push(Action::OpenOutput { pane: self.focused });
        self.feedback = "正在读取任务输出…".into();
    }

    /// Keys while the task output view is open. Page keys scroll, Ctrl+R re-reads the task.
    fn output_key(&mut self, key: KeyEvent) {
        if self.output.is_none() {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.output = None;
                self.feedback = "已关闭任务输出。".into();
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.outbox.push(Action::RefreshOutput);
                self.feedback = "正在刷新任务输出…".into();
            }
            KeyCode::PageUp => {
                if let Some(form) = self.output.as_mut() {
                    form.scroll = form.scroll.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if let Some(form) = self.output.as_mut() {
                    form.scroll = form.scroll.saturating_add(10);
                }
            }
            _ => {}
        }
    }

    /// Ask the client to open the task editor for the focused pane (live mode).
    fn open_task(&mut self) {
        if self.panes[self.focused].agent_id.is_none() {
            self.feedback = "该窗格尚未配置 Agent，无法编排任务。".into();
            return;
        }
        self.outbox.push(Action::OpenTask { pane: self.focused });
        self.feedback = "正在读取任务列表，准备任务编辑器…".into();
    }

    /// Keys while the task editor is open. Enter inserts a newline in the focused text field and
    /// Ctrl+S submits, so a multi-line goal never dispatches by accident; a space is content in the
    /// text fields and a toggle in the dependency list and the coding switch.
    fn task_key(&mut self, key: KeyEvent) {
        if self.task.is_none() {
            return;
        }
        let field = self.task.as_ref().map(|form| form.field).unwrap_or(0);
        let text_field = field < 2;
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('s') {
                self.submit_task();
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.task = None;
                self.feedback = "已取消任务编排。".into();
            }
            KeyCode::Tab => self.task_field(1),
            KeyCode::BackTab => self.task_field(3),
            KeyCode::Enter if text_field => {
                let limit = if field == 0 {
                    TASK_GOAL_LIMIT
                } else {
                    TASK_ACCEPTANCE_LIMIT
                };
                self.task_edit(|buffer| {
                    buffer.insert_char('\n', limit);
                });
            }
            KeyCode::Backspace => self.task_edit(TextBuf::backspace),
            KeyCode::Delete => self.task_edit(TextBuf::delete),
            KeyCode::Left => {
                if field == 3 {
                    self.task_toggle_coding();
                } else {
                    self.task_edit(TextBuf::move_left);
                }
            }
            KeyCode::Right => {
                if field == 3 {
                    self.task_toggle_coding();
                } else {
                    self.task_edit(TextBuf::move_right);
                }
            }
            KeyCode::Home => self.task_edit(TextBuf::move_line_start),
            KeyCode::End => self.task_edit(TextBuf::move_line_end),
            KeyCode::Up if field == 2 => self.task_row(-1),
            KeyCode::Down if field == 2 => self.task_row(1),
            KeyCode::PageUp if field == 2 => self.task_row(-5),
            KeyCode::PageDown if field == 2 => self.task_row(5),
            KeyCode::Char(' ') if field == 2 => self.task_toggle_dependency(),
            KeyCode::Char(' ') if field == 3 => self.task_toggle_coding(),
            KeyCode::Char(c) if !c.is_control() && text_field => {
                let limit = if field == 0 {
                    TASK_GOAL_LIMIT
                } else {
                    TASK_ACCEPTANCE_LIMIT
                };
                self.task_edit(|buffer| {
                    buffer.insert_char(c, limit);
                });
            }
            _ => {}
        }
    }

    /// Move the caret within the focused task text field.
    fn task_edit(&mut self, edit: impl FnOnce(&mut TextBuf)) {
        if let Some(form) = self.task.as_mut()
            && let Some(buffer) = form.text_field()
        {
            edit(buffer);
        }
    }

    /// Cycle the focused task field.
    fn task_field(&mut self, step: usize) {
        if let Some(form) = self.task.as_mut() {
            form.field = (form.field + step) % 4;
            if form.field != 2 {
                form.note.clear();
            }
        }
    }

    /// Move the highlighted dependency row, clamped to the list.
    fn task_row(&mut self, step: isize) {
        if let Some(form) = self.task.as_mut()
            && !form.dependencies.is_empty()
        {
            let last = form.dependencies.len() - 1;
            let next = form.index as isize + step;
            form.index = next.clamp(0, last as isize) as usize;
        }
    }

    /// Toggle the highlighted dependency, refusing candidates that can never be satisfied.
    fn task_toggle_dependency(&mut self) {
        let Some(form) = self.task.as_mut() else {
            return;
        };
        let selected = form
            .dependencies
            .iter()
            .filter(|dependency| dependency.selected)
            .count();
        let Some(dependency) = form.dependencies.get_mut(form.index) else {
            return;
        };
        if !dependency.selectable {
            form.note = format!("{}{}", dependency.label, dependency.note);
            return;
        }
        if !dependency.selected && selected >= MAX_DEPENDENCIES {
            form.note = format!("一次最多声明 {MAX_DEPENDENCIES} 项依赖。");
            return;
        }
        dependency.selected = !dependency.selected;
        form.note.clear();
    }

    /// Flip the coding switch; `task/create` then requires integration before dependents run.
    fn task_toggle_coding(&mut self) {
        if let Some(form) = self.task.as_mut() {
            form.coding = !form.coding;
        }
    }

    /// Point the editor at one field and explain what is missing.
    fn task_note(&mut self, field: usize, note: &str) {
        if let Some(form) = self.task.as_mut() {
            form.field = field;
            form.note = note.into();
        }
    }

    /// Validate the editor locally, then queue `task/create` with the full parameter set.
    fn submit_task(&mut self) {
        let Some(form) = self.task.as_ref() else {
            return;
        };
        let goal = form.goal.text.trim().to_owned();
        if goal.is_empty() {
            self.task_note(0, "先填写任务目标。");
            return;
        }
        let acceptance = form.acceptance.text.trim().to_owned();
        if acceptance.is_empty() {
            self.task_note(
                1,
                "填写验收标准；若确实不需要，请在窗格内用 Enter 直接派发。",
            );
            return;
        }
        let pane = form.pane;
        let coding = form.coding;
        let dependencies = form.selected();
        self.task = None;
        self.outbox.push(Action::CreateTask {
            pane,
            goal,
            acceptance,
            dependencies,
            coding,
        });
        self.feedback = "已提交任务，等待后台确认。".into();
    }

    /// Ask the client to open the user-publication form (live mode).
    fn open_publish(&mut self) {
        self.outbox.push(Action::OpenPublish);
        self.feedback = "正在准备用户约束表单…".into();
    }

    /// Keys while the user-publication form is open. Left/Right/Tab toggles the kind; a space is
    /// ordinary content, since constraints are free text.
    fn publish_key(&mut self, key: KeyEvent) {
        if self.publish.is_none() {
            return;
        }
        match key.code {
            KeyCode::Enter => self.submit_publish(),
            KeyCode::Esc => {
                self.publish = None;
                self.feedback = "已取消发布。".into();
            }
            KeyCode::Left | KeyCode::BackTab => {
                if let Some(form) = self.publish.as_mut() {
                    form.kind = (form.kind + PUBLISH_KINDS.len() - 1) % PUBLISH_KINDS.len();
                }
            }
            KeyCode::Right | KeyCode::Tab => {
                if let Some(form) = self.publish.as_mut() {
                    form.kind = (form.kind + 1) % PUBLISH_KINDS.len();
                }
            }
            KeyCode::Backspace => {
                if let Some(form) = self.publish.as_mut() {
                    form.input.pop();
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(form) = self.publish.as_mut()
                    && form.input.len() + c.len_utf8() <= 4096
                {
                    form.input.push(c);
                }
            }
            _ => {}
        }
    }

    /// Validate the publication form locally, then queue `event/publish`.
    fn submit_publish(&mut self) {
        let Some(form) = self.publish.as_ref() else {
            return;
        };
        let data = form.input.trim().to_owned();
        if data.is_empty() {
            if let Some(form) = self.publish.as_mut() {
                form.note = "先填写内容，再发布。".into();
            }
            return;
        }
        let form = self.publish.take().expect("form is open");
        let kind = PUBLISH_KINDS[form.kind.min(PUBLISH_KINDS.len() - 1)];
        self.outbox.push(Action::Publish { kind, data });
        self.feedback = format!("正在发布{}…", publish_kind_label(kind));
    }

    /// Ask the client to run a capability probe for the focused agent (live mode).
    fn open_probe(&mut self) {
        if self.panes[self.focused].agent_id.is_none() {
            self.feedback = "该窗格尚未配置 Agent，无法自检。".into();
            return;
        }
        self.outbox
            .push(Action::TestConnection { pane: self.focused });
        self.feedback = "正在排队连接探针任务…".into();
    }

    /// Ask the client to prepare the Agent configuration form for the focused pane.
    fn open_agent(&mut self) {
        self.outbox.push(Action::OpenAgent { pane: self.focused });
        self.feedback = "正在准备 Agent 配置表单…".into();
    }

    /// Keys while the Agent form is open. Tab cycles fields; the protocol is toggled, not typed.
    fn agent_key(&mut self, key: KeyEvent) {
        if self.agent.is_none() {
            return;
        }
        match key.code {
            KeyCode::Enter => self.submit_agent(),
            KeyCode::Esc => {
                self.agent = None;
                self.feedback = "已取消 Agent 配置。".into();
            }
            KeyCode::Tab | KeyCode::Down => {
                if let Some(form) = self.agent.as_mut() {
                    form.field = (form.field + 1) % 4;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Some(form) = self.agent.as_mut() {
                    form.field = (form.field + 3) % 4;
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                if let Some(form) = self.agent.as_mut()
                    && form.field == 2
                {
                    form.protocol = if form.protocol == "responses" {
                        "chat_completions".into()
                    } else {
                        "responses".into()
                    };
                }
            }
            KeyCode::Backspace => {
                if let Some(form) = self.agent.as_mut()
                    && form.field != 2
                {
                    agent_field_mut(form).pop();
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(form) = self.agent.as_mut() {
                    if form.field == 2 {
                        // The protocol field is a toggle; letters are ignored.
                    } else if form.field == 3 && !c.is_ascii_digit() {
                        form.note = "contextBytes 只能是数字。".into();
                    } else {
                        let limit = if form.field == 3 { 7 } else { 4096 };
                        let target = agent_field_mut(form);
                        if target.len() + c.len_utf8() <= limit {
                            target.push(c);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Point the form at one field and explain what is missing.
    fn agent_note(&mut self, field: usize, note: &str) {
        if let Some(form) = self.agent.as_mut() {
            form.field = field;
            form.note = note.into();
        }
    }

    /// Validate the Agent form locally, then queue `agent/configure` for the host.
    fn submit_agent(&mut self) {
        let Some(form) = self.agent.as_ref() else {
            return;
        };
        let model = form.model.trim().to_owned();
        if model.is_empty() || model.len() > 200 {
            self.agent_note(0, "模型名需为 1..200 字节。");
            return;
        }
        let base_url = form.base_url.trim().to_owned();
        if let Err(note) = check_base_url(&base_url) {
            self.agent_note(1, note);
            return;
        }
        let context_bytes = match form.context_bytes.trim().parse::<usize>() {
            Ok(bytes) if (4096..=1_000_000).contains(&bytes) => bytes,
            _ => {
                self.agent_note(3, "contextBytes 需为 4096..1000000 的整数。");
                return;
            }
        };
        let form = self.agent.take().expect("form is open");
        self.outbox.push(Action::ConfigureAgent {
            pane: form.pane,
            id: form.id,
            role: form.role,
            model,
            base_url,
            protocol: form.protocol,
            context_bytes,
        });
        self.feedback = "正在配置 Agent，等待后台确认…".into();
    }

    /// Open the review-and-integrate form for the focused pane's latest task (live mode).
    fn open_review(&mut self) {
        if self.panes[self.focused].agent_id.is_none() {
            self.feedback = "该窗格尚未配置 Agent，无法审查。".into();
            return;
        }
        self.outbox.push(Action::OpenReview { pane: self.focused });
        self.feedback = "正在读取任务与差异…".into();
    }

    /// Keys while the review form is open. Enter submits the integration for the exact commit.
    fn review_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let ready = self
                    .review
                    .as_ref()
                    .is_some_and(|r| !r.input.trim().is_empty());
                if !ready {
                    self.feedback = "先填写审查结论，再集成。".into();
                    return;
                }
                let form = self.review.take().expect("form is open");
                self.outbox.push(Action::Integrate {
                    task_id: form.task_id,
                    expected_commit: form.commit,
                    review: form.input,
                });
                self.feedback = "已提交集成请求，正在验证候选提交…".into();
            }
            KeyCode::Esc => {
                self.review = None;
                self.feedback = "已取消集成。".into();
            }
            KeyCode::Backspace => {
                if let Some(form) = self.review.as_mut() {
                    form.input.pop();
                }
            }
            KeyCode::PageUp => {
                if let Some(form) = self.review.as_mut() {
                    form.scroll = form.scroll.saturating_sub(8);
                }
            }
            KeyCode::PageDown => {
                if let Some(form) = self.review.as_mut() {
                    form.scroll = form.scroll.saturating_add(8);
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(form) = self.review.as_mut()
                    && form.input.len() + c.len_utf8() <= 8192
                {
                    form.input.push(c);
                }
            }
            _ => {}
        }
    }

    /// Open the masked credential form for the focused agent (live mode).
    fn open_credential(&mut self) {
        if self.panes[self.focused].agent_id.is_none() {
            self.feedback = "该窗格尚未配置 Agent，无法设置密钥。".into();
            return;
        }
        self.credential = Some(CredentialForm {
            pane: self.focused,
            input: String::new(),
        });
        self.feedback = "输入 API Key（不回显）：Enter 保存 · Ctrl+D 删除 · Esc 取消。".into();
    }

    /// Keys while the credential form is open. The typed key is never echoed back.
    fn credential_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let form = self.credential.take().expect("form is open");
                if form.input.is_empty() {
                    self.feedback = "密钥为空，未提交。".into();
                    return;
                }
                self.outbox.push(Action::SaveCredential {
                    pane: form.pane,
                    key: form.input,
                });
                self.feedback = "已提交密钥，等待后台确认。".into();
            }
            KeyCode::Esc => {
                self.credential = None;
                self.feedback = "已取消密钥输入。".into();
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(form) = self.credential.take() {
                    self.outbox
                        .push(Action::DeleteCredential { pane: form.pane });
                    self.feedback = "已请求删除该 Agent 的密钥。".into();
                }
            }
            KeyCode::Backspace => {
                if let Some(form) = self.credential.as_mut() {
                    form.input.pop();
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(form) = self.credential.as_mut()
                    && form.input.len() + c.len_utf8() <= 8192
                {
                    form.input.push(c);
                }
            }
            _ => {}
        }
    }

    /// Keys while an operation approval is pending. Decisions apply to one request at a time.
    fn approval_key(&mut self, key: KeyEvent) {
        if self.approvals.is_empty() {
            return;
        }
        let index = self.approval_index.min(self.approvals.len() - 1);
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.decide(index, true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.decide(index, false),
            KeyCode::Down | KeyCode::Tab => {
                self.approval_index = (index + 1) % self.approvals.len();
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.approval_index = (index + self.approvals.len() - 1) % self.approvals.len();
            }
            _ => {}
        }
    }

    fn decide(&mut self, index: usize, accept: bool) {
        let item = self.approvals.remove(index);
        if self.approvals.is_empty() {
            self.approval_index = 0;
        } else {
            self.approval_index = index.min(self.approvals.len() - 1);
        }
        self.outbox.push(Action::Decide {
            id: item.id.clone(),
            accept,
        });
        let verb = if accept { "批准" } else { "拒绝" };
        self.feedback = format!("已{verb}{}（仅本次操作）。", short_id(&item.id));
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(form) = self.task.as_mut() {
            // The editor's text fields are multi-line: a pasted newline stays a newline. The
            // dependency list and the coding switch take no text at all.
            if form.field < 2 {
                let limit = if form.field == 0 {
                    TASK_GOAL_LIMIT
                } else {
                    TASK_ACCEPTANCE_LIMIT
                };
                let mut full = false;
                for character in text.chars() {
                    let character = match character {
                        '\n' => '\n',
                        '\t' => ' ',
                        c if c.is_control() => continue,
                        c => c,
                    };
                    let buffer = form.text_field().expect("a text field is focused");
                    if !buffer.insert_char(character, limit) {
                        full = true;
                        break;
                    }
                }
                if full {
                    self.feedback = "任务文本已达上限，请分段填写。".into();
                }
            }
            return;
        }
        if let Some(form) = self.agent.as_mut() {
            if form.field == 2 {
                return;
            }
            let digits_only = form.field == 3;
            let limit = if digits_only { 7 } else { 4096 };
            let mut full = false;
            for c in text.chars() {
                let c = if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                };
                if c.is_control() || (digits_only && !c.is_ascii_digit()) {
                    continue;
                }
                let target = agent_field_mut(form);
                if target.len() + c.len_utf8() > limit {
                    full = true;
                    break;
                }
                target.push(c);
            }
            if full {
                self.feedback = "输入上限已满，请分段填写。".into();
            }
            return;
        }
        if let Some(form) = self.create.as_mut() {
            let allow_newline = form.field != 0;
            let mut full = false;
            for c in text.chars() {
                if c == '\n' && allow_newline {
                    if form.validation.len() + form.requirement.len() >= 8180 {
                        full = true;
                        break;
                    }
                    field_mut(form).push('\n');
                    continue;
                }
                if c.is_control() {
                    continue;
                }
                if field_mut(form).len() + c.len_utf8() > 4096 {
                    full = true;
                    break;
                }
                field_mut(form).push(c);
            }
            if full {
                self.feedback = "输入上限已满，请分段填写。".into();
            }
            return;
        }
        if let Some(form) = self.compact.as_mut() {
            let mut full = false;
            for c in text.chars() {
                let c = if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                };
                if c.is_control() {
                    continue;
                }
                if form.input.len() + c.len_utf8() > 8192 {
                    full = true;
                    break;
                }
                form.input.push(c);
            }
            if full {
                self.feedback = "摘要上限为 8192 字节，请分段输入。".into();
            }
            return;
        }
        if let Some(form) = self.review.as_mut() {
            let mut full = false;
            for c in text.chars() {
                let c = if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                };
                if c.is_control() {
                    continue;
                }
                if form.input.len() + c.len_utf8() > 8192 {
                    full = true;
                    break;
                }
                form.input.push(c);
            }
            if full {
                self.feedback = "审查结论上限为 8192 字节，请分段输入。".into();
            }
            return;
        }
        if let Some(form) = self.publish.as_mut() {
            let mut full = false;
            for c in text.chars() {
                let c = if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                };
                if c.is_control() {
                    continue;
                }
                if form.input.len() + c.len_utf8() > 4096 {
                    full = true;
                    break;
                }
                form.input.push(c);
            }
            if full {
                self.feedback = "约束上限为 4096 字节，请分段发布。".into();
            }
            return;
        }
        // Credential input accepts no separators and is capped like the host's Secret::new.
        if let Some(form) = self.credential.as_mut() {
            for c in text.chars() {
                if c.is_control() {
                    continue;
                }
                if form.input.len() + c.len_utf8() > 8192 {
                    self.feedback = "密钥上限为 8192 字节，请分段输入。".into();
                    break;
                }
                form.input.push(c);
            }
            return;
        }
        let pane = &mut self.panes[self.focused];
        // shortcut: 单行输入，上限 4096 字节；多行任务文本在 Ctrl+F 任务编辑器里。
        for c in text.chars() {
            let c = if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            };
            if c.is_control() {
                continue;
            }
            if !pane.input.insert_char(c, 4096) {
                self.feedback = "输入上限为 4096 字节，请分段输入。".into();
                break;
            }
        }
    }

    fn send(&mut self) {
        let index = self.focused;
        let message = self.panes[index].input.text.trim().to_owned();
        if message.is_empty() {
            return;
        }
        self.panes[index].remember(&message);
        match self.mode {
            Mode::Demo => {
                let p = &mut self.panes[index];
                p.history.push(line(format!("你 › {message}"), ACCENT));
                p.history.push(line(
                    format!("{} › [模拟回显] 已收到此窗格的输入。", p.model),
                    GREEN,
                ));
                // shortcut: 演示仅保留最近 200 条记录，接入后台时改为读取持久化历史。
                if p.history.len() > 200 {
                    let excess = p.history.len() - 200;
                    p.history.drain(..excess);
                }
                p.input.clear();
                p.recall = None;
                p.scroll = 0;
                p.stopped = false;
                p.status = "模拟完成".into();
                let role = p.role.clone();
                self.feedback = format!("输入只发送到{role}窗格；这里没有真实 API 调用。");
            }
            Mode::Live => {
                let p = &mut self.panes[index];
                p.input.clear();
                p.recall = None;
                p.scroll = 0;
                self.outbox.push(Action::Send {
                    pane: index,
                    text: message,
                    coding: self.coding,
                });
                self.feedback = "已派发任务，等待后台确认。".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    #[test]
    fn focus_cycles_and_keeps_each_input() {
        let mut app = App::demo();
        app.paste("主控输入");
        app.key(press(KeyCode::Tab));
        app.paste("编码输入");
        app.key(press(KeyCode::BackTab));
        assert_eq!(app.focused, 0);
        assert_eq!(app.panes[0].input, "主控输入");
        assert_eq!(app.panes[1].input, "编码输入");
        app.key(press(KeyCode::BackTab));
        assert_eq!(app.focused, 3);
    }
    #[test]
    fn send_is_local_and_never_starts_a_model() {
        let mut app = App::demo();
        let lengths = app.panes.each_ref().map(|p| p.history.len());
        app.focused = 2;
        app.paste("检查取消边界");
        app.key(press(KeyCode::Enter));
        assert!(app.panes[2].input.is_empty());
        for (i, length) in lengths.into_iter().enumerate() {
            assert_eq!(
                app.panes[i].history.len(),
                length + if i == 2 { 2 } else { 0 }
            );
        }
        assert!(app.feedback.contains("没有真实 API"));
    }
    #[test]
    fn live_send_queues_an_action_without_touching_history() {
        let mut app = App::live();
        let length = app.panes[0].history.len();
        app.paste("实现重试策略");
        app.key(press(KeyCode::Enter));
        assert!(app.panes[0].input.is_empty());
        assert_eq!(app.panes[0].history.len(), length);
        match app.outbox.as_slice() {
            [Action::Send { pane, text, coding }] => {
                assert_eq!(*pane, 0);
                assert_eq!(text, "实现重试策略");
                assert!(!coding);
            }
            other => panic!("unexpected outbox: {other:?}"),
        }
    }
    #[test]
    fn coding_toggle_is_carried_into_task_creation() {
        let mut app = App::live();
        app.key(ctrl('e'));
        assert!(app.coding);
        app.paste("实现重试策略");
        app.key(press(KeyCode::Enter));
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::Send { coding: true, .. }]
        ));
    }
    #[test]
    fn review_requires_a_conclusion_then_integrates() {
        let mut app = App::live();
        app.panes[0].agent_id = Some("c".into());
        app.key(ctrl('g'));
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::OpenReview { pane: 0 }]
        ));
        app.review = Some(ReviewForm {
            pane: 0,
            task_id: "task-1".into(),
            commit: "abc123".into(),
            status: "completed".into(),
            integration: "pending".into(),
            summary: "done".into(),
            diff: String::new(),
            input: String::new(),
            scroll: 0,
        });
        app.key(press(KeyCode::Enter));
        assert!(app.review.is_some(), "empty review must not integrate");
        app.paste("已核对差异与验证");
        app.key(press(KeyCode::Enter));
        assert!(app.review.is_none());
        match app.outbox.last() {
            Some(Action::Integrate {
                task_id,
                expected_commit,
                review,
            }) => {
                assert_eq!(task_id, "task-1");
                assert_eq!(expected_commit, "abc123");
                assert_eq!(review, "已核对差异与验证");
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }
    #[test]
    fn review_cancel_returns_to_normal_input() {
        let mut app = App::live();
        app.review = Some(ReviewForm {
            pane: 0,
            task_id: "t".into(),
            commit: "c".into(),
            status: "completed".into(),
            integration: "pending".into(),
            summary: String::new(),
            diff: String::new(),
            input: "half".into(),
            scroll: 0,
        });
        app.key(press(KeyCode::Esc));
        assert!(app.review.is_none());
    }
    #[test]
    fn compact_form_carries_the_probed_revision() {
        let mut app = App::live();
        app.key(ctrl('p'));
        assert!(matches!(app.outbox.as_slice(), [Action::OpenCompact]));
        app.compact = Some(CompactForm {
            revision: 42,
            input: String::new(),
            note: String::new(),
        });
        app.key(press(KeyCode::Enter));
        assert!(app.compact.is_some(), "empty summary must not compact");
        app.paste("已核对记录");
        app.key(press(KeyCode::Enter));
        assert!(app.compact.is_none());
        match app.outbox.last() {
            Some(Action::Compact {
                summary,
                expected_revision,
            }) => {
                assert_eq!(summary, "已核对记录");
                assert_eq!(*expected_revision, 42);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }
    #[test]
    fn create_form_parses_validation_and_submits() {
        let mut app = App::live();
        app.key(ctrl('n'));
        assert!(matches!(app.outbox.as_slice(), [Action::OpenCreate]));
        app.create = Some(CreateForm {
            repository: "/tmp/repo".into(),
            validation: "cargo test\ncargo clippy -- -D warnings".into(),
            requirement: "加缓存".into(),
            field: 0,
            note: String::new(),
        });
        app.key(press(KeyCode::Tab));
        assert_eq!(app.create.as_ref().unwrap().field, 1);
        app.key(press(KeyCode::Enter));
        assert!(app.create.is_none());
        match app.outbox.last() {
            Some(Action::CreateSession {
                repository,
                validation,
                requirement,
            }) => {
                assert_eq!(repository, "/tmp/repo");
                assert_eq!(
                    validation,
                    &vec![
                        vec!["cargo".to_string(), "test".to_string()],
                        vec![
                            "cargo".to_string(),
                            "clippy".to_string(),
                            "--".to_string(),
                            "-D".to_string(),
                            "warnings".to_string()
                        ]
                    ]
                );
                assert_eq!(requirement, "加缓存");
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }
    #[test]
    fn create_form_requires_repository_and_validation() {
        let mut app = App::live();
        app.create = Some(CreateForm {
            repository: String::new(),
            validation: String::new(),
            requirement: String::new(),
            field: 2,
            note: String::new(),
        });
        app.key(press(KeyCode::Enter));
        {
            let form = app.create.as_ref().unwrap();
            assert_eq!(form.field, 0);
            assert!(form.note.contains("仓库"));
        }
        app.create.as_mut().unwrap().repository = "/tmp/repo".into();
        app.key(press(KeyCode::Enter));
        let form = app.create.as_ref().unwrap();
        assert_eq!(form.field, 1, "validation is required next");
        assert!(form.note.contains("验证命令"));
        assert!(app.outbox.is_empty(), "nothing must be sent while invalid");
    }
    fn agent(id: &str) -> AgentForm {
        AgentForm {
            pane: 0,
            id: id.into(),
            role: "coordinator",
            model: "planner".into(),
            base_url: "https://example.invalid/v1".into(),
            protocol: "responses".into(),
            context_bytes: "65536".into(),
            field: 0,
            note: String::new(),
        }
    }
    #[test]
    fn agent_form_requires_a_valid_endpoint_and_submits() {
        let mut app = App::live();
        app.agent = Some(agent("11111111-2222-4333-8444-555555555555"));
        app.key(press(KeyCode::Enter));
        match app.outbox.last() {
            Some(Action::ConfigureAgent {
                pane,
                id,
                role,
                model,
                base_url,
                protocol,
                context_bytes,
            }) => {
                assert_eq!(*pane, 0);
                assert_eq!(id, "11111111-2222-4333-8444-555555555555");
                assert_eq!(*role, "coordinator");
                assert_eq!(model, "planner");
                assert_eq!(base_url, "https://example.invalid/v1");
                assert_eq!(protocol, "responses");
                assert_eq!(*context_bytes, 65536);
            }
            other => panic!("unexpected action: {other:?}"),
        }
        assert!(app.agent.is_none(), "a valid form is consumed");
    }
    #[test]
    fn agent_form_rejects_empty_model_bad_url_and_budget() {
        let mut app = App::live();
        app.agent = Some(agent("id"));
        app.agent.as_mut().unwrap().model = "  ".into();
        app.key(press(KeyCode::Enter));
        assert!(app.agent.is_some(), "empty model must not submit");
        assert_eq!(app.agent.as_ref().unwrap().field, 0);
        assert!(app.outbox.is_empty());

        app.agent.as_mut().unwrap().model = "planner".into();
        app.agent.as_mut().unwrap().base_url = "http://example.com/v1".into();
        app.key(press(KeyCode::Enter));
        assert_eq!(app.agent.as_ref().unwrap().field, 1);
        assert!(app.agent.as_ref().unwrap().note.contains("http"));
        assert!(app.outbox.is_empty());

        app.agent.as_mut().unwrap().base_url = "https://example.invalid/v1".into();
        app.agent.as_mut().unwrap().context_bytes = "1024".into();
        app.key(press(KeyCode::Enter));
        assert_eq!(app.agent.as_ref().unwrap().field, 3);
        assert!(app.agent.as_ref().unwrap().note.contains("4096"));
        assert!(app.outbox.is_empty());
    }
    #[test]
    fn agent_form_toggles_protocol_cycles_fields_and_cancels() {
        let mut app = App::live();
        app.agent = Some(agent("id"));
        app.key(press(KeyCode::Tab));
        app.key(press(KeyCode::Tab));
        assert_eq!(app.agent.as_ref().unwrap().field, 2);
        app.key(press(KeyCode::Char(' ')));
        assert_eq!(app.agent.as_ref().unwrap().protocol, "chat_completions");
        app.key(press(KeyCode::Right));
        assert_eq!(app.agent.as_ref().unwrap().protocol, "responses");
        app.key(press(KeyCode::Backspace));
        assert_eq!(
            app.agent.as_ref().unwrap().protocol,
            "responses",
            "the protocol field is not text-editable"
        );
        app.key(press(KeyCode::Esc));
        assert!(app.agent.is_none());
        assert!(app.outbox.is_empty());
    }
    #[test]
    fn agent_form_paste_is_sanitized_per_field() {
        let mut app = App::live();
        app.agent = Some(agent("id"));
        app.agent.as_mut().unwrap().field = 1;
        app.agent.as_mut().unwrap().base_url.clear();
        app.paste("https://a\nb\t/c");
        assert_eq!(app.agent.as_ref().unwrap().base_url, "https://a b /c");
        app.agent.as_mut().unwrap().field = 3;
        app.agent.as_mut().unwrap().context_bytes.clear();
        app.paste("12a34\n56789000");
        assert_eq!(app.agent.as_ref().unwrap().context_bytes, "1234567");
    }
    #[test]
    fn base_url_check_mirrors_the_host_rule() {
        assert!(check_base_url("https://api.example.com/v1").is_ok());
        assert!(check_base_url("http://localhost:8080/v1").is_ok());
        assert!(check_base_url("http://127.0.0.1/v1").is_ok());
        assert!(check_base_url("http://[::1]:9/v1").is_ok());
        assert!(check_base_url("").is_err());
        assert!(check_base_url("ftp://example.com").is_err());
        assert!(check_base_url("https://").is_err());
        assert!(check_base_url("http://example.com").is_err());
        assert!(check_base_url("https://user:pw@example.com").is_err());
        assert!(check_base_url("https://example.com/v1?x=1").is_err());
        assert!(check_base_url("https://example.com/v1#f").is_err());
    }
    #[test]
    fn output_view_opens_scrolls_refreshes_and_closes() {
        let mut app = App::live();
        app.key(ctrl('o'));
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::OpenOutput { pane: 0 }]
        ));
        app.output = Some(OutputForm {
            pane: 0,
            task_id: "task-1".into(),
            status: "工作中".into(),
            result: "已完成一半".into(),
            draft: "正在写…".into(),
            body: String::new(),
            diff: String::new(),
            artifacts: "full result · 12 字节 · /tmp/x.txt".into(),
            scroll: 0,
            live: true,
        });
        app.key(press(KeyCode::PageDown));
        assert_eq!(app.output.as_ref().unwrap().scroll, 10);
        app.key(press(KeyCode::PageUp));
        app.key(press(KeyCode::PageUp));
        assert_eq!(
            app.output.as_ref().unwrap().scroll,
            0,
            "scrolling stops at the top"
        );
        app.key(ctrl('r'));
        assert!(matches!(app.outbox.last(), Some(Action::RefreshOutput)));
        app.key(press(KeyCode::Esc));
        assert!(app.output.is_none());
    }
    #[test]
    fn pane_input_edits_at_the_caret() {
        let mut app = App::demo();
        app.paste("abcd");
        app.key(press(KeyCode::Left));
        app.key(press(KeyCode::Left));
        app.paste("X");
        assert_eq!(app.panes[0].input, "abXcd");
        assert_eq!(app.panes[0].input.cursor, 3);
        app.key(press(KeyCode::Backspace));
        assert_eq!(app.panes[0].input, "abcd");
        app.key(press(KeyCode::Delete));
        assert_eq!(app.panes[0].input, "abd");
        app.key(press(KeyCode::Home));
        assert_eq!(app.panes[0].input.cursor, 0);
        app.paste("Y");
        assert_eq!(app.panes[0].input, "Yabd");
        app.key(press(KeyCode::End));
        assert_eq!(app.panes[0].input.cursor, 4);
        app.paste("Z");
        assert_eq!(app.panes[0].input, "YabdZ");
        // Arrow keys never move the caret off the line.
        for _ in 0..8 {
            app.key(press(KeyCode::Right));
        }
        assert_eq!(app.panes[0].input.cursor, 5);
        for _ in 0..8 {
            app.key(press(KeyCode::Left));
        }
        assert_eq!(app.panes[0].input.cursor, 0);
        app.key(press(KeyCode::Backspace));
        assert_eq!(
            app.panes[0].input, "YabdZ",
            "backspace at the start is a no-op"
        );
    }
    #[test]
    fn pane_input_edits_multibyte_text_at_char_boundaries() {
        let mut app = App::demo();
        app.paste("中文ab");
        app.key(press(KeyCode::Left));
        app.key(press(KeyCode::Backspace));
        assert_eq!(app.panes[0].input, "中文b");
        assert_eq!(app.panes[0].input.cursor, 6);
        app.key(press(KeyCode::Home));
        app.key(press(KeyCode::Delete));
        assert_eq!(
            app.panes[0].input, "文b",
            "Delete removes one whole character"
        );
        app.paste("新");
        assert_eq!(app.panes[0].input, "新文b");
    }
    #[test]
    fn pane_input_recalls_sent_lines_and_restores_the_draft() {
        let mut app = App::demo();
        assert_eq!(app.panes[0].sent.len(), 0);
        for message in ["first line", "second line"] {
            app.paste(message);
            app.key(press(KeyCode::Enter));
        }
        assert_eq!(app.panes[0].sent, vec!["first line", "second line"]);
        app.paste("draft");
        app.key(press(KeyCode::Up));
        assert_eq!(app.panes[0].input, "second line");
        assert_eq!(
            app.panes[0].input.cursor, 11,
            "recall puts the caret at the end"
        );
        app.key(press(KeyCode::Up));
        assert_eq!(app.panes[0].input, "first line");
        app.key(press(KeyCode::Up));
        assert_eq!(
            app.panes[0].input, "first line",
            "the oldest entry is sticky"
        );
        app.key(press(KeyCode::Down));
        assert_eq!(app.panes[0].input, "second line");
        app.key(press(KeyCode::Down));
        assert_eq!(
            app.panes[0].input, "draft",
            "past the newest the draft comes back"
        );
        app.key(press(KeyCode::Down));
        assert_eq!(
            app.panes[0].input, "draft",
            "Down without a recall is a no-op"
        );
        // Immediate repeats are not stored twice.
        app.panes[0].input.clear();
        app.paste("same");
        app.key(press(KeyCode::Enter));
        app.paste("same");
        app.key(press(KeyCode::Enter));
        assert_eq!(app.panes[0].sent.last().map(String::as_str), Some("same"));
        assert_eq!(app.panes[0].sent.len(), 3);
    }
    #[test]
    fn pane_input_history_is_bounded_and_per_pane() {
        let mut app = App::demo();
        for index in 0..120 {
            app.paste(&format!("line {index}"));
            app.key(press(KeyCode::Enter));
        }
        assert_eq!(app.panes[0].sent.len(), 100);
        assert_eq!(
            app.panes[0].sent.first().map(String::as_str),
            Some("line 20")
        );
        app.key(press(KeyCode::Tab));
        app.key(press(KeyCode::Up));
        assert_eq!(app.panes[1].input, "", "history is per pane");
    }
    #[test]
    fn probe_needs_a_configured_agent_then_queues() {
        let mut app = App::live();
        app.key(ctrl('t'));
        assert!(
            app.outbox.is_empty(),
            "an unconfigured pane cannot be probed"
        );
        assert!(app.feedback.contains("尚未配置"));
        app.panes[2].agent_id = Some("w".into());
        app.focused = 2;
        app.key(ctrl('t'));
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::TestConnection { pane: 2 }]
        ));
    }
    #[test]
    fn publish_form_toggles_kind_and_requires_content() {
        let mut app = App::live();
        app.key(ctrl('u'));
        assert!(matches!(app.outbox.as_slice(), [Action::OpenPublish]));
        app.publish = Some(PublishForm {
            kind: 0,
            input: String::new(),
            note: String::new(),
        });
        app.key(press(KeyCode::Enter));
        assert!(app.publish.is_some(), "empty content must not publish");
        assert!(app.publish.as_ref().unwrap().note.contains("内容"));
        app.key(press(KeyCode::Right));
        assert_eq!(app.publish.as_ref().unwrap().kind, 1, "kind cycles forward");
        app.key(press(KeyCode::Char(' ')));
        {
            let form = app.publish.as_ref().unwrap();
            assert_eq!(form.kind, 1, "a space in the text must not change the kind");
            assert_eq!(form.input, " ", "spaces are part of the constraint text");
        }
        app.publish.as_mut().unwrap().input.clear();
        app.key(press(KeyCode::Left));
        app.key(press(KeyCode::Left));
        assert_eq!(app.publish.as_ref().unwrap().kind, 2, "kind wraps backward");
        app.paste("必须先保证兼容性");
        app.key(press(KeyCode::Enter));
        assert!(app.publish.is_none(), "a valid form is consumed");
        match app.outbox.last() {
            Some(Action::Publish { kind, data }) => {
                assert_eq!(*kind, "suggestion");
                assert_eq!(data, "必须先保证兼容性");
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }
    #[test]
    fn publish_form_escape_cancels_and_paste_is_sanitized() {
        let mut app = App::live();
        app.publish = Some(PublishForm {
            kind: 1,
            input: String::new(),
            note: String::new(),
        });
        app.paste("第一行\n第二行\x1b\t结束");
        assert_eq!(app.publish.as_ref().unwrap().input, "第一行 第二行 结束");
        app.paste(&"中".repeat(5000));
        assert!(app.publish.as_ref().unwrap().input.len() <= 4096);
        assert!(app.feedback.contains("上限"));
        app.key(press(KeyCode::Esc));
        assert!(app.publish.is_none());
        assert!(app.outbox.is_empty(), "cancel must not publish");
    }
    #[test]
    fn live_cancel_and_refresh_use_the_outbox() {
        let mut app = App::live();
        app.focused = 1;
        app.key(ctrl('c'));
        assert_eq!(app.panes[1].status, "停止中");
        app.key(ctrl('r'));
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::Cancel { pane: 1 }, Action::Refresh]
        ));
    }
    fn approval(id: &str) -> ApprovalItem {
        ApprovalItem {
            id: id.into(),
            method: "item/commandExecution/requestApproval".into(),
            agent_id: "a".into(),
            task_id: "t".into(),
            detail: "cargo test".into(),
        }
    }
    #[test]
    fn approval_modal_cycles_and_decides() {
        let mut app = App::live();
        app.approvals = vec![approval("first"), approval("second")];
        app.key(press(KeyCode::Tab));
        assert_eq!(app.approval_index, 1);
        app.key(press(KeyCode::Char('n')));
        assert_eq!(app.approvals.len(), 1);
        match app.outbox.as_slice() {
            [Action::Decide { id, accept }] => {
                assert_eq!(id, "second");
                assert!(!accept);
            }
            other => panic!("unexpected outbox: {other:?}"),
        }
        app.key(press(KeyCode::Char('y')));
        assert!(app.approvals.is_empty());
        assert!(matches!(
            app.outbox.as_slice(),
            [_, Action::Decide { accept: true, .. }]
        ));
    }
    #[test]
    fn credential_form_is_masked_and_submits() {
        let mut app = App::live();
        app.panes[0].agent_id = Some("c".into());
        app.key(ctrl('k'));
        assert!(app.credential.is_some());
        app.paste("sk-secret\nkey");
        assert_eq!(app.credential.as_ref().unwrap().input, "sk-secretkey");
        assert!(app.panes[0].input.is_empty());
        assert!(app.panes[0].history.is_empty());
        app.key(press(KeyCode::Enter));
        assert!(app.credential.is_none());
        match app.outbox.as_slice() {
            [Action::SaveCredential { pane, key }] => {
                assert_eq!(*pane, 0);
                assert_eq!(key, "sk-secretkey");
            }
            other => panic!("unexpected outbox: {other:?}"),
        }
    }
    #[test]
    fn credential_form_delete_and_cancel() {
        let mut app = App::live();
        app.panes[2].agent_id = Some("d".into());
        app.focused = 2;
        app.key(ctrl('k'));
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(app.credential.is_none());
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::DeleteCredential { pane: 2 }]
        ));
        app.key(ctrl('k'));
        app.key(press(KeyCode::Esc));
        assert!(app.credential.is_none());
    }
    #[test]
    fn quit_works_while_a_modal_is_open() {
        let mut app = App::live();
        app.approvals = vec![approval("a")];
        assert!(!app.key(ctrl('q')));
        assert!(app.confirm_quit);
        assert!(app.key(press(KeyCode::Enter)));
        assert_eq!(app.approvals.len(), 1, "quit must not decide the approval");

        let mut app = App::live();
        app.panes[0].agent_id = Some("c".into());
        app.key(ctrl('k'));
        assert!(app.credential.is_some());
        app.key(ctrl('q'));
        assert!(app.confirm_quit);
    }
    #[test]
    fn credential_form_needs_a_configured_agent() {
        let mut app = App::live();
        app.key(ctrl('k'));
        assert!(app.credential.is_none());
        assert!(app.feedback.contains("尚未配置"));
    }
    #[test]
    fn stop_does_not_affect_other_panes() {
        let mut app = App::demo();
        app.focused = 1;
        app.key(ctrl('c'));
        assert_eq!(app.panes[1].status, "已停止");
        assert!(!app.panes[0].stopped);
        app.key(ctrl('c'));
        assert_eq!(
            app.panes[1]
                .history
                .iter()
                .filter(|l| l.to_string().contains("任务已停止"))
                .count(),
            1
        );
    }
    #[test]
    fn quit_requires_confirmation_and_escape_cancels() {
        let mut app = App::demo();
        assert!(!app.key(ctrl('q')));
        assert!(app.confirm_quit);
        assert!(!app.key(press(KeyCode::Esc)));
        assert!(!app.confirm_quit);
        app.key(ctrl('q'));
        assert!(app.key(press(KeyCode::Enter)));
    }
    #[test]
    fn paste_sanitizes_control_characters_and_preserves_unicode() {
        let mut app = App::demo();
        app.paste("中文\n第二行\x1b\t结束");
        assert_eq!(app.panes[0].input, "中文 第二行 结束");
        app.key(press(KeyCode::Backspace));
        assert_eq!(app.panes[0].input, "中文 第二行 结");
        app.paste(&"中".repeat(5000));
        assert!(app.panes[0].input.text.len() <= 4096);
    }
    #[test]
    fn zoom_shared_and_scroll_are_independent() {
        let mut app = App::demo();
        app.key(press(KeyCode::F(2)));
        assert!(app.zoom);
        app.key(press(KeyCode::F(3)));
        assert!(app.shared);
        app.key(press(KeyCode::PageUp));
        assert_eq!(app.panes[0].scroll, 8);
        app.key(press(KeyCode::End));
        assert_eq!(app.panes[0].scroll, 0);
        app.key(press(KeyCode::Esc));
        assert!(!app.zoom && !app.shared);
    }
    #[test]
    fn history_is_bounded_and_empty_input_does_not_send() {
        let mut app = App::demo();
        let length = app.panes[0].history.len();
        app.paste("   ");
        app.key(press(KeyCode::Enter));
        assert_eq!(app.panes[0].history.len(), length);
        for _ in 0..300 {
            app.paste("test");
            app.key(press(KeyCode::Enter));
        }
        assert_eq!(app.panes[0].history.len(), 200);
    }

    fn task_form() -> TaskForm {
        TaskForm {
            pane: 0,
            goal: TextBuf::default(),
            acceptance: TextBuf::default(),
            dependencies: vec![
                Dependency {
                    id: "t1".into(),
                    label: "工作1 · 工作中 · 实现重试".into(),
                    selectable: true,
                    note: "未完成，新任务会等待它".into(),
                    selected: false,
                },
                Dependency {
                    id: "t2".into(),
                    label: "工作2 · 失败 · 坏的构建".into(),
                    selectable: false,
                    note: "前置已失败：勾选后新任务会直接阻塞".into(),
                    selected: false,
                },
            ],
            index: 0,
            coding: false,
            field: 0,
            note: String::new(),
        }
    }
    #[test]
    fn text_buffer_edits_line_wise_at_char_boundaries() {
        let mut buffer = TextBuf::end("第一行\n第二行");
        assert_eq!(buffer.cursor, buffer.text.len());
        buffer.move_line_start();
        assert_eq!(buffer.cursor, 10, "Home stops after the newline");
        buffer.move_line_end();
        assert_eq!(buffer.cursor, buffer.text.len());
        let mut buffer = TextBuf::end("中文");
        buffer.move_left();
        buffer.backspace();
        assert_eq!(
            buffer, "文",
            "Backspace removes the character before the caret"
        );
        assert_eq!(buffer.cursor, 0);
        buffer.delete();
        assert_eq!(buffer, "");
        assert!(buffer.is_empty());
        buffer.insert_char('a', 1);
        assert!(!buffer.insert_char('b', 1), "the byte cap is enforced");
        assert_eq!(buffer, "a");
    }
    #[test]
    fn task_editor_opens_only_for_a_configured_pane() {
        let mut app = App::live();
        app.key(ctrl('f'));
        assert!(app.outbox.is_empty(), "an unconfigured pane cannot compose");
        assert!(app.feedback.contains("尚未配置"));
        assert!(app.task.is_none());
        app.panes[2].agent_id = Some("w".into());
        app.focused = 2;
        app.key(ctrl('f'));
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::OpenTask { pane: 2 }]
        ));
    }
    #[test]
    fn task_editor_maps_fields_and_submits_the_full_parameter_set() {
        let mut app = App::live();
        app.task = Some(task_form());
        app.paste("实现退避重试");
        assert_eq!(app.task.as_ref().unwrap().goal, "实现退避重试");
        app.key(press(KeyCode::Tab));
        assert_eq!(app.task.as_ref().unwrap().field, 1);
        app.paste("测试通过且取消不误伤");
        app.key(press(KeyCode::Tab));
        assert_eq!(app.task.as_ref().unwrap().field, 2);
        app.key(press(KeyCode::Char(' ')));
        assert!(app.task.as_ref().unwrap().dependencies[0].selected);
        app.key(press(KeyCode::Down));
        assert_eq!(app.task.as_ref().unwrap().index, 1);
        app.key(press(KeyCode::Char(' ')));
        assert!(
            !app.task.as_ref().unwrap().dependencies[1].selected,
            "a failed dependency can never satisfy the task"
        );
        assert!(app.task.as_ref().unwrap().note.contains("阻塞"));
        app.key(press(KeyCode::Down));
        assert_eq!(app.task.as_ref().unwrap().index, 1, "the highlight clamps");
        app.key(press(KeyCode::Tab));
        assert_eq!(app.task.as_ref().unwrap().field, 3);
        app.key(press(KeyCode::Char(' ')));
        assert!(app.task.as_ref().unwrap().coding);
        app.key(ctrl('s'));
        assert!(app.task.is_none());
        match app.outbox.as_slice() {
            [
                Action::CreateTask {
                    pane,
                    goal,
                    acceptance,
                    dependencies,
                    coding,
                },
            ] => {
                assert_eq!(*pane, 0);
                assert_eq!(goal, "实现退避重试");
                assert_eq!(acceptance, "测试通过且取消不误伤");
                assert_eq!(dependencies, &vec!["t1".to_string()]);
                assert!(coding);
            }
            other => panic!("unexpected outbox: {other:?}"),
        }
    }
    #[test]
    fn task_editor_requires_a_goal_and_an_acceptance() {
        let mut app = App::live();
        app.task = Some(task_form());
        app.key(ctrl('s'));
        {
            let form = app.task.as_ref().unwrap();
            assert_eq!(form.field, 0);
            assert!(form.note.contains("任务目标"));
        }
        assert!(app.outbox.is_empty());
        app.paste("实现重试");
        app.key(ctrl('s'));
        {
            let form = app.task.as_ref().unwrap();
            assert_eq!(form.field, 1);
            assert!(form.note.contains("验收标准"));
        }
        assert!(app.outbox.is_empty(), "the editor requires acceptance text");
        app.paste("测试通过");
        app.key(ctrl('s'));
        assert!(app.task.is_none());
        assert!(matches!(app.outbox.as_slice(), [Action::CreateTask { .. }]));
    }
    #[test]
    fn task_editor_enter_inserts_newlines_and_space_stays_text() {
        let mut app = App::live();
        app.task = Some(task_form());
        app.paste("第一行");
        app.key(press(KeyCode::Enter));
        app.paste("第二行");
        assert_eq!(app.task.as_ref().unwrap().goal, "第一行\n第二行");
        app.key(press(KeyCode::Char(' ')));
        assert_eq!(app.task.as_ref().unwrap().goal, "第一行\n第二行 ");
        app.key(press(KeyCode::Home));
        assert_eq!(
            app.task.as_ref().unwrap().goal.cursor,
            10,
            "Home is line-level"
        );
        app.key(press(KeyCode::End));
        assert_eq!(
            app.task.as_ref().unwrap().goal.cursor,
            app.task.as_ref().unwrap().goal.text.len()
        );
        app.key(press(KeyCode::Esc));
        assert!(app.task.is_none());
        assert!(app.outbox.is_empty(), "cancel must not dispatch");
    }
    #[test]
    fn task_editor_caps_dependencies_at_the_limit() {
        let mut app = App::live();
        let mut form = task_form();
        form.dependencies = (0..10)
            .map(|index| Dependency {
                id: format!("t{index}"),
                label: format!("任务 {index}"),
                selectable: true,
                note: String::new(),
                selected: false,
            })
            .collect();
        app.task = Some(form);
        app.task.as_mut().unwrap().field = 2;
        for _ in 0..=MAX_DEPENDENCIES {
            app.key(press(KeyCode::Char(' ')));
            app.key(press(KeyCode::Down));
        }
        let form = app.task.as_ref().unwrap();
        assert_eq!(form.selected().len(), MAX_DEPENDENCIES);
        assert!(form.note.contains("最多"), "the cap is explained");
        app.task.as_mut().unwrap().index = 0;
        app.key(press(KeyCode::Up));
        assert_eq!(app.task.as_ref().unwrap().index, 0);
        app.key(press(KeyCode::PageDown));
        assert_eq!(app.task.as_ref().unwrap().index, 5);
        assert_eq!(
            app.task.as_ref().unwrap().field,
            2,
            "paging scrolls the list instead of cycling fields"
        );
    }
    #[test]
    fn task_editor_paste_keeps_newlines_and_ignores_text_in_the_list() {
        let mut app = App::live();
        app.task = Some(task_form());
        app.paste("第一行\n第二行\ttab\x1b");
        assert_eq!(app.task.as_ref().unwrap().goal, "第一行\n第二行 tab");
        // Home is line-level, so the insertion lands at the start of the caret's line.
        app.key(press(KeyCode::Home));
        app.paste("X");
        assert_eq!(app.task.as_ref().unwrap().goal, "第一行\nX第二行 tab");
        app.task.as_mut().unwrap().field = 2;
        app.paste("ignored");
        assert_eq!(app.task.as_ref().unwrap().goal.text, "第一行\nX第二行 tab");
        assert_eq!(app.task.as_ref().unwrap().acceptance, "", "no stray text");
    }
    fn detail_form() -> DetailForm {
        DetailForm {
            pane: 1,
            task_id: "aaaaaaaa-1111-4111-8111-111111111111".into(),
            status: "等待依赖".into(),
            coding: true,
            context_version: 7,
            goal: "实现重试".into(),
            acceptance: "测试通过".into(),
            worktree: "/tmp/wt".into(),
            base_commit: "89461ea68338".into(),
            commit: String::new(),
            integration: "未集成".into(),
            dependencies: vec![("bbbbbbbb…".into(), "已完成".into())],
            blocked: "dependency did not complete".into(),
            delegation: "已由宿主续跑 2 次".into(),
            result: String::new(),
            artifacts: String::new(),
            body: String::new(),
            scroll: 0,
        }
    }
    #[test]
    fn detail_view_opens_only_for_a_task_and_scrolls() {
        let mut app = App::live();
        app.key(ctrl('d'));
        assert!(app.outbox.is_empty(), "an unconfigured pane has no task");
        assert!(app.feedback.contains("尚未配置"));
        app.panes[0].agent_id = Some("c".into());
        app.key(ctrl('d'));
        assert!(matches!(
            app.outbox.as_slice(),
            [Action::OpenDetail { pane: 0 }]
        ));
        app.outbox.clear();
        app.detail = Some(detail_form());
        app.key(press(KeyCode::PageDown));
        assert_eq!(app.detail.as_ref().unwrap().scroll, 10);
        app.key(press(KeyCode::PageUp));
        app.key(press(KeyCode::PageUp));
        assert_eq!(app.detail.as_ref().unwrap().scroll, 0);
        app.key(ctrl('r'));
        assert!(matches!(app.outbox.as_slice(), [Action::RefreshDetail]));
        app.key(press(KeyCode::Esc));
        assert!(app.detail.is_none());
        assert!(app.feedback.contains("已关闭"));
        // Demo mode has no host snapshot to read.
        let mut app = App::demo();
        app.key(ctrl('d'));
        assert!(app.outbox.is_empty() && app.detail.is_none());
    }
    fn session_picker() -> SessionList {
        SessionList {
            items: vec![
                SessionItem {
                    id: "aaaaaaaa-1111-4111-8111-111111111111".into(),
                    repository: "/tmp/repo".into(),
                    branch: "codex/collab-a".into(),
                    base_commit: "89461ea68338".into(),
                    validations: 2,
                    requirements: vec!["[用户] 保留公开接口".into()],
                    scanned: true,
                    current: false,
                },
                SessionItem {
                    id: "bbbbbbbb-2222-4222-8222-222222222222".into(),
                    repository: "/tmp/repo".into(),
                    branch: "codex/collab-b".into(),
                    base_commit: "89461ea68338".into(),
                    validations: 1,
                    requirements: Vec::new(),
                    scanned: true,
                    current: true,
                },
            ],
            index: 1,
            note: String::new(),
        }
    }
    #[test]
    fn session_picker_switches_only_to_another_session() {
        let mut app = App::live();
        app.key(ctrl('l'));
        assert!(matches!(app.outbox.as_slice(), [Action::OpenSessions]));
        app.outbox.clear();
        app.sessions = Some(session_picker());
        app.key(press(KeyCode::Up));
        assert_eq!(app.sessions.as_ref().unwrap().index, 0);
        // The list stays open at the top edge.
        app.key(press(KeyCode::Up));
        assert_eq!(app.sessions.as_ref().unwrap().index, 0);
        app.key(press(KeyCode::Enter));
        assert!(app.sessions.is_none());
        match app.outbox.as_slice() {
            [Action::SwitchSession { id }] => {
                assert_eq!(id, "aaaaaaaa-1111-4111-8111-111111111111");
            }
            other => panic!("unexpected outbox: {other:?}"),
        }
        // Entering the session that is already open closes the picker without a request.
        app.outbox.clear();
        app.sessions = Some(session_picker());
        app.key(press(KeyCode::Enter));
        assert!(app.sessions.is_none());
        assert!(app.outbox.is_empty());
        assert!(app.feedback.contains("已在该会话中"));
    }
    #[test]
    fn session_picker_is_live_only_and_esc_closes() {
        let mut app = App::demo();
        app.key(ctrl('l'));
        assert!(app.sessions.is_none(), "demo mode has no sessions to pick");
        assert!(app.outbox.is_empty());
        let mut app = App::live();
        app.sessions = Some(session_picker());
        app.key(press(KeyCode::Esc));
        assert!(app.sessions.is_none());
        assert!(app.feedback.contains("已关闭"));
        // An empty list still answers Enter with a note instead of switching.
        let mut app = App::live();
        app.sessions = Some(SessionList {
            items: Vec::new(),
            index: 0,
            note: String::new(),
        });
        app.key(press(KeyCode::Enter));
        assert!(app.sessions.as_ref().unwrap().note.contains("Ctrl+N"));
        assert!(app.outbox.is_empty());
    }
    #[test]
    fn reset_session_view_drops_previous_session_data() {
        let mut app = App::live();
        app.shared_records.push(line("[用户] 旧会话约束", ACCENT));
        app.approvals.push(ApprovalItem {
            id: "a1".into(),
            method: "m".into(),
            agent_id: "c".into(),
            task_id: "t".into(),
            detail: "d".into(),
        });
        app.approval_index = 3;
        app.panes[0].agent_id = Some("c".into());
        app.panes[0].model = "planner-model".into();
        app.panes[0].input.set("半截输入");
        app.panes[0].history.push(line("旧记录", ACCENT));
        app.focused = 2;
        app.coding = true;
        app.output = Some(OutputForm {
            pane: 0,
            task_id: "t".into(),
            status: "已完成".into(),
            result: "r".into(),
            draft: String::new(),
            body: String::new(),
            diff: String::new(),
            artifacts: String::new(),
            scroll: 0,
            live: false,
        });
        app.reset_session_view();
        assert!(app.shared_records.is_empty(), "shared records leaked over");
        assert!(app.approvals.is_empty() && app.approval_index == 0);
        assert!(app.output.is_none());
        for pane in &app.panes {
            assert!(pane.agent_id.is_none());
            assert_eq!(pane.model, "—");
            assert!(pane.history.is_empty());
            assert!(pane.input.is_empty());
        }
        assert_eq!(app.focused, 2, "focus is a global choice");
        assert!(app.coding, "the coding toggle is a global choice");
    }
}
