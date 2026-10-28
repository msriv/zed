use anyhow::{Context as _, ensure};
use markdown::Markdown;
use serde_json::json;
use twg::{Comment, NamedId, User};

use super::*;

#[derive(Clone, Copy)]
pub enum Screen {
    List,
    ListError,
    StatusFiltered,
    CustomStatus,
    Ticket,
    Edit,
    Transition,
    LogWork,
}

impl Screen {
    pub const ALL: [Self; 8] = [
        Self::List,
        Self::ListError,
        Self::StatusFiltered,
        Self::CustomStatus,
        Self::Ticket,
        Self::Edit,
        Self::Transition,
        Self::LogWork,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::ListError => "list_error",
            Self::StatusFiltered => "status_filtered",
            Self::CustomStatus => "custom_status",
            Self::Ticket => "ticket",
            Self::Edit => "edit",
            Self::Transition => "transition",
            Self::LogWork => "log_work",
        }
    }
}

#[derive(Clone, Copy)]
pub enum PullRequestScreen {
    Files,
    Overview,
    Comments,
    Commits,
    Inline,
}

impl PullRequestScreen {
    pub fn name(self) -> &'static str {
        match self {
            Self::Files => "pull_request",
            Self::Overview => "pull_request_overview",
            Self::Comments => "pull_request_comments",
            Self::Commits => "pull_request_commits",
            Self::Inline => "pull_request_inline",
        }
    }
}

pub fn create_pull_request(
    workspace: WeakEntity<Workspace>,
    screen: PullRequestScreen,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    cx.new(|cx| {
        let tab = match screen {
            PullRequestScreen::Files => Some(pull_request_view::Tab::Files),
            PullRequestScreen::Overview => Some(pull_request_view::Tab::Overview),
            PullRequestScreen::Comments => Some(pull_request_view::Tab::Comments),
            PullRequestScreen::Commits => Some(pull_request_view::Tab::Commits),
            PullRequestScreen::Inline => None,
        };
        match tab {
            Some(tab) => pull_request_view::PullRequestView::for_visual_fixture_tab(
                workspace, tab, window, cx,
            ),
            None => {
                pull_request_view::PullRequestView::for_visual_fixture_inline(workspace, window, cx)
            }
        }
        .expect("The static pull request visual fixture must parse")
    })
    .into()
}

pub fn create(
    workspace: WeakEntity<Workspace>,
    screen: Screen,
    window: &mut Window,
    cx: &mut Context<WorkPanel>,
) -> WorkPanel {
    let pr_list = cx.new(|_| match screen {
        Screen::ListError => PrList::for_visual_fixture_error(workspace.clone()),
        _ => PrList::for_visual_fixture(workspace.clone()),
    });
    let status_screen = matches!(screen, Screen::StatusFiltered | Screen::CustomStatus);
    WorkPanel {
        pr_list,
        work_view: if status_screen {
            WorkView::Tickets
        } else {
            WorkView::All
        },
        workspace,
        connection: TwgSettings::get_global(cx).clone(),
        focus_handle: cx.focus_handle(),
        search: make_editor("Filter, or DAT-1206", "", 1, window, cx),
        tickets: tickets()
            .into_iter()
            .filter(|ticket| {
                !status_screen || ticket.status.name.eq_ignore_ascii_case("In Progress")
            })
            .collect(),
        filters: vec![SavedFilter {
            id: "fixture-query".into(),
            name: "Team backlog".into(),
            jql: "project = DAT ORDER BY updated DESC".into(),
        }],
        query: "assignee = currentUser() ORDER BY updated DESC".into(),
        query_name: "Assigned to me".into(),
        status_filter: status_screen.then(|| "In Progress".into()),
        status_options: vec![
            "Blocked".into(),
            "In Progress".into(),
            "In review".into(),
            "To do".into(),
        ],
        custom_status: matches!(screen, Screen::CustomStatus)
            .then(|| make_editor("Exact Jira status name", "Awaiting approval", 1, window, cx)),
        limit: 25,
        loading: false,
        error: None,
        synced: Some(Instant::now()),
        requests: state::Requests::default(),
        fetch_task: None,
        refresh_task: None,
        subscriptions: Vec::new(),
    }
}

pub fn create_ticket(
    workspace: WeakEntity<Workspace>,
    screen: Screen,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    ticket_fixture(workspace, screen, window, cx).into()
}

