use crate::app::{
    ACCENT, App, GREEN, MAX_DEPENDENCIES, MUTED, PUBLISH_KINDS, TaskForm, TextBuf, WARN, line,
    publish_kind_label, short_id,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

const BACKGROUND: Color = Color::Rgb(23, 28, 35);
const FOREGROUND: Color = Color::Rgb(211, 219, 230);
const BORDER: Color = Color::Rgb(58, 69, 84);

pub fn pane_areas(area: Rect, focused: usize, zoom: bool) -> [Rect; 4] {
    let mut result = [Rect::default(); 4];
    if zoom || area.width < 100 || area.height < 24 {
        result[focused] = area;
    } else {
        let columns = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        result[0] = columns[0];
        let workers = Layout::vertical([Constraint::Fill(1); 3]).split(columns[1]);
        result[1..].copy_from_slice(&workers);
    }
    result
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let full = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND).fg(FOREGROUND)),
        full,
    );
    if full.is_empty() {
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(if app.shared { 5 } else { 0 }),
        Constraint::Length(3),
    ])
    .split(full);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("›_ codex-collab", Style::default().fg(ACCENT)),
                Span::raw(format!("  {}", app.session_label)),
            ]),
            Line::styled(
                if rows[1].width < 100 || rows[1].height < 24 {
                    "终端较小，仅显示当前模型；Tab 切换，F2 切换放大。"
                } else {
                    "左侧主控 / 右侧三个工作终端"
                },
                Style::default().fg(MUTED),
            ),
        ]),
        rows[0],
    );
    app.areas = pane_areas(rows[1], app.focused, app.zoom);
    for index in 0..4 {
        let area = app.areas[index];
        if !area.is_empty() {
            draw_pane(frame, app, index, area);
        }
    }
    if app.shared {
        let records = if app.shared_records.is_empty() {
            vec![line("（暂无共享记录）", MUTED)]
        } else {
            app.shared_records.clone()
        };
        frame.render_widget(
            Paragraph::new(records)
                .style(Style::default().fg(ACCENT))
                .block(
                    Block::bordered()
                        .title(" 共享记录 · F3 收起 ")
                        .border_style(Style::default().fg(BORDER)),
                ),
            rows[2],
        );
    }
    let focused = &app.panes[app.focused];
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!(
                    "[{} {}] 编码:{}  Tab 焦点  F2 放大  F3 共享  Ctrl+A 配置  Ctrl+L 会话  Ctrl+R 刷新  Ctrl+C 停止  Ctrl+Q 退出",
                    app.focused + 1,
                    focused.role,
                    if app.coding { "开" } else { "关" }
                ),
                Style::default().fg(ACCENT),
            ),
            Line::styled(
                "Ctrl+T 自检  Ctrl+U 约束  Ctrl+O 结果  Ctrl+F 编排  Ctrl+E 编码  Ctrl+G 审查集成  Ctrl+P 压缩  Ctrl+N 新建  Ctrl+K 密钥",
                Style::default().fg(ACCENT),
            ),
            Line::styled(app.feedback.as_str(), Style::default().fg(MUTED)),
        ]),
        rows[3],
    );
    if !app.approvals.is_empty() {
        let index = app.approval_index.min(app.approvals.len() - 1);
        let item = &app.approvals[index];
        let popup = centered(full, 88, 11);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(vec![
                line(
                    format!("Agent {}   任务 {}", item.agent_id, item.task_id),
                    MUTED,
                ),
                line(format!("操作：{}", item.method), FOREGROUND),
                line(item.detail.clone(), ACCENT),
                line(format!("({}/{})", index + 1, app.approvals.len()), MUTED),
                line("Y/Enter 批准 · N/Esc 拒绝 · Tab 切换", GREEN),
            ])
            .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
            .wrap(Wrap { trim: false })
            .block(
                Block::bordered()
                    .title(" 操作审批 · 仅批准本次操作 ")
                    .border_style(Style::default().fg(ACCENT)),
            ),
            popup,
        );
    }
    if let Some(form) = &app.credential {
        let agent = app.panes[form.pane]
            .agent_id
            .clone()
            .unwrap_or_else(|| "—".into());
        let masked = "•".repeat(form.input.chars().count().min(48));
        let popup = centered(full, 72, 7);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(vec![
                line(format!("Agent {agent}"), MUTED),
                line(format!("Key ❯ {masked}"), ACCENT),
                line("Enter 保存 · Ctrl+D 删除 · Esc 取消（不回显）", GREEN),
            ])
            .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
            .wrap(Wrap { trim: false })
            .block(
                Block::bordered()
                    .title(" 设置 API Key ")
                    .border_style(Style::default().fg(ACCENT)),
            ),
            popup,
        );
    }
    if let Some(form) = &app.create {
        let mut lines = Vec::new();
        push_field(
            &mut lines,
            "仓库路径（Git 绝对路径）",
            &form.repository,
            form.field == 0,
        );
        push_field(
            &mut lines,
            "验证命令（每行一条）",
            &form.validation,
            form.field == 1,
        );
        push_field(
            &mut lines,
            "需求（可选）",
            &form.requirement,
            form.field == 2,
        );
        lines.push(line(String::new(), FOREGROUND));
        if !form.note.is_empty() {
            lines.push(line(form.note.clone(), WARN));
        }
        lines.push(line(
            "Tab/↑↓ 切换字段 · Enter 创建 · Esc 取消（仅本地创建，不推送远端）",
            GREEN,
        ));
        let popup = centered(full, full.width.min(110), full.height.min(26));
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(" 新建协作会话 · 从 HEAD 创建协作分支与集成工作区 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if let Some(form) = &app.agent {
        let protocol = match form.protocol.as_str() {
            "chat_completions" => "Chat Completions",
            _ => "Responses",
        };
        let role = if form.role == "coordinator" {
            "主控（coordinator）"
        } else {
            "工作（worker）"
        };
        let mut lines = vec![
            line(format!("Agent {} · 角色 {role}", form.id), MUTED),
            line(String::new(), FOREGROUND),
        ];
        push_field(&mut lines, "模型", &form.model, form.field == 0);
        push_field(
            &mut lines,
            "Base URL（https，或本机 http）",
            &form.base_url,
            form.field == 1,
        );
        push_field(
            &mut lines,
            "协议（←/→/空格 切换）",
            protocol,
            form.field == 2,
        );
        push_field(
            &mut lines,
            "contextBytes（4096..1000000）",
            &form.context_bytes,
            form.field == 3,
        );
        lines.push(line(String::new(), FOREGROUND));
        if !form.note.is_empty() {
            lines.push(line(form.note.clone(), WARN));
        }
        lines.push(line(
            "Tab/↑↓ 切换字段 · Enter 配置 · Esc 取消（运行中的任务需先停止）",
            GREEN,
        ));
        let popup = centered(full, full.width.min(110), full.height.min(26));
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(" 配置 Agent · 端点、协议与上下文预算 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if let Some(list) = &app.sessions {
        let mut lines = Vec::new();
        const WINDOW: usize = 8;
        let start = list
            .index
            .saturating_sub(WINDOW / 2)
            .min(list.items.len().saturating_sub(WINDOW));
        for (offset, item) in list.items[start..].iter().take(WINDOW).enumerate() {
            let row = start + offset;
            let active = row == list.index;
            lines.push(line(
                format!(
                    "{} {} {} · 验证 {} 条{}",
                    if active { "▸" } else { " " },
                    if item.current { "●" } else { "○" },
                    short_id(&item.id),
                    item.validations,
                    if item.current { "  · 当前" } else { "" }
                ),
                if active { ACCENT } else { FOREGROUND },
            ));
        }
        if list.items.is_empty() {
            lines.push(line("（尚无会话；Ctrl+N 新建）", MUTED));
        }
        if list.items.len() > WINDOW {
            lines.push(line(
                format!("…（共 {} 个会话，↑↓ 移动）", list.items.len()),
                MUTED,
            ));
        }
        if let Some(item) = list.items.get(list.index) {
            lines.push(line(String::new(), FOREGROUND));
            lines.push(line(format!("仓库 {}", item.repository), MUTED));
            lines.push(line(format!("分支 {}", item.branch), MUTED));
            lines.push(line(format!("基线 {}", item.base_commit), MUTED));
            if item.requirements.is_empty() {
                lines.push(line(
                    if item.scanned {
                        "用户约束：无"
                    } else {
                        "用户约束：读取中…"
                    },
                    MUTED,
                ));
            } else {
                lines.push(line(
                    format!("用户约束 {} 条：", item.requirements.len()),
                    MUTED,
                ));
                for text in &item.requirements {
                    lines.push(line(text.clone(), ACCENT));
                }
            }
        }
        if !list.note.is_empty() {
            lines.push(line(list.note.clone(), WARN));
        }
        lines.push(line(
            "↑/↓ 选择 · Enter 切换 · Ctrl+R 重新读取 · Esc 关闭",
            GREEN,
        ));
        // Every picker line is short enough not to wrap, so size the popup to its content.
        let height = (lines.len() as u16 + 2).clamp(3, 26);
        let popup = centered(full, full.width.min(110), height);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(" 会话列表 · session/list 切换会话 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if let Some(form) = &app.output {
        let mut lines = vec![line(
            format!("任务 {} · {}", short_id(&form.task_id), form.status),
            MUTED,
        )];
        if !form.result.is_empty() {
            lines.push(line("结果摘要（持久化）：", MUTED));
            push_block(&mut lines, &form.result, 200);
        }
        if !form.draft.is_empty() {
            lines.push(line("运行中公开草稿：", MUTED));
            push_block(&mut lines, &form.draft, 200);
        }
        if !form.body.is_empty() {
            lines.push(line("完整结果（附件，本机只读）：", MUTED));
            push_block(&mut lines, &form.body, 400);
        }
        if !form.artifacts.is_empty() {
            lines.push(line("产物：", MUTED));
            for text in form.artifacts.lines() {
                lines.push(line(text.to_string(), MUTED));
            }
        }
        if form.result.is_empty() && form.draft.is_empty() && form.body.is_empty() {
            lines.push(line("（暂无输出；任务可能仍在排队。）", MUTED));
        }
        lines.push(line(
            if form.live {
                "运行中，自动刷新 · PgUp/PgDn 滚动 · Ctrl+R 刷新 · Esc 关闭"
            } else {
                "PgUp/PgDn 滚动 · Ctrl+R 刷新 · Esc 关闭"
            },
            GREEN,
        ));
        // Keep the key hints visible even on short terminals.
        let height = full.height.saturating_sub(2).clamp(3, 34);
        let popup = centered(full, full.width.min(120), height);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .scroll((form.scroll.min(u16::MAX as usize) as u16, 0))
                .block(
                    Block::bordered()
                        .title(" 任务输出 · 摘要、公开草稿与附件 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if let Some(form) = &app.task {
        let role = app.panes[form.pane].role.clone();
        let agent = app.panes[form.pane]
            .agent_id
            .clone()
            .unwrap_or_else(|| "—".into());
        let mut lines = vec![
            line(
                format!("窗格 {} {role} · Agent {agent}", form.pane + 1),
                MUTED,
            ),
            line(
                format!(
                    "编码 {} · 已选依赖 {} 项",
                    if form.coding {
                        "开（产物需集成）"
                    } else {
                        "关"
                    },
                    form.selected().len()
                ),
                MUTED,
            ),
            line(String::new(), FOREGROUND),
        ];
        push_caret_field(
            &mut lines,
            "任务目标（Enter 换行）",
            &form.goal,
            form.field == 0,
        );
        push_caret_field(
            &mut lines,
            "验收标准（Enter 换行）",
            &form.acceptance,
            form.field == 1,
        );
        push_dependencies(&mut lines, form);
        lines.push(line(
            format!(
                "{}编码任务（空格 切换）  {}",
                if form.field == 3 { "❯ " } else { "  " },
                if form.coding { "◉ 开" } else { "○ 关" }
            ),
            if form.field == 3 { ACCENT } else { MUTED },
        ));
        if !form.note.is_empty() {
            lines.push(line(form.note.clone(), WARN));
        }
        lines.push(line(
            "Tab 切换字段 · Enter 换行 · 空格 勾选 · Ctrl+S 派发 · Esc 取消（验收标准必填）",
            GREEN,
        ));
        // Keep the key hints visible even on short terminals.
        let height = full.height.saturating_sub(2).clamp(3, 34);
        let popup = centered(full, full.width.min(120), height);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(" 编排任务 · task/create 目标、验收与依赖 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if let Some(form) = &app.publish {
        let selector = PUBLISH_KINDS
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let mark = if index == form.kind { "◉" } else { "○" };
                format!("{mark} {}", publish_kind_label(kind))
            })
            .collect::<Vec<_>>()
            .join("   ");
        let mut lines = vec![
            line("会话级用户约束会写入共享记录并持续注入模型上下文", MUTED),
            line(String::new(), FOREGROUND),
            line(format!("类型  {selector}"), FOREGROUND),
            line(format!("内容 ❯ {}", form.input), ACCENT),
        ];
        if !form.note.is_empty() {
            lines.push(line(form.note.clone(), WARN));
        }
        lines.push(line(
            "←/→ 切换类型 · Enter 发布 · Esc 取消（需求与用户覆盖优先于模型决策）",
            GREEN,
        ));
        let popup = centered(full, full.width.min(110), full.height.min(18));
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(" 发布用户约束 · event/publish ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if let Some(form) = &app.compact {
        let mut lines = vec![
            line(format!("会话版本 {}", form.revision), MUTED),
            line(format!("摘要 ❯ {}", form.input), ACCENT),
        ];
        if !form.note.is_empty() {
            lines.push(line(form.note.clone(), WARN));
        }
        lines.push(line(
            "Enter 压缩 · Esc 取消（保留全部审计历史与用户约束）",
            GREEN,
        ));
        let popup = centered(full, full.width.min(110), full.height.min(18));
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(" 压缩上下文 · 写入检查点 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if let Some(form) = &app.review {
        let agent = app.panes[form.pane]
            .agent_id
            .clone()
            .unwrap_or_else(|| "—".into());
        let mut lines = vec![
            line(format!("Agent {agent}"), MUTED),
            line(
                format!("状态 {} · 集成 {}", form.status, form.integration),
                MUTED,
            ),
            line(format!("候选提交 {}", form.commit), FOREGROUND),
        ];
        if !form.summary.is_empty() {
            lines.push(line(format!("结果 {}", form.summary), ACCENT));
        }
        lines.push(line(String::new(), FOREGROUND));
        lines.push(line("差异：", MUTED));
        let total = form.diff.lines().count();
        for text in form.diff.lines().take(300) {
            lines.push(line(text.to_string(), FOREGROUND));
        }
        if total > 300 {
            lines.push(line(format!("…（差异共 {total} 行，已截断显示）"), MUTED));
        }
        lines.push(line(String::new(), FOREGROUND));
        lines.push(line(format!("审查结论 ❯ {}", form.input), ACCENT));
        lines.push(line(
            "Enter 集成 · PgUp/PgDn 滚动 · Esc 取消（先填写结论）",
            GREEN,
        ));
        let popup = centered(full, full.width.min(120), full.height.min(34));
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .scroll((form.scroll.min(u16::MAX as usize) as u16, 0))
                .block(
                    Block::bordered()
                        .title(" 审查并集成 · 验证候选提交后合入协作分支 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
    if app.confirm_quit {
        let height = full.height.min(6);
        let width = full.width.min(68);
        let popup = Rect::new(
            full.x + (full.width - width) / 2,
            full.y + (full.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new("关闭终端演示？\n模拟记录不写入后台。\nEnter 确认退出 · Esc 返回")
                .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(" 退出确认 ")
                        .border_style(Style::default().fg(ACCENT)),
                ),
            popup,
        );
    }
}

/// Push the first `limit` lines of a long block, noting how many were hidden.
fn push_block(lines: &mut Vec<Line<'static>>, text: &str, limit: usize) {
    let total = text.lines().count();
    for text in text.lines().take(limit) {
        lines.push(line(text.to_string(), FOREGROUND));
    }
    if total > limit {
        lines.push(line(
            format!("…（共 {total} 行，已截断显示 {limit} 行）"),
            MUTED,
        ));
    }
}

/// Push a multi-line task text field, marking the caret inside the focused field.
///
/// The caret is inlined rather than moved with the terminal cursor, so wrapping stays correct and
/// no line can be pushed out of the viewport by an inaccurate row calculation.
fn push_caret_field(lines: &mut Vec<Line<'static>>, label: &str, buffer: &TextBuf, focused: bool) {
    lines.push(line(
        format!("{}{label}", if focused { "❯ " } else { "  " }),
        if focused { ACCENT } else { MUTED },
    ));
    if buffer.is_empty() && !focused {
        lines.push(line("  （空）", MUTED));
        return;
    }
    let color = if focused { ACCENT } else { FOREGROUND };
    // Clamp once, so a stale caret can never split a character or hide the marker.
    let mut cursor = buffer.cursor.min(buffer.text.len());
    while !buffer.text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let mut start = 0usize;
    for text in buffer.text.split('\n') {
        let end = start + text.len();
        let marking = focused && cursor >= start && cursor <= end;
        if marking {
            let caret = cursor - start;
            lines.push(line(
                format!("  {}❙{}", &text[..caret], &text[caret..]),
                color,
            ));
        } else {
            lines.push(line(format!("  {text}"), color));
        }
        start = end + 1;
    }
}

/// Push the dependency rows: a window around the highlight, with unusable rows marked.
fn push_dependencies(lines: &mut Vec<Line<'static>>, form: &TaskForm) {
    lines.push(line(
        format!(
            "{}依赖（空格 勾选，最多 {MAX_DEPENDENCIES} 项）",
            if form.field == 2 { "❯ " } else { "  " }
        ),
        if form.field == 2 { ACCENT } else { MUTED },
    ));
    if form.dependencies.is_empty() {
        lines.push(line("  （本会话暂无可作为依赖的任务）", MUTED));
        return;
    }
    const WINDOW: usize = 8;
    let start = form
        .index
        .saturating_sub(WINDOW / 2)
        .min(form.dependencies.len().saturating_sub(WINDOW));
    for (offset, dependency) in form.dependencies[start..].iter().take(WINDOW).enumerate() {
        let row = start + offset;
        let active = form.field == 2 && row == form.index;
        let color = if !dependency.selectable {
            BORDER
        } else if active {
            ACCENT
        } else {
            FOREGROUND
        };
        lines.push(line(
            format!(
                "{} {} {}  {}",
                if active { "▸" } else { " " },
                if dependency.selected { "◉" } else { "○" },
                dependency.label,
                dependency.note
            ),
            color,
        ));
    }
    if form.dependencies.len() > WINDOW {
        lines.push(line(
            format!("  …（共 {} 项，↑↓ 移动）", form.dependencies.len()),
            MUTED,
        ));
    }
}

/// Push a labelled form field; the focused field is marked and highlighted.
fn push_field(lines: &mut Vec<Line<'static>>, label: &str, value: &str, focused: bool) {
    let marker = if focused { "❯ " } else { "  " };
    let label_color = if focused { ACCENT } else { MUTED };
    lines.push(line(format!("{marker}{label}"), label_color));
    if value.is_empty() {
        lines.push(line("  （空）", MUTED));
        return;
    }
    let value_color = if focused { ACCENT } else { FOREGROUND };
    for text in value.lines() {
        lines.push(line(format!("  {text}"), value_color));
    }
}

fn centered(full: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(full.width);
    let height = height.min(full.height);
    Rect::new(
        full.x + (full.width - width) / 2,
        full.y + (full.height - height) / 2,
        width,
        height,
    )
}

fn draw_pane(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    let active = index == app.focused;
    let show_cursor = active && !app.confirm_quit;
    let p = &mut app.panes[index];
    let title = format!(
        " {}{} {} · {} [{}] ",
        if active { "▶ " } else { "" },
        index + 1,
        p.role,
        p.model,
        p.status
    );
    let block = Block::bordered()
        .title(title)
        .border_style(Style::default().fg(if active { ACCENT } else { BORDER }));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 8 || inner.height < 3 {
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);
    let budget = p
        .context_bytes
        .map(|bytes| format!(" · {bytes} 字节预算"))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(format!("{} / {}{budget}", p.api, p.protocol))
            .style(Style::default().fg(MUTED)),
        rows[0],
    );
    let mut lines = p.history.clone();
    if !p.draft.is_empty() {
        lines.push(line(p.draft.clone(), ACCENT));
    }
    let history = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max_scroll = history
        .line_count(rows[1].width)
        .saturating_sub(rows[1].height as usize);
    p.scroll = p.scroll.min(max_scroll);
    let top = max_scroll.saturating_sub(p.scroll).min(u16::MAX as usize) as u16;
    frame.render_widget(history.scroll((top, 0)), rows[1]);
    let width = rows[2].width.saturating_sub(3);
    let (visible, caret) = input_window(&p.input.text, p.input.cursor, width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "❯ ",
                Style::default().fg(if active { GREEN } else { MUTED }),
            ),
            Span::raw(visible),
        ])),
        rows[2],
    );
    if show_cursor && rows[2].width > 2 {
        frame.set_cursor_position((rows[2].x + 2 + caret, rows[2].y));
    }
    if p.scroll > 0 {
        frame.render_widget(
            Paragraph::new("历史 · PgUp/PgDn · End 回底部").style(Style::default().fg(ACCENT)),
            rows[0],
        );
    }
}

/// The visible single-line window of the pane input, plus the caret column inside it.
///
/// The window is anchored at the end, like a shell, unless that would push the caret out of view;
/// in that case the window starts at the caret so editing never happens off-screen.
fn input_window(input: &str, cursor: usize, width: u16) -> (&str, u16) {
    let mut start = input.len();
    let mut columns = 0;
    for (offset, character) in input.char_indices().rev() {
        let character_width = Line::from(character.to_string()).width();
        if columns + character_width > width as usize {
            break;
        }
        columns += character_width;
        start = offset;
    }
    let mut cursor = cursor.min(input.len());
    while !input.is_char_boundary(cursor) {
        cursor -= 1;
    }
    if cursor < start {
        start = cursor;
    }
    let caret = Line::from(&input[start..cursor]).width();
    (&input[start..], (caret.min(width as usize)) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut output = String::new();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                let symbol = buffer.cell((x, y)).unwrap().symbol();
                output.push_str(symbol);
                x += Line::from(symbol).width().max(1) as u16;
            }
            output.push('\n');
        }
        output
    }
    #[test]
    fn layout_has_one_tall_coordinator_and_three_worker_panes() {
        let area = Rect::new(0, 2, 140, 42);
        let panes = pane_areas(area, 0, false);
        assert_eq!(panes[0].height, 42);
        assert_eq!(panes[0].width, 63);
        assert_eq!(panes[1].x, 63);
        assert_eq!(panes[1].height + panes[2].height + panes[3].height, 42);
        for (i, a) in panes.iter().enumerate() {
            for b in panes.iter().skip(i + 1) {
                assert!(a.intersection(*b).is_empty());
            }
        }
    }
    #[test]
    fn full_layout_renders_all_models_and_demo_boundary() {
        let text = screen(&mut App::demo(), 160, 50);
        for model in ["planner-model", "code-model", "review-model", "fast-model"] {
            assert!(text.contains(model));
        }
        assert!(text.contains("模拟记录"));
        assert!(text.contains("Ctrl+Q"));
        assert!(text.contains("Ctrl+T 自检"));
    }
    #[test]
    fn narrow_terminal_and_zoom_show_only_focused_model() {
        let mut app = App::demo();
        app.focused = 2;
        let text = screen(&mut app, 70, 30);
        assert!(text.contains("review-model"));
        assert!(!text.contains("planner-model"));
        app.zoom = true;
        let text = screen(&mut app, 160, 50);
        assert!(text.contains("review-model"));
        assert!(!text.contains("fast-model"));
    }
    #[test]
    fn unicode_input_window_fits_without_cutting_utf8() {
        // Tail-anchored by default, with the caret column measured inside the window.
        assert_eq!(input_window("abc中文", 9, 4), ("中文", 4));
        assert_eq!(input_window("中文", 6, 1), ("", 0));
        // A caret left of the tail window re-anchors the window so editing stays on screen.
        assert_eq!(input_window("abcdef中文ghij", 6, 6), ("中文ghij", 0));
        assert_eq!(input_window("abcdef中文ghij", 14, 6), ("文ghij", 4));
        let long = "中".repeat(10);
        assert_eq!(input_window(&long, 12, 8), ("中中中中中中", 0));
        // An out-of-range or mid-character caret never panics the renderer.
        assert_eq!(input_window("中文", 99, 8), ("中文", 4));
        assert_eq!(input_window("中文", 1, 8), ("中文", 0));
        let mut app = App::demo();
        app.paste(&"中文".repeat(300));
        screen(&mut app, 70, 20);
    }
    #[test]
    fn resize_clamps_history_offset_and_shared_records_render() {
        let mut app = App::demo();
        app.panes[0].scroll = usize::MAX;
        app.shared = true;
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("共享记录"));
        assert!(app.panes[0].scroll < usize::MAX);
        app.panes[0].scroll = 0;
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("等待审查结论"));
    }
    #[test]
    fn session_picker_renders_current_marker_and_requirements() {
        use crate::app::{SessionItem, SessionList};
        let mut app = App::live();
        app.sessions = Some(SessionList {
            items: vec![
                SessionItem {
                    id: "aaaaaaaa-1111-4111-8111-111111111111".into(),
                    repository: "/tmp/repo".into(),
                    branch: "codex/collab-a".into(),
                    base_commit: "89461ea68338…".into(),
                    validations: 2,
                    requirements: vec!["[用户] 保留公开接口".into()],
                    scanned: true,
                    current: false,
                },
                SessionItem {
                    id: "bbbbbbbb-2222-4222-8222-222222222222".into(),
                    repository: "—".into(),
                    branch: "—".into(),
                    base_commit: "—".into(),
                    validations: 0,
                    requirements: Vec::new(),
                    scanned: false,
                    current: true,
                },
            ],
            index: 1,
            note: String::new(),
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("会话列表"));
        assert!(text.contains("aaaaaaaa…") && text.contains("bbbbbbbb…"));
        assert!(text.contains("当前"));
        assert!(text.contains("用户约束：读取中"));
        assert!(
            text.contains("分支 —"),
            "the highlighted row shows its own detail"
        );
        // Moving up shows the other session's branch and constraint summary.
        app.sessions.as_mut().unwrap().index = 0;
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("codex/collab-a"));
        assert!(text.contains("用户约束 1 条"));
        assert!(text.contains("保留公开接口"));
        // Empty and oversized lists stay renderable.
        app.sessions.as_mut().unwrap().items.clear();
        app.sessions.as_mut().unwrap().index = 0;
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("尚无会话"));
    }

    #[test]
    fn empty_and_oversized_session_picker_renders_without_panicking() {
        use crate::app::{SessionItem, SessionList};
        let mut app = App::live();
        let items = (0..40)
            .map(|index| SessionItem {
                id: format!("{index:08}-1111-4111-8111-111111111111"),
                repository: format!("/tmp/仓库{index}"),
                branch: format!("codex/collab-{index}"),
                base_commit: "89461ea68338…".into(),
                validations: index,
                requirements: Vec::new(),
                scanned: true,
                current: index == 12,
            })
            .collect();
        app.sessions = Some(SessionList {
            items,
            index: 39,
            note: "共 40 个会话".into(),
        });
        let text = screen(&mut app, 90, 30);
        assert!(text.contains("共 40 个会话"));
        assert!(text.contains("第 40") || text.contains("codex/collab-39"));
    }

    #[test]
    fn compact_and_create_modals_render() {
        use crate::app::{CompactForm, CreateForm};
        let mut app = App::live();
        app.compact = Some(CompactForm {
            revision: 77,
            input: "已核对用户约束".into(),
            note: "版本已刷新".into(),
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("压缩上下文"));
        assert!(text.contains("会话版本 77"));
        assert!(text.contains("已核对用户约束"));
        app.compact = None;
        app.create = Some(CreateForm {
            repository: "/tmp/repo".into(),
            validation: "cargo test".into(),
            requirement: String::new(),
            field: 1,
            note: String::new(),
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("新建协作会话"));
        assert!(text.contains("/tmp/repo"));
        assert!(text.contains("cargo test"));
    }
    #[test]
    fn output_modal_renders_result_draft_and_attachments() {
        use crate::app::OutputForm;
        let mut app = App::live();
        app.output = Some(OutputForm {
            pane: 0,
            task_id: "11111111-2222-4333-8444-555555555555".into(),
            status: "已完成".into(),
            result: "加了重试与退避。".into(),
            draft: String::new(),
            body: "完整输出第一行".into(),
            artifacts: "full result · 128 字节 · /tmp/state/attachments/t/1.txt".into(),
            scroll: 0,
            live: false,
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("任务输出"));
        assert!(text.contains("加了重试与退避。"));
        assert!(text.contains("完整输出第一行"));
        assert!(text.contains("full result"));
        assert!(text.contains("Ctrl+O 结果"), "the footer advertises Ctrl+O");
    }

    #[test]
    fn task_modal_renders_goal_acceptance_dependencies_and_the_caret() {
        use crate::app::{Dependency, TaskForm, TextBuf};
        let mut app = App::live();
        app.panes[0].agent_id = Some("a".into());
        app.task = Some(TaskForm {
            pane: 0,
            goal: TextBuf::end("实现退避重试"),
            acceptance: TextBuf::end("测试通过"),
            dependencies: vec![
                Dependency {
                    id: "t1".into(),
                    label: "工作1 · 工作中 · 加锁".into(),
                    selectable: true,
                    note: "未完成，新任务会等待它".into(),
                    selected: true,
                },
                Dependency {
                    id: "t2".into(),
                    label: "工作2 · 失败 · 坏的构建".into(),
                    selectable: false,
                    note: "前置已失败：勾选后新任务会直接阻塞".into(),
                    selected: false,
                },
            ],
            index: 1,
            coding: true,
            field: 1,
            note: "依赖未满足时任务保持排队；前置失败则直接阻塞".into(),
        });
        let text = screen(&mut app, 132, 44);
        assert!(text.contains("编排任务"));
        assert!(text.contains("实现退避重试"));
        assert!(text.contains("测试通过"));
        assert!(
            text.contains('◉') && text.contains('○'),
            "selection is visible"
        );
        assert!(
            text.contains("前置已失败"),
            "an unusable dependency explains itself"
        );
        assert!(text.contains("◉ 开"), "the coding switch shows its state");
        assert!(text.contains("Ctrl+S 派发"));
        assert!(text.contains("Ctrl+F 编排"), "the footer advertises Ctrl+F");
        let caret = text
            .lines()
            .find(|row| row.contains('❙'))
            .expect("the focused field shows a caret");
        assert!(
            caret.contains("测试通过"),
            "the caret sits in the focused field: {caret:?}"
        );
    }
    #[test]
    fn empty_and_oversized_task_modals_render_without_panicking() {
        use crate::app::{Dependency, TaskForm, TextBuf};
        let mut app = App::live();
        let goal = TextBuf {
            cursor: 99,
            ..TextBuf::default()
        };
        app.task = Some(TaskForm {
            pane: 3,
            goal,
            acceptance: TextBuf::default(),
            dependencies: Vec::new(),
            index: 7,
            coding: false,
            field: 0,
            note: String::new(),
        });
        // An out-of-range caret is clamped instead of panicking, and the empty field still shows it.
        let text = screen(&mut app, 132, 44);
        assert!(text.contains("暂无可作为依赖的任务"));
        assert!(text.contains('❙'));
        app.task.as_mut().unwrap().field = 2;
        app.task.as_mut().unwrap().dependencies = (0..30)
            .map(|index| Dependency {
                id: format!("t{index}"),
                label: format!("任务 {index}"),
                selectable: true,
                note: "未完成，新任务会等待它".into(),
                selected: false,
            })
            .collect();
        app.task.as_mut().unwrap().index = usize::MAX;
        for width in [0, 7, 60] {
            for height in [0, 3, 12] {
                screen(&mut app, width, height);
            }
        }
    }

    #[test]
    fn publish_modal_renders_kind_selector_and_content() {
        use crate::app::PublishForm;
        let mut app = App::live();
        app.publish = Some(PublishForm {
            kind: 1,
            input: "保持向后兼容".into(),
            note: String::new(),
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("发布用户约束"));
        assert!(text.contains("用户覆盖（user_override）"));
        assert!(text.contains("保持向后兼容"));
        assert!(text.contains('◉'), "the active kind is marked");
        assert!(text.contains("Ctrl+U 约束"), "the footer advertises Ctrl+U");
    }
    #[test]
    fn review_modal_renders_diff_and_conclusion() {
        use crate::app::ReviewForm;
        let mut app = App::live();
        app.review = Some(ReviewForm {
            pane: 0,
            task_id: "task-1".into(),
            commit: "abc123".into(),
            status: "completed".into(),
            integration: "pending".into(),
            summary: "retry implemented".into(),
            diff: "+ retry()\n- old()".into(),
            input: "looks good".into(),
            scroll: 0,
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("审查并集成"));
        assert!(text.contains("abc123"));
        assert!(text.contains("+ retry()"));
        assert!(text.contains("looks good"));
    }
    #[test]
    fn approval_and_credential_modals_render_masked() {
        use crate::app::{ApprovalItem, CredentialForm};
        let mut app = App::demo();
        app.approvals.push(ApprovalItem {
            id: "a1".into(),
            method: "item/commandExecution/requestApproval".into(),
            agent_id: "agent-1".into(),
            task_id: "task-1".into(),
            detail: "cargo test --all".into(),
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("操作审批"));
        assert!(text.contains("cargo test --all"));
        app.approvals.clear();
        app.credential = Some(CredentialForm {
            pane: 0,
            input: "sk-secret-value".into(),
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("设置 API Key"));
        assert!(text.contains('•'));
        assert!(!text.contains("sk-secret"));
    }
    #[test]
    fn agent_modal_renders_identity_endpoint_and_budget() {
        use crate::app::AgentForm;
        let mut app = App::live();
        app.agent = Some(AgentForm {
            pane: 1,
            id: "11111111-2222-4333-8444-555555555555".into(),
            role: "worker",
            model: "worker-model".into(),
            base_url: "https://example.invalid/v1".into(),
            protocol: "chat_completions".into(),
            context_bytes: "65536".into(),
            field: 1,
            note: "为窗格 2 新增 Agent（角色由位置决定）。".into(),
        });
        let text = screen(&mut app, 140, 42);
        assert!(text.contains("配置 Agent"));
        assert!(text.contains("worker-model"));
        assert!(text.contains("https://example.invalid/v1"));
        assert!(text.contains("Chat Completions"));
        assert!(text.contains("65536"));
        assert!(text.contains("工作（worker）"));
    }
    #[test]
    fn footer_key_hints_fit_the_demo_frame() {
        // The 132-column demo frame must show every hint, including the last one, unabridged.
        let text = screen(&mut App::demo(), 132, 40);
        for hint in [
            "Ctrl+A 配置",
            "Ctrl+T 自检",
            "Ctrl+U 约束",
            "Ctrl+O 结果",
            "Ctrl+F 编排",
            "Ctrl+L 会话",
            "Ctrl+G 审查集成",
            "Ctrl+C 停止",
            "Ctrl+Q 退出",
        ] {
            assert!(text.contains(hint), "footer hint {hint:?} is clipped");
        }
    }
    #[test]
    fn tiny_sizes_and_quit_popup_do_not_panic() {
        use crate::app::OutputForm;
        for width in [0, 1, 7, 15, 80] {
            for height in [0, 1, 3, 10] {
                let mut app = App::demo();
                app.shared = true;
                app.confirm_quit = true;
                screen(&mut app, width, height);
                app.confirm_quit = false;
                app.output = Some(OutputForm {
                    pane: 0,
                    task_id: "task-1".into(),
                    status: "工作中".into(),
                    result: "结果".into(),
                    draft: "草稿".into(),
                    body: String::new(),
                    artifacts: String::new(),
                    scroll: usize::MAX,
                    live: true,
                });
                screen(&mut app, width, height);
            }
        }
        let mut app = App::demo();
        app.confirm_quit = true;
        assert!(screen(&mut app, 100, 30).contains("退出确认"));
    }
}
