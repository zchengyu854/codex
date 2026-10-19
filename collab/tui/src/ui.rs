use crate::app::{ACCENT, App, GREEN, MUTED, WARN, line};
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
        Constraint::Length(2),
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
                    "[{} {}] 编码:{}  Tab 焦点  F2 放大  F3 共享  Ctrl+E 编码  Ctrl+G 审查集成  Ctrl+P 压缩  Ctrl+N 新建  Ctrl+K 密钥  Ctrl+R 刷新  Ctrl+C 停止  Ctrl+Q 退出",
                    app.focused + 1,
                    focused.role,
                    if app.coding { "开" } else { "关" }
                ),
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
    frame.render_widget(
        Paragraph::new(format!("{} / {}", p.api, p.protocol)).style(Style::default().fg(MUTED)),
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
    let tail = input_tail(&p.input, width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "❯ ",
                Style::default().fg(if active { GREEN } else { MUTED }),
            ),
            Span::raw(tail),
        ])),
        rows[2],
    );
    if show_cursor && rows[2].width > 2 {
        frame.set_cursor_position((rows[2].x + 2 + Line::from(tail).width() as u16, rows[2].y));
    }
    if p.scroll > 0 {
        frame.render_widget(
            Paragraph::new("历史 · PgUp/PgDn · End 回底部").style(Style::default().fg(ACCENT)),
            rows[0],
        );
    }
}

fn input_tail(input: &str, width: u16) -> &str {
    let mut columns = 0;
    let mut start = input.len();
    for (offset, character) in input.char_indices().rev() {
        let character_width = Line::from(character.to_string()).width();
        if columns + character_width > width as usize {
            break;
        }
        columns += character_width;
        start = offset;
    }
    &input[start..]
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
    fn unicode_input_tail_fits_without_cutting_utf8() {
        assert_eq!(input_tail("abc中文", 4), "中文");
        assert_eq!(input_tail("中文", 1), "");
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
    fn tiny_sizes_and_quit_popup_do_not_panic() {
        for width in [0, 1, 7, 15, 80] {
            for height in [0, 1, 3, 10] {
                let mut app = App::demo();
                app.shared = true;
                app.confirm_quit = true;
                screen(&mut app, width, height);
            }
        }
        let mut app = App::demo();
        app.confirm_quit = true;
        assert!(screen(&mut app, 100, 30).contains("退出确认"));
    }
}
