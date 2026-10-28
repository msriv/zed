mod pr_list;
mod pull_request_view;
mod state;
mod ticket_editor;
mod ticket_view;
mod work_log;

#[cfg(any(test, feature = "test-support"))]
pub mod visual_fixture;

use std::time::{Duration, Instant};

use editor::{Editor, EditorEvent};
use gpui::{
    App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    Pixels, Subscription, Task, WeakEntity, Window, actions, prelude::*,
};
use settings::{DockSide, RegisterSetting, Settings};
use twg::{Client, SavedFilter, Ticket, TwgSettings};
use ui::{ContextMenu, DropdownMenu, DropdownStyle, TintColor, Tooltip, prelude::*};
use util::ResultExt;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use pr_list::PrList;
use ticket_editor::{EditDraft, StatusDraft};
use ticket_view::{TicketEvent, TicketView};
use work_log::WorkLogDraft;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkView {
    All,
    Tickets,
    PullRequests,
}

actions!(
    work_panel,
    [ToggleFocus, Refresh, Save, Cancel, SubmitComment]
);

#[derive(Clone, RegisterSetting)]
pub struct WorkPanelSettings {
    pub dock: DockSide,
    pub default_width: Pixels,
    pub button: bool,
}

impl Settings for WorkPanelSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let panel = content.work_panel.as_ref();
        Self {
            dock: panel.and_then(|panel| panel.dock).unwrap_or(DockSide::Left),
            default_width: px(panel.and_then(|panel| panel.default_width).unwrap_or(420.0)),
            button: panel.and_then(|panel| panel.button).unwrap_or(true),
        }
    }
}

pub fn init(cx: &mut App) {
    WorkPanelSettings::register(cx);
    cx.bind_keys([
        KeyBinding::new("cmd-s", Save, Some("WorkPanel")),
        KeyBinding::new("ctrl-s", Save, Some("WorkPanel")),
        KeyBinding::new("escape", Cancel, Some("WorkPanel")),
        KeyBinding::new("cmd-enter", SubmitComment, Some("WorkPanel")),
    ]);
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<WorkPanel>(window, cx);
        });
    })
    .detach();
}

pub struct WorkPanel {
    pr_list: Entity<PrList>,
    work_view: WorkView,
    workspace: WeakEntity<Workspace>,
    connection: TwgSettings,
    focus_handle: FocusHandle,
    search: Entity<Editor>,
    tickets: Vec<Ticket>,
    filters: Vec<SavedFilter>,
    query: String,
    query_name: String,
    status_filter: Option<String>,
    status_options: Vec<String>,
    custom_status: Option<Entity<Editor>>,
    limit: usize,
    loading: bool,
    error: Option<String>,
    synced: Option<Instant>,
    requests: state::Requests,
    fetch_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    subscriptions: Vec<Subscription>,
}

