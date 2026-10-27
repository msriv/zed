use super::*;
use acp_thread::{AgentThreadEntry, DisplayTerminalPatch, ToolCallStatus};
use agent_client_protocol::schema::MaybeUndefined;
use agent_ui::AgentPanel;
use workspace::dock::Panel as _;

pub(super) fn run(
    app_state: Arc<AppState>,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<()> {
    for width in [420.0, 560.0] {
        capture_conversation(app_state.clone(), width, cx, update_baseline)?;
    }
    Ok(())
}

fn capture_conversation(
    app_state: Arc<AppState>,
    width: f32,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<()> {
    let connection = StubAgentConnection::new();
    let server: Rc<dyn AgentServer> = Rc::new(StubAgentServer::new(connection));
    let project = cx.update(|cx| {
        Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags {
                init_worktree_trust: false,
                ..Default::default()
            },
            cx,
        )
    });
    let workspace_window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(width), px(900.0)),
                })),
                focus: false,
                show: false,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Workspace::new(None, project, app_state, window, cx)),
        )
    })?;
    cx.run_until_parked();

    let (workspace, async_window) = workspace_window.update(cx, |workspace, window, cx| {
        (workspace.weak_handle(), window.to_async(cx))
    })?;
    cx.background_executor.allow_parking();
    let panel_result = cx
        .foreground_executor
        .block_test(AgentPanel::load(workspace, async_window));
    cx.background_executor.forbid_parking();
    let panel = panel_result.context("Failed to load fixture AgentPanel")?;

    workspace_window.update(cx, |workspace, window, cx| {
        workspace.add_panel(panel.clone(), window, cx);
        workspace.open_panel::<AgentPanel>(window, cx);
        panel.update(cx, |panel, cx| {
            panel.set_zoomed(true, window, cx);
            panel.open_external_thread_with_server(server, window, cx);
        });
    })?;
    cx.run_until_parked();

    let conversation = cx
        .read(|cx| panel.read(cx).active_thread_view_for_tests().cloned())
        .context("Fixture did not create a conversation")?;
    let thread = cx
        .read(|cx| {
            conversation
                .read(cx)
                .active_thread()
                .map(|view| view.read(cx).thread.clone())
        })
        .context("Fixture conversation is still loading")?;
    let terminal_id = acp_v2::TerminalId::new("fixture-terminal");
    let running_tool_id = acp_v2::ToolCallId::new("fixture-terminal-tool");
    let terminal_output = format!(
        "   Compiling agent_ui\r\n    Finished test profile\r\n     Running conversation tests\r\n\r\nrunning 32 tests\r\n{}Tests still running...",
        (1..=24)
            .map(|index| format!("test conversation_layout_{index:02} ... ok\r\n"))
            .collect::<String>()
    );

    // NewEntry observers read thread length, so deliver one insertion per update.
    thread.update(cx, |thread, cx| -> Result<()> {
        thread.set_provisional_title("Polish the agent conversation".into(), cx);
        thread.upsert_user_message(
            acp_v2::UserMessage::new("fixture-user")
                .content(vec!["Make this feel calmer and easier to scan.".into()]),
            cx,
        )
    })?;
    thread.update(cx, |thread, cx| {
        thread.upsert_assistant_message(
            acp_v2::AgentMessage::new("fixture-assistant").content(vec![
                "I’ll keep the conversation easy to follow, with a compact activity stack and a quieter composer.\n\nThe shared thread renderer keeps native and external agents consistent. I’m checking the layout at both panel widths now."
                    .into(),
            ]),
            cx,
        )
    })?;
    thread.update(cx, |thread, cx| {
        thread.upsert_wire_tool_call(
            acp_v2::ToolCallUpdate::new("fixture-read")
                .title("Read `thread_view.rs`")
                .kind(acp_v2::ToolKind::Read)
                .status(acp_v2::ToolCallStatus::Completed),
            cx,
        )
    })?;
    thread.update(cx, |thread, cx| -> Result<()> {
        thread.upsert_display_terminal(
            terminal_id.clone(),
            DisplayTerminalPatch {
                command: MaybeUndefined::Value("cargo test -p agent_ui conversation".into()),
                cwd: MaybeUndefined::Value(acp_v2::AbsolutePath::new(PathBuf::from(
                    "/workspace/panel-preview",
                ))),
                ..Default::default()
            },
            cx,
        )?;
        thread.append_display_terminal_output(terminal_id.clone(), terminal_output.as_bytes(), cx)
    })?;
    thread.update(cx, |thread, cx| {
        thread.upsert_wire_tool_call(
            acp_v2::ToolCallUpdate::new(running_tool_id.clone())
                .title("Run conversation layout tests")
                .kind(acp_v2::ToolKind::Execute)
                .status(acp_v2::ToolCallStatus::InProgress)
                .content(vec![acp_v2::ToolCallContent::Terminal(
                    acp_v2::Terminal::new(terminal_id.clone()),
                )]),
            cx,
        )
    })?;
    thread.update(cx, |thread, cx| {
        thread.upsert_wire_tool_call(
            acp_v2::ToolCallUpdate::new("fixture-search")
                .title("Search for remaining layout regressions")
                .kind(acp_v2::ToolKind::Search)
                .status(acp_v2::ToolCallStatus::Pending),
            cx,
        )
    })?;
    cx.run_until_parked();
    conversation.update(cx, |view, cx| view.expand_tool_call(running_tool_id, cx));
    cx.run_until_parked();

    cx.read(|cx| -> Result<()> {
        let thread = thread.read(cx);
        anyhow::ensure!(
            thread.entries().len() == 5,
            "Expected user, prose, and three adjacent tools"
        );
        anyhow::ensure!(
            matches!(
                thread.entries().first(),
                Some(AgentThreadEntry::UserMessage(_))
            ),
            "Missing user bubble"
        );
        anyhow::ensure!(
            matches!(
                thread.entries().get(1),
                Some(AgentThreadEntry::AssistantMessage(_))
            ),
            "Missing assistant prose"
        );
        let statuses: Vec<_> = thread
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                AgentThreadEntry::ToolCall(tool) => Some(tool.status()),
                _ => None,
            })
            .collect();
        anyhow::ensure!(
            statuses
                == [
                    ToolCallStatus::Completed,
                    ToolCallStatus::InProgress,
                    ToolCallStatus::Pending
                ],
            "Fixture tool status order changed: {statuses:?}"
        );
        let terminal = thread.terminal(terminal_id)?;
        anyhow::ensure!(
            terminal.read(cx).working_dir().as_deref()
                == Some(Path::new("/workspace/panel-preview")),
            "Fixture must exercise the terminal working directory header"
        );
        anyhow::ensure!(
            !terminal.read(cx).is_process_backed(),
            "Fixture must not launch a process"
        );
        anyhow::ensure!(
            terminal.read(cx).output().is_none(),
            "Fixture terminal must remain running"
        );
        anyhow::ensure!(
            terminal
                .read(cx)
                .to_markdown(cx)
                .contains("Tests still running..."),
            "Fixture terminal output has not rendered"
        );
        Ok(())
    })?;

    let result = run_visual_test(
        &format!("agent_conversation_redesign_{}", width as u32),
        workspace_window.into(),
        cx,
        update_baseline,
    );
    cx.update_window(workspace_window.into(), |_, window, _| {
        window.remove_window()
    })?;
    cx.run_until_parked();
    result?;
    Ok(())
}