fn ticket_fixture(
    workspace: WeakEntity<Workspace>,
    screen: Screen,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TicketView> {
    cx.new(|cx| {
        let ticket = tickets().remove(0);
        let pr_list = cx.new(|_| PrList::for_visual_fixture(workspace.clone()));
        let description = twg::rich_text::project(&ticket.description).markdown;
        let description = Some(cx.new(|cx| Markdown::new(description.into(), None, None, cx)));
        let comments = ticket
            .comments
            .iter()
            .map(|comment| {
                let text = twg::rich_text::project(&comment.body).markdown;
                cx.new(|cx| Markdown::new(text.into(), None, None, cx))
            })
            .collect();
        let edit = matches!(screen, Screen::Edit)
            .then(|| EditDraft::for_visual_fixture(&ticket, window, cx));
        let status = matches!(screen, Screen::Transition)
            .then(|| StatusDraft::for_visual_fixture(window, cx));
        let work_log =
            matches!(screen, Screen::LogWork).then(|| WorkLogDraft::for_visual_fixture(window, cx));
        TicketView {
            pr_list,
            workspace,
            connection: TwgSettings::get_global(cx).clone(),
            key: ticket.key.clone(),
            focus_handle: cx.focus_handle(),
            comment: make_editor("Comment on this ticket…", "", 3, window, cx),
            loading: false,
            error: None,
            notice: None,
            synced: Some(Instant::now()),
            ticket: Some(ticket),
            description,
            comments,
            comment_total: Some(5),
            comment_cursor: Some("fixture-older-comments".into()),
            comments_loading: false,
            comment_requests: state::Requests::default(),
            comments_task: None,
            edit,
            status,
            work_log,
            writing: false,
            requests: state::Requests::default(),
            fetch_task: None,
            write_task: None,
            refresh_task: None,
            subscriptions: Vec::new(),
        }
    })
}

pub fn validate(panel: &WorkPanel, screen: Screen, cx: &App) -> anyhow::Result<()> {
    ensure!(
        panel.fetch_task.is_none() && panel.refresh_task.is_none() && !panel.loading,
        "Work panel visual fixtures must not start background reads or writes"
    );
    ensure!(
        matches!(
            screen,
            Screen::List | Screen::ListError | Screen::StatusFiltered | Screen::CustomStatus
        ),
        "Ticket fixtures belong in the editor"
    );
    if matches!(screen, Screen::StatusFiltered | Screen::CustomStatus) {
        ensure!(
            panel.status_filter.as_deref() == Some("In Progress"),
            "Status fixture must show the selected status"
        );
        ensure!(
            panel.tickets.len() == 1
                && panel.tickets[0]
                    .status
                    .name
                    .eq_ignore_ascii_case("In Progress"),
            "Filtered fixture must contain matching tickets only"
        );
        ensure!(
            panel.effective_query()
                == "(assignee = currentUser()) AND status = \"In Progress\" ORDER BY updated DESC",
            "Selected status must reach the server query"
        );
    }
    ensure!(
        panel.custom_status.is_some() == matches!(screen, Screen::CustomStatus),
        "Custom status fixture must expose its exact-name editor"
    );
    if let Some(editor) = &panel.custom_status {
        ensure!(
            editor.read(cx).text(cx) == "Awaiting approval",
            "Custom status fixture must show an unlisted workflow name"
        );
    }
    if matches!(screen, Screen::ListError) {
        ensure!(
            !panel.pr_list.read(cx).mine.is_empty(),
            "List error fixture must include pull request rows"
        );
    }
    Ok(())
}

pub fn validate_ticket(view: &gpui::AnyView, screen: Screen, cx: &App) -> anyhow::Result<()> {
    let view = view
        .clone()
        .downcast::<TicketView>()
        .ok()
        .context("Expected a ticket editor fixture")?;
    let ticket = view.read(cx);
    validate_idle_ticket(ticket)?;
    ensure!(
        !ticket.writing,
        "Ticket screenshots must not contain pending writes"
    );
    ensure!(
        ticket.ticket.is_some(),
        "Ticket fixture must include ticket details"
    );
    ensure!(
        ticket.edit.is_some() == matches!(screen, Screen::Edit),
        "Incorrect edit fixture state"
    );
    ensure!(
        ticket.status.is_some() == matches!(screen, Screen::Transition),
        "Incorrect transition fixture state"
    );
    ensure!(
        ticket.work_log.is_some() == matches!(screen, Screen::LogWork),
        "Incorrect work log fixture state"
    );
    if let Some(edit) = &ticket.edit {
        ensure!(
            edit.dirty_count(cx) == 2,
            "Edit fixture must exercise two unsaved fields"
        );
    }
    ensure!(
        workspace::item::Item::is_dirty(ticket, cx) == !matches!(screen, Screen::Ticket),
        "Ticket tabs must expose unsaved drafts to the workspace"
    );
    Ok(())
}

fn validate_idle_ticket(ticket: &TicketView) -> anyhow::Result<()> {
    ensure!(
        ticket.fetch_task.is_none()
            && ticket.write_task.is_none()
            && ticket.refresh_task.is_none()
            && ticket.comments_task.is_none()
            && !ticket.loading
            && !ticket.comments_loading,
        "Ticket fixtures must not start background reads or writes"
    );
    Ok(())
}

pub fn exercise_status_filter(
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let panel = cx.new(|cx| create(workspace, Screen::List, window, cx));
    panel.update(cx, |panel, cx| {
        ensure!(
            panel.connection.executable == "/fixture/twg-must-not-run",
            "Status exercises must stay offline"
        );
        panel
            .search
            .update(cx, |editor, cx| editor.set_text("analytics", window, cx));
        let base_query = panel.query.clone();
        let base_name = panel.query_name.clone();
        panel.limit = 75;
        let stale_generation = panel.requests.next();
        panel.select_status(Some("In Progress".into()), window, cx);
        ensure!(
            panel.fetch_task.is_some(),
            "Selecting status must start a real server refresh"
        );
        panel.fetch_task.take();
        panel.loading = false;
        ensure!(
            panel.query == base_query && panel.query_name == base_name,
            "Status must retain the selected base query"
        );
        ensure!(
            panel.search.read(cx).text(cx) == "analytics",
            "Status must retain search text"
        );
        ensure!(
            panel.limit == 25 && panel.tickets.is_empty(),
            "Status changes must reset pagination and clear stale rows"
        );
        ensure!(
            !panel.requests.is_current(stale_generation),
            "Status changes must invalidate older fetches"
        );
        ensure!(
            panel.effective_query()
                == "(assignee = currentUser()) AND status = \"In Progress\" ORDER BY updated DESC",
            "Status must filter the server query before pagination"
        );
        panel.select_query(
            "Team backlog".into(),
            "project = DAT ORDER BY updated DESC".into(),
            window,
            cx,
        );
        panel.fetch_task.take();
        panel.loading = false;
        ensure!(
            panel.status_filter.as_deref() == Some("In Progress"),
            "Status must survive base-query changes"
        );
        ensure!(
            panel.effective_query()
                == "(project = DAT) AND status = \"In Progress\" ORDER BY updated DESC",
            "A changed base query must retain its status clause"
        );
        panel.limit = (panel.limit + 25).min(500);
        panel.refresh(window, cx);
        panel.fetch_task.take();
        panel.loading = false;
        ensure!(
            panel.limit == 50 && panel.status_filter.as_deref() == Some("In Progress"),
            "Load more must preserve the selected status"
        );
        ensure!(
            panel.effective_query()
                == "(project = DAT) AND status = \"In Progress\" ORDER BY updated DESC",
            "Load more must request another page of the filtered query"
        );
        panel.begin_custom_status(window, cx);
        let custom = panel
            .custom_status
            .clone()
            .context("Custom status action must show its editor")?;
        ensure!(
            custom.read(cx).text(cx) == "In Progress",
            "Custom status editor must start with the selected name"
        );
        ensure!(
            !panel
                .status_options
                .iter()
                .any(|status| status == "Awaiting approval"),
            "Custom status regression must use a status absent from current options"
        );
        custom.update(cx, |editor, cx| {
            editor.set_text("  Awaiting approval  ", window, cx)
        });
        panel.apply_custom_status(window, cx);
        panel.fetch_task.take();
        panel.loading = false;
        ensure!(
            panel.status_filter.as_deref() == Some("Awaiting approval")
                && panel.custom_status.is_none(),
            "Custom status must apply its trimmed exact name"
        );
        ensure!(
            panel.effective_query()
                == "(project = DAT) AND status = \"Awaiting approval\" ORDER BY updated DESC",
            "Unlisted custom statuses must reach the server query"
        );
        ensure!(panel.limit == 25, "Custom status must restart pagination");
        panel.select_status(None, window, cx);
        panel.fetch_task.take();
        panel.loading = false;
        ensure!(
            panel.effective_query() == panel.query
                && panel.query == "project = DAT ORDER BY updated DESC",
            "Clearing status must restore the unchanged base query"
        );
        ensure!(
            panel.search.read(cx).text(cx) == "analytics",
            "Filter actions must retain search text"
        );
        ensure!(
            panel.fetch_task.is_none() && panel.refresh_task.is_none(),
            "Status exercises must leave no live commands"
        );
        Ok(())
    })
}

pub struct TicketTabExercise {
    workspace: Entity<Workspace>,
    pending: Entity<TicketView>,
}

impl TicketTabExercise {
    pub fn validate(&self, cx: &App) -> anyhow::Result<()> {
        ensure!(
            self.workspace
                .read(cx)
                .items(cx)
                .any(|item| item.item_id() == self.pending.entity_id()),
            "A pending ticket write must veto an actual pane close"
        );
        ensure!(
            !workspace::item::Item::can_close(self.pending.read(cx), cx),
            "A pending ticket write must veto close even with dirty drafts"
        );
        for ticket in self.workspace.read(cx).items_of_type::<TicketView>(cx) {
            validate_idle_ticket(ticket.read(cx))?;
        }
        Ok(())
    }
}

pub fn exercise_ticket_tab(
    panel: &Entity<WorkPanel>,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<TicketTabExercise> {
    let (workspace, query, ticket_keys, connection) = {
        let panel = panel.read(cx);
        (
            panel.workspace.clone(),
            panel.query.clone(),
            panel
                .tickets
                .iter()
                .map(|ticket| ticket.key.clone())
                .collect::<Vec<_>>(),
            panel.connection.clone(),
        )
    };
    ensure!(
        connection.executable == "/fixture/twg-must-not-run",
        "Ticket lifecycle exercises must use the offline executable"
    );
    let workspace = workspace
        .upgrade()
        .context("Ticket fixture workspace was dropped")?;

    // Drop constructor tasks before the executor polls them, keeping the actual open path offline.
    panel.update(cx, |panel, cx| {
        panel.open_ticket("DAT-1206".into(), window, cx)
    });
    let created = workspace
        .read(cx)
        .active_item_as::<TicketView>(cx)
        .context("Opening a ticket from WorkPanel must create an editor item")?;
    ensure!(
        created.read(cx).key == "DAT-1206",
        "Ticket tab must identify its ticket key"
    );
    created.update(cx, |ticket, _| {
        ticket.fetch_task.take();
        ticket.refresh_task.take();
        ticket.comments_task.take();
        ticket.loading = false;
        ticket.comments_loading = false;
    });
    let pending = ticket_fixture(workspace.downgrade(), Screen::Edit, window, cx);
    pending.update(cx, |ticket, _| {
        ticket.key = "DAT-1196".into();
        ticket.ticket.as_mut().unwrap().key = ticket.key.clone();
    });
    workspace.update(cx, |workspace, cx| {
        workspace.add_item_to_active_pane(Box::new(pending.clone()), None, true, window, cx)
    });
    ensure!(
        workspace.read(cx).active_item_as::<TicketView>(cx) == Some(pending.clone()),
        "Second ticket fixture must initially be active"
    );
    panel.update(cx, |panel, cx| {
        panel.open_ticket("dat-1206".into(), window, cx)
    });
    ensure!(
        workspace.read(cx).active_item_as::<TicketView>(cx) == Some(created.clone()),
        "Reopening a ticket must focus its existing tab"
    );
    panel.update(cx, |panel, cx| {
        panel.open_ticket("dat-1196".into(), window, cx)
    });
    ensure!(
        workspace.read(cx).active_item_as::<TicketView>(cx) == Some(pending.clone()),
        "Different ticket keys must have separate tabs"
    );
    ensure!(
        pending.read(cx).edit.as_ref().unwrap().dirty_count(cx) == 2,
        "Reopening a ticket must preserve its unsaved draft"
    );
    ensure!(
        workspace.read(cx).items_of_type::<TicketView>(cx).count() == 2,
        "Reopening a ticket must not duplicate its tab"
    );

    let other_site = ticket_fixture(workspace.downgrade(), Screen::Ticket, window, cx);
    other_site.update(cx, |ticket, _| {
        ticket.connection.site = "other.fixture.invalid".into()
    });
    workspace.update(cx, |workspace, cx| {
        workspace.add_item_to_active_pane(Box::new(other_site.clone()), Some(0), true, window, cx)
    });
    panel.update(cx, |panel, cx| {
        panel.open_ticket("DAT-1206".into(), window, cx)
    });
    ensure!(
        workspace.read(cx).active_item_as::<TicketView>(cx) == Some(created.clone()),
        "A ticket with the same key on another site must not capture navigation"
    );
    ensure!(
        workspace.read(cx).items_of_type::<TicketView>(cx).count() == 3,
        "Different sites must retain separate ticket tabs"
    );

    created.update(cx, |ticket, cx| {
        ensure!(
            !workspace::item::Item::is_dirty(ticket, cx),
            "A new ticket tab must start clean"
        );
        ticket.comment.update(cx, |editor, cx| {
            editor.set_text("Unsaved comment", window, cx)
        });
        ensure!(
            workspace::item::Item::is_dirty(ticket, cx),
            "A comment draft must mark the editor tab dirty"
        );
        anyhow::Ok(())
    })?;
    pending.update(cx, |ticket, cx| {
        ensure!(
            workspace::item::Item::is_dirty(ticket, cx),
            "An edit draft must mark the editor tab dirty"
        );
        ensure!(
            workspace::item::Item::can_close(ticket, cx),
            "Dirty drafts must allow the workspace discard prompt"
        );
        ticket.writing = true;
        anyhow::Ok(())
    })?;
    let pane = workspace.read(cx).active_pane().clone();
    pane.update(cx, |pane, cx| {
        pane.close_item_by_id(pending.entity_id(), workspace::SaveIntent::Skip, window, cx)
            .detach_and_log_err(cx);
    });
    let panel = panel.read(cx);
    ensure!(
        panel.query == query
            && panel
                .tickets
                .iter()
                .map(|ticket| ticket.key.clone())
                .collect::<Vec<_>>()
                == ticket_keys,
        "Opening a ticket must retain the panel list state"
    );
    ensure!(
        panel.work_view == WorkView::All,
        "Opening a ticket must retain the panel view"
    );
    Ok(TicketTabExercise { workspace, pending })
}

fn tickets() -> Vec<Ticket> {
    let assignee = User {
        account_id: "fixture-developer".into(),
        display_name: "Alex Morgan".into(),
    };
    [
        ("DAT-1206", "Remove duplicate analytics events", "In progress"),
        ("DAT-1196", "Simplify the event ingestion query", "In review"),
        ("DAT-1188", "Restore the missing dashboard refresh", "Blocked"),
        ("DAT-1211", "Document the attribution pipeline", "To do"),
        ("DAT-1214", "Clean up unused event dimensions", "To do"),
    ].into_iter().map(|(key, summary, status)| Ticket {
        key: key.into(), summary: summary.into(),
        status: NamedId { id: status.into(), name: status.into() },
        priority: Some(NamedId { id: "1".into(), name: "High".into() }),
        assignee: Some(assignee.clone()), labels: vec!["analytics".into(), "refactor".into()],
        sprints: Some(vec![twg::Sprint { id: Some(21), name: "Analytics sprint 21".into(), state: "active".into(), end_date: Some("2026-10-28T07:00:00.000Z".into()) }]),
        description: json!("Remove repeated analytics events before the daily aggregation.\n\n### Acceptance criteria\n\n- [x] Identify duplicate event keys\n- [ ] Preserve one record per source event\n- [ ] Add regression coverage for late arrivals"),
        url: Some(format!("https://jira.example.invalid/browse/{key}")),
        comments: vec![
            Comment { id: "fixture-comment-2".into(), author: Some(User { account_id: "fixture-reviewer".into(), display_name: "Sam Taylor".into() }), body: json!("The duplicate-key example looks good. Please include the late-arrival case in the regression check."), created: Some("2026-10-27 14:20".into()) },
            Comment { id: "fixture-comment-1".into(), author: Some(assignee.clone()), body: json!("I traced the repeated events to the merge step. The fix keeps the earliest source record."), created: Some("2026-10-26 09:45".into()) },
        ],
        ..Default::default()
    }).collect()
}
