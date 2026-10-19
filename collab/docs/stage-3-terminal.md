# 终端壳层验收

按用户选择采用第二种布局：左侧一个主控终端（45%），右侧三个工作终端上下排列（55%）。所有窗格在同一个终端页面显示。此次交付范围为可交互的模拟演示，尚未连接协作后台或真实模型。

## 启动

需要 Rust 1.95+，在项目根目录运行：

```sh
cargo run --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- --demo
```

建议终端至少 100 列 × 28 行；展开共享记录时建议至少 33 行。空间不足会只显示当前模型，通过 Tab 切换。模型名、API 标识、版本、提交、测试结果和工作状态均为示例数据。

## 操作

| 操作 | 按键 |
| --- | --- |
| 切换当前模型 | Tab / Shift+Tab，或鼠标点击 |
| 独立发送 | 输入后 Enter，仅写入当前窗格 |
| 放大 / 返回布局 | F2（也支持 Alt+Enter） |
| 展开 / 收起共享记录 | F3 |
| 滚动当前窗格 | PgUp / PgDown，或鼠标滚轮 |
| 回到记录底部 | End |
| 停止当前模拟任务 | Ctrl+C |
| 退出确认 | Ctrl+Q，然后 Enter；Esc 返回 |

每个窗格保留自己的输入和滚动位置。支持中文及粘贴，粘贴中的换行转换为空格，输入限制为 4096 字节。关闭演示不修改项目文件，不读取密钥，不保存聊天记录。

## 验证

13 项状态与渲染测试通过，覆盖焦点、独立输入、取消、退出确认、中文粘贴、长度限制、布局、放大、历史滚动、共享记录及极小窗口。Clippy 警告视为错误检查通过。最终 workspace 回归共 45 项通过，1 项已有 Codex 进程端到端测试按默认配置跳过，此次未重跑。结果见 `../evidence/stage-3-terminal-summary.json`。

伪终端烟雾测试通过：四模型显示、粘贴发送、单独停止、确认退出、输入模式及备用屏幕恢复、非交互运行拒绝。

```sh
cargo test --locked --manifest-path collab/Cargo.toml -p codex-collab-tui
cargo clippy --locked --manifest-path collab/Cargo.toml -p codex-collab-tui --all-targets -- -D warnings
cargo build --locked --manifest-path collab/Cargo.toml -p codex-collab-tui
python3 collab/scripts/probe_terminal.py collab/target/debug/codex-collab-tui
```

## 边界和下一步

输入目前仅支持末尾追加和退格；每个窗格最多保留最近 200 条模拟记录。真实流式输出、持久化补读、模型配置、审批、差异与测试结果尚未接入。此版本验证布局及操作，不能据此判断真实多模型协作表现。

用户确认终端壳层后，再按阶段确认机制接入现有 Runtime 与共享记录；不继续 Tauri / Vue 图形桌面路线。第一段（子进程宿主 + 行协议，真实会话/Agent/任务显示与公开文本流）已完成，见 [阶段 4](stage-4-host-client.md)。
