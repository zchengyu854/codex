mod app;
mod client;
mod host;
mod ui;

use anyhow::{Result, bail, ensure};
use app::App;
use client::{Client, Options};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind, MouseButton, MouseEventKind,
    },
    execute,
};
use host::Host;
use ratatui::layout::Position;
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::Duration,
};

const HELP: &str = "用法:
  codex-collab-tui --demo
      终端布局演示；不连接 API，不读取密钥，不修改项目。

  codex-collab-tui --state <状态目录> [--host <核心可执行文件>] [--codex <Codex 可执行文件>]
                   [--session <会话 ID>] [--repository <Git 路径>] [--validation '<命令>' ...]
      连接协作后台：启动/复用 codex-collab-core 宿主，按 core-api.md 的 JSON 行协议读取
      真实会话、Agent 与任务，流式显示公开消息。未指定 --session 时使用第一个会话；
      没有任何会话且给出 --repository 时按其创建（需至少一条 --validation）。

操作：Tab/Shift+Tab 焦点 · F2 放大 · F3 共享记录 · Ctrl+R 刷新
      Ctrl+C 取消当前窗格任务 · Ctrl+Q 确认退出";

struct TerminalModes;
impl Drop for TerminalModes {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        ratatui::restore();
    }
}

enum Command {
    Help,
    Demo,
    Live(Live),
}

struct Live {
    host: PathBuf,
    state: PathBuf,
    codex: PathBuf,
    options: Options,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(args)? {
        Command::Help => {
            println!("{HELP}");
            Ok(())
        }
        Command::Demo => run_demo(),
        Command::Live(live) => run_live(live),
    }
}

fn parse(args: Vec<String>) -> Result<Command> {
    if args.is_empty() {
        return Ok(Command::Help);
    }
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        return Ok(Command::Help);
    }
    if args.len() == 1 && args[0] == "--demo" {
        return Ok(Command::Demo);
    }
    let mut host = default_core_binary();
    let mut state = None;
    let mut codex = PathBuf::from("codex");
    let mut options = Options::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--host" => host = PathBuf::from(value(&mut iter, "--host")?),
            "--state" => state = Some(PathBuf::from(value(&mut iter, "--state")?)),
            "--codex" => codex = PathBuf::from(value(&mut iter, "--codex")?),
            "--session" => options.session = Some(value(&mut iter, "--session")?),
            "--repository" => options.repository = Some(value(&mut iter, "--repository")?),
            "--validation" => {
                let argv = command_argv(&value(&mut iter, "--validation")?)?;
                options.validation.push(argv);
            }
            "--demo" => bail!("--demo 不能与其他参数同时使用。"),
            other => bail!("未知参数：{other}\n\n{HELP}"),
        }
    }
    let state =
        state.ok_or_else(|| anyhow::anyhow!("实时模式需要 --state <状态目录>。\n\n{HELP}"))?;
    Ok(Command::Live(Live {
        host,
        state,
        codex,
        options,
    }))
}

fn value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    iter.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} 缺少取值"))
}

fn command_argv(raw: &str) -> Result<Vec<String>> {
    let argv: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
    ensure!(!argv.is_empty(), "验证命令不能为空");
    Ok(argv)
}

/// Locate the collaboration host: explicit env override, then a sibling binary, then PATH.
fn default_core_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CODEX_COLLAB_CORE") {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("codex-collab-core");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("codex-collab-core")
}

fn run_demo() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "请在交互终端中运行 --demo；不支持管道输入或重定向输出。"
    );
    let _modes = TerminalModes;
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let mut app = App::demo();
    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release && app.key(key) => break,
            Event::Paste(text) if !app.confirm_quit => app.paste(&text),
            Event::Mouse(mouse) if !app.confirm_quit => handle_mouse(&mut app, mouse),
            _ => {}
        }
    }
    Ok(())
}

fn run_live(live: Live) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "请在交互终端中运行；不支持管道输入或重定向输出。"
    );
    let host = Host::spawn(&live.host, &live.state, &live.codex)?;
    let mut app = App::live();
    let mut client = Client::new(host, live.options);
    client.start(&mut app);
    client.poll(&mut app);
    let _modes = TerminalModes;
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release && app.key(key) => break,
                Event::Paste(text) if !app.confirm_quit => app.paste(&text),
                Event::Mouse(mouse) if !app.confirm_quit => handle_mouse(&mut app, mouse),
                _ => {}
            }
        }
        client.poll(&mut app);
    }
    // Dropping the client closes the host's stdin, which stops the scheduler and cancels running tasks.
    drop(client);
    Ok(())
}

fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) {
    if let Some(index) = app
        .areas
        .iter()
        .position(|area| area.contains(Position::new(mouse.column, mouse.row)))
    {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => app.focused = index,
            MouseEventKind::ScrollUp => {
                app.focused = index;
                app.panes[index].scroll = app.panes[index].scroll.saturating_add(3);
            }
            MouseEventKind::ScrollDown => {
                app.focused = index;
                app.panes[index].scroll = app.panes[index].scroll.saturating_sub(3);
            }
            _ => {}
        }
    }
}