impl WorkPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        cx.update(|window, cx| {
            cx.new(|cx| {
                let pr_list = cx.new(|cx| PrList::new(workspace.clone(), window, cx));
                let search = make_editor("Filter, or DAT-1206", "", 1, window, cx);
                let mut subscriptions =
                    vec![
                        cx.subscribe_in(&search, window, |_: &mut Self, _, event, _, cx| {
                            if matches!(event, EditorEvent::BufferEdited) {
                                cx.notify();
                            }
                        }),
                    ];
                subscriptions.push(cx.observe(&pr_list, |_, _, cx| cx.notify()));
                let mut panel = Self {
                    pr_list,
                    work_view: WorkView::All,
                    workspace,
                    connection: TwgSettings::get_global(cx).clone(),
                    focus_handle: cx.focus_handle(),
                    search,
                    tickets: Vec::new(),
                    filters: Vec::new(),
                    query: state::assigned_query(&TwgSettings::get_global(cx).default_project),
                    query_name: "Assigned to me".into(),
                    status_filter: None,
                    status_options: Vec::new(),
                    custom_status: None,
                    limit: 25,
                    loading: false,
                    error: None,
                    synced: None,
                    requests: state::Requests::default(),
                    fetch_task: None,
                    refresh_task: None,
                    subscriptions,
                };
                panel.refresh(window, cx);
                panel.start_refresh_timer(window, cx);
                panel
            })
        })
    }

    fn connection_is_current(&self, cx: &App) -> bool {
        let current = TwgSettings::get_global(cx);
        state::same_connection(
            &self.connection.executable,
            &self.connection.site,
            &current.executable,
            &current.site,
        )
    }

    fn guard_connection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.connection_is_current(cx) {
            return true;
        }
        self.error =
            Some("TWG connection changed. Refresh the Work list before opening a ticket.".into());
        cx.notify();
        false
    }

    fn bound_client(&mut self, cx: &mut Context<Self>) -> Option<Client> {
        self.guard_connection(cx).then(|| self.connection.client())
    }

    fn start_refresh_timer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let minutes = cx
                    .update(|_, cx| TwgSettings::get_global(cx).refresh_interval_minutes)
                    .unwrap_or(5);
                cx.background_executor()
                    .timer(state::refresh_interval(minutes).unwrap_or(Duration::from_secs(60)))
                    .await;
                if minutes == 0 {
                    continue;
                }
                if this
                    .update_in(cx, |this, window, cx| {
                        this.refresh(window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connection_is_current(cx) {
            self.requests.next();
            self.fetch_task = None;
            self.loading = false;
            self.connection = TwgSettings::get_global(cx).clone();
            self.tickets.clear();
            self.filters.clear();
            self.synced = None;
            self.limit = 25;
            self.query = state::assigned_query(&self.connection.default_project);
            self.query_name = "Assigned to me".into();
            self.status_filter = None;
            self.status_options.clear();
            self.custom_status = None;
            self.search
                .update(cx, |editor, cx| editor.set_text("", window, cx));
        }
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        let query = self.effective_query();
        let limit = self.limit;
        let generation = self.requests.next();
        self.loading = true;
        self.error = None;
        self.fetch_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let tickets = client.query_tickets(&query, limit).await?;
                    let filters = client.filters().await;
                    anyhow::Ok((tickets, filters))
                })
                .await;
            this.update(cx, |this, cx| {
                if !this.requests.is_current(generation) {
                    return;
                }
                this.loading = false;
                if !this.guard_connection(cx) {
                    return;
                }
                match result {
                    Ok((tickets, filters)) => {
                        this.status_options = state::status_options(
                            this.status_options
                                .iter()
                                .map(String::as_str)
                                .chain(tickets.iter().map(|ticket| ticket.status.name.as_str())),
                        );
                        this.tickets = tickets;
                        match filters {
                            Ok(filters) => this.filters = filters,
                            Err(error) => {
                                this.error = Some(format!(
                                    "Tickets loaded; saved queries unavailable: {error:#}"
                                ))
                            }
                        }
                        this.synced = Some(Instant::now());
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn open_ticket(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard_connection(cx) {
            return;
        }
        let connection = self.connection.clone();
        let pr_list = self.pr_list.clone();
        let workspace_handle = self.workspace.clone();
        let result = self.workspace.update(cx, |workspace, cx| {
            let existing = workspace
                .items_of_type::<TicketView>(cx)
                .find(|view| view.read(cx).matches(&connection, &key));
            if let Some(view) = existing {
                workspace.activate_item(&view, true, true, window, cx);
                return None;
            }
            let view = cx
                .new(|cx| TicketView::new(workspace_handle, pr_list, connection, key, window, cx));
            workspace.add_item_to_active_pane(Box::new(view.clone()), None, true, window, cx);
            Some(view)
        });
        match result {
            Ok(Some(view)) => self.subscriptions.push(cx.subscribe_in(
                &view,
                window,
                |this, _, event, window, cx| {
                    if matches!(event, TicketEvent::Updated) {
                        this.refresh(window, cx);
                    }
                },
            )),
            Ok(None) => {}
            Err(error) => {
                self.error = Some(format!("Could not open ticket: {error:#}"));
                cx.notify();
            }
        }
    }

    fn select_query(
        &mut self,
        name: String,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query_name = name;
        self.query = query;
        self.reset_query(window, cx);
    }

    fn effective_query(&self) -> String {
        state::status_query(&self.query, self.status_filter.as_deref())
    }

    fn reset_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.limit = 25;
        self.tickets.clear();
        self.synced = None;
        self.refresh(window, cx);
    }

    fn select_status(
        &mut self,
        status: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.status_filter = status
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        self.status_options = state::status_options(
            self.status_options
                .iter()
                .map(String::as_str)
                .chain(self.status_filter.as_deref()),
        );
        self.custom_status = None;
        self.reset_query(window, cx);
    }

    fn begin_custom_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = make_editor(
            "Exact Jira status, e.g. In Progress",
            self.status_filter.as_deref().unwrap_or(""),
            1,
            window,
            cx,
        );
        window.focus(&editor.focus_handle(cx), cx);
        self.custom_status = Some(editor);
        cx.notify();
    }

    fn apply_custom_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.custom_status else {
            return;
        };
        self.select_status(Some(editor.read(cx).text(cx)), window, cx);
    }

    fn lookup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.search.read(cx).text(cx).trim().to_string();
        if is_ticket_key(&search) {
            self.open_ticket(search.to_uppercase(), window, cx);
        } else if !search.is_empty() {
            let escaped = search.replace('\\', "\\\\").replace('"', "\\\"");
            self.select_query(
                format!("Search: {search}"),
                format!("text ~ \"{escaped}\" ORDER BY updated DESC"),
                window,
                cx,
            );
            self.search
                .update(cx, |editor, cx| editor.set_text("", window, cx));
        }
    }

    fn render_list(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let search = self.search.read(cx).text(cx).to_lowercase();
        let panel = cx.entity();
        let filters = self.filters.clone();
        let menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let assigned_panel = panel.clone();
            let mut menu = menu.entry("Assigned to me", None, move |window, cx| {
                assigned_panel.update(cx, |this, cx| {
                    this.select_query(
                        "Assigned to me".into(),
                        state::assigned_query(&TwgSettings::get_global(cx).default_project),
                        window,
                        cx,
                    );
                })
            });
            let sprint_panel = panel.clone();
            menu = menu.entry("Current sprint", None, move |window, cx| {
                sprint_panel.update(cx, |this, cx| {
                    this.select_query(
                        "Current sprint".into(),
                        state::current_sprint_query(&TwgSettings::get_global(cx).default_project),
                        window,
                        cx,
                    );
                });
            });
            for filter in filters {
                let filter_panel = panel.clone();
                menu = menu.entry(filter.name.clone(), None, move |window, cx| {
                    filter_panel.update(cx, |this, cx| {
                        this.select_query(filter.name.clone(), filter.jql.clone(), window, cx);
                    })
                });
            }
            menu
        });
        let panel = cx.entity();
        let statuses = state::status_options(self.status_options.iter().map(String::as_str));
        let selected_status = self.status_filter.clone();
        let status_menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let all_panel = panel.clone();
            let mut menu = menu.toggleable_entry(
                "All statuses",
                selected_status.is_none(),
                ui::IconPosition::Start,
                None,
                move |window, cx| {
                    all_panel.update(cx, |panel, cx| panel.select_status(None, window, cx));
                },
            );
            for status in statuses {
                let status_panel = panel.clone();
                let selected = selected_status
                    .as_ref()
                    .is_some_and(|selected| selected.eq_ignore_ascii_case(&status));
                menu = menu.toggleable_entry(
                    status.clone(),
                    selected,
                    ui::IconPosition::Start,
                    None,
                    move |window, cx| {
                        status_panel.update(cx, |panel, cx| {
                            panel.select_status(Some(status.clone()), window, cx)
                        });
                    },
                );
            }
            menu.separator()
                .entry("Custom status…", None, move |window, cx| {
                    panel.update(cx, |panel, cx| panel.begin_custom_status(window, cx));
                })
        });
        v_flex()
            .size_full()
            .gap_2()
            .p_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(ticket_editor::editor_field(
                        "",
                        self.search.clone(),
                        true,
                        cx,
                    )))
                    .child(
                        Button::new("lookup", "Search server")
                            .tab_index(0_isize)
                            .on_click(cx.listener(|this, _, window, cx| this.lookup(window, cx))),
                    ),
            )
            .child(
                DropdownMenu::new("queries", self.query_name.clone(), menu)
                    .tab_index(0_isize)
                    .style(DropdownStyle::Outlined),
            )
            .child(
                DropdownMenu::new_with_element(
                    "ticket-status-filter",
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_left()
                        .child(
                            Label::new(format!(
                                "Status: {}",
                                self.status_filter.as_deref().unwrap_or("All statuses")
                            ))
                            .truncate(),
                        )
                        .into_any_element(),
                    status_menu,
                )
                .full_width(true)
                .tab_index(0_isize)
                .aria_label("Ticket status")
                .aria_value(
                    self.status_filter
                        .clone()
                        .unwrap_or_else(|| "All statuses".into()),
                )
                .style(DropdownStyle::Outlined)
                .trigger_tooltip(Tooltip::text(
                    self.status_filter
                        .clone()
                        .unwrap_or_else(|| "All statuses".into()),
                )),
            )
            .when_some(self.custom_status.clone(), |this, editor| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(ticket_editor::editor_field(
                            "Custom status",
                            editor,
                            true,
                            cx,
                        ))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("apply-status-filter", "Apply")
                                        .tab_index(0_isize)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_custom_status(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("cancel-status-filter", "Cancel")
                                        .tab_index(0_isize)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.custom_status = None;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
            .child(
                Label::new("MY TICKETS")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                v_flex()
                    .id("work-tickets")
                    .flex_1()
                    .overflow_y_scroll()
                    .gap_1()
                    .when(self.loading && self.tickets.is_empty(), |this| {
                        this.child(Label::new("Loading work…").color(Color::Muted))
                    })
                    .when(!self.loading && self.tickets.is_empty(), |this| {
                        this.child(Label::new("No tickets in this query.").color(Color::Muted))
                    })
                    .children(
                        self.tickets
                            .iter()
                            .filter(|ticket| {
                                search.is_empty()
                                    || ticket.key.to_lowercase().contains(&search)
                                    || ticket.summary.to_lowercase().contains(&search)
                            })
                            .map(|ticket| {
                                let key = ticket.key.clone();
                                ui::ButtonLike::new(SharedString::from(format!("ticket-{key}")))
                                    .tab_index(0_isize)
                                    .full_width()
                                    .auto_height()
                                    .style(ButtonStyle::Subtle)
                                    .aria_label(format!(
                                        "{}: {}, {}",
                                        ticket.key, ticket.summary, ticket.status.name
                                    ))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_start()
                                            .text_left()
                                            .gap_2()
                                            .p_2()
                                            .border_b_1()
                                            .border_color(cx.theme().colors().border_variant)
                                            .child(
                                                Label::new(ticket.key.clone())
                                                    .color(Color::Accent)
                                                    .flex_shrink_0(),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .overflow_hidden()
                                                    .child(Label::new(ticket.summary.clone())),
                                            )
                                            .child(
                                                Label::new(ticket.status.name.clone())
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted)
                                                    .flex_shrink_0(),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_ticket(key.clone(), window, cx)
                                    }))
                            }),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(Label::new(format!("{} loaded", self.tickets.len())).color(Color::Muted))
                    .when(
                        self.tickets.len() >= self.limit && self.limit < 500,
                        |this| {
                            this.child(
                                Button::new("load-more", "Load 25 more")
                                    .tab_index(0_isize)
                                    .disabled(self.loading)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.limit = (this.limit + 25).min(500);
                                        this.refresh(window, cx);
                                    })),
                            )
                        },
                    ),
            )
            .into_any_element()
    }

    fn render_work_list(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.work_view {
            WorkView::Tickets => self.render_list(window, cx),
            WorkView::PullRequests => v_flex()
                .id("work-pr-list")
                .size_full()
                .overflow_y_scroll()
                .child(self.pr_list.clone())
                .into_any_element(),
            WorkView::All => v_flex()
                .size_full()
                .child(div().flex_1().min_h_0().child(self.render_list(window, cx)))
                .child(
                    div()
                        .id("work-pr-summary")
                        .border_t_1()
                        .border_color(cx.theme().colors().border)
                        .max_h(px(300.0))
                        .overflow_y_scroll()
                        .child(self.pr_list.clone()),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .child(
                h_flex().px_3().gap_2().children(
                    [
                        (WorkView::All, "All"),
                        (WorkView::Tickets, "Tickets"),
                        (WorkView::PullRequests, "PRs"),
                    ]
                    .into_iter()
                    .map(|(view, label)| {
                        Button::new(label, label)
                            .tab_index(0_isize)
                            .style(if self.work_view == view {
                                ButtonStyle::Tinted(TintColor::Accent)
                            } else {
                                ButtonStyle::Subtle
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.work_view = view;
                                if view != WorkView::Tickets {
                                    this.pr_list
                                        .update(cx, |list, cx| list.refresh(false, window, cx));
                                }
                                cx.notify();
                            }))
                    }),
                ),
            )
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }
}

fn make_editor<T: 'static>(
    placeholder: &str,
    text: &str,
    lines: usize,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Entity<Editor> {
    cx.new(|cx| {
        let mut editor = if lines == 1 {
            Editor::single_line(window, cx)
        } else {
            Editor::auto_height(lines, lines.max(12), window, cx)
        };
        editor.set_placeholder_text(placeholder, window, cx);
        editor.set_text(text, window, cx);
        editor
    })
}

fn is_ticket_key(text: &str) -> bool {
    text.rsplit_once('-').is_some_and(|(project, number)| {
        !project.is_empty()
            && project
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            && !number.is_empty()
            && number.chars().all(|character| character.is_ascii_digit())
    })
}

impl Render for WorkPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("work-panel")
            .key_context("WorkPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background.opacity(1.0))
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.refresh(window, cx)))
            .child(
                h_flex()
                    .p_3()
                    .justify_between()
                    .child(Label::new("Work").size(LabelSize::Large))
                    .child(
                        IconButton::new("refresh-work", IconName::RotateCw)
                            .tab_index(0_isize)
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().p_3().child(Label::new(error).color(Color::Warning)))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_work_list(window, cx)),
            )
            .child(
                h_flex()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .justify_between()
                    .child(
                        Label::new(if self.loading {
                            "Syncing via twg…".into()
                        } else if let Some(synced) = self.synced {
                            format!("Synced {}m ago via twg", synced.elapsed().as_secs() / 60)
                        } else {
                            "Not synced".into()
                        })
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .child(
                        Button::new("connections", "Connections")
                            .tab_index(0_isize)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(zed_actions::OpenSettingsPage {
                                        page: "twg".into(),
                                        target: None,
                                    }),
                                    cx,
                                );
                            }),
                    ),
            )
    }
}

impl Focusable for WorkPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for WorkPanel {}
impl Panel for WorkPanel {
    fn persistent_name() -> &'static str {
        "Work"
    }
    fn panel_key() -> &'static str {
        "WorkPanel"
    }
    fn position(&self, _: &Window, cx: &App) -> DockPosition {
        match WorkPanelSettings::get_global(cx).dock {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }
    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            let fs = workspace.read(cx).project().read(cx).fs().clone();
            settings::update_settings_file(fs, cx, move |settings, _| {
                settings.work_panel.get_or_insert_default().dock =
                    Some(if position == DockPosition::Right {
                        DockSide::Right
                    } else {
                        DockSide::Left
                    });
            });
        }
    }
    fn default_size(&self, _: &Window, cx: &App) -> Pixels {
        WorkPanelSettings::get_global(cx).default_width
    }
    fn min_size(&self, _: &Window, _: &App) -> Option<Pixels> {
        Some(px(280.0))
    }
    fn icon(&self, _: &Window, cx: &App) -> Option<IconName> {
        WorkPanelSettings::get_global(cx)
            .button
            .then_some(IconName::ListTodo)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Work")
    }
    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }
    fn activation_priority(&self) -> u32 {
        7
    }
}
