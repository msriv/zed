use agent_ui::{AgentPanel, ExternalSourcePrompt};
use gpui::ClipboardItem;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use workspace::item::{Item, ItemEvent, SaveOptions};

use super::*;

pub(super) enum TicketEvent {
    Item(ItemEvent),
    Updated,
}

pub(super) struct TicketView {
    pub(super) pr_list: Entity<PrList>,
    pub(super) workspace: WeakEntity<Workspace>,
    pub(super) connection: TwgSettings,
    pub(super) key: String,
    pub(super) focus_handle: FocusHandle,
    pub(super) comment: Entity<Editor>,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    pub(super) notice: Option<String>,
    pub(super) synced: Option<Instant>,
    pub(super) ticket: Option<Ticket>,
    pub(super) description: Option<Entity<Markdown>>,
    pub(super) comments: Vec<Entity<Markdown>>,
    pub(super) comment_total: Option<usize>,
    pub(super) comment_cursor: Option<String>,
    pub(super) comments_loading: bool,
    pub(super) comment_requests: state::Requests,
    pub(super) comments_task: Option<Task<()>>,
    pub(super) edit: Option<EditDraft>,
    pub(super) status: Option<StatusDraft>,
    pub(super) work_log: Option<WorkLogDraft>,
    pub(super) writing: bool,
    pub(super) requests: state::Requests,
    pub(super) fetch_task: Option<Task<()>>,
    pub(super) write_task: Option<Task<()>>,
    pub(super) refresh_task: Option<Task<()>>,
    pub(super) subscriptions: Vec<Subscription>,
}

impl TicketView {
    pub(super) fn new(
        workspace: WeakEntity<Workspace>,
        pr_list: Entity<PrList>,
        connection: TwgSettings,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let comment = make_editor("Comment on this ticket…", "", 3, window, cx);
        let subscriptions = vec![
            cx.subscribe(&comment, |_, _, event, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    cx.emit(TicketEvent::Item(ItemEvent::Edit));
                    cx.notify();
                }
            }),
            cx.observe(&pr_list, |_, _, cx| cx.notify()),
            cx.observe_self(|_, cx| cx.emit(TicketEvent::Item(ItemEvent::UpdateTab))),
        ];
        let mut view = Self {
            workspace,
            pr_list,
            connection,
            key: key.to_uppercase(),
            focus_handle: cx.focus_handle(),
            comment,
            loading: false,
            error: None,
            notice: None,
            synced: None,
            ticket: None,
            description: None,
            comments: Vec::new(),
            comment_total: None,
            comment_cursor: None,
            comments_loading: false,
            comment_requests: state::Requests::default(),
            comments_task: None,
            edit: None,
            status: None,
            work_log: None,
            writing: false,
            requests: state::Requests::default(),
            fetch_task: None,
            write_task: None,
            refresh_task: None,
            subscriptions,
        };
        view.refresh(window, cx);
        view.start_refresh_timer(window, cx);
        view
    }

    pub(super) fn matches(&self, connection: &TwgSettings, key: &str) -> bool {
        state::same_connection(
            &self.connection.executable,
            &self.connection.site,
            &connection.executable,
            &connection.site,
        ) && self.key.eq_ignore_ascii_case(key)
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

    pub(super) fn guard_connection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.connection_is_current(cx) {
            return true;
        }
        self.error = Some("TWG connection changed. Your drafts are retained. This ticket tab stays bound to its original connection. Restore that connection to refresh or submit changes.".into());
        cx.notify();
        false
    }

    pub(super) fn bound_client(&mut self, cx: &mut Context<Self>) -> Option<Client> {
        self.guard_connection(cx).then(|| self.connection.client())
    }

    fn has_unsaved_drafts(&self, cx: &App) -> bool {
        !self.comment.read(cx).text(cx).trim().is_empty()
            || self
                .edit
                .as_ref()
                .is_some_and(|draft| draft.dirty_count(cx) > 0)
            || self.status.as_ref().is_some_and(|draft| draft.is_dirty(cx))
            || self
                .work_log
                .as_ref()
                .is_some_and(|draft| draft.is_dirty(cx))
    }

    pub(super) fn finish_write(
        &mut self,
        result: twg::WriteResult,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TicketEvent::Updated);
        self.notice = Some(message.to_string());
        self.error = result
            .verification_error
            .map(|error| format!("{message} Refresh to verify: {error}"));
        if self.guard_connection(cx) {
            if let Some(mut ticket) = result.ticket {
                retain_known_sprints(&mut ticket, self.ticket.as_ref());
                self.set_ticket(ticket, window, cx);
            }
        }
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
                        if !this.writing
                            && this.edit.is_none()
                            && this.status.is_none()
                            && this.work_log.is_none()
                        {
                            this.refresh(window, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        if !self.connection_is_current(cx) {
            self.requests.next();
            self.comment_requests.next();
            self.fetch_task = None;
            self.comments_task = None;
            self.loading = false;
            self.comments_loading = false;
            self.guard_connection(cx);
            return;
        }
        if self.edit.is_some() || self.status.is_some() || self.work_log.is_some() {
            return;
        }
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        let key = self.key.clone();
        let generation = self.requests.next();
        self.loading = true;
        self.error = None;
        self.fetch_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { client.get_ticket_details(&key).await })
                .await;
            this.update_in(cx, |this, window, cx| {
                if !this.requests.is_current(generation) {
                    return;
                }
                this.loading = false;
                if !this.guard_connection(cx) {
                    return;
                }
                match result {
                    Ok(details) => {
                        this.set_ticket(details.ticket, window, cx);
                        this.error = details.warning;
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn set_ticket(&mut self, ticket: Ticket, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard_connection(cx) {
            return;
        }
        let description = twg::rich_text::project(&ticket.description).markdown;
        self.description = Some(cx.new(|cx| Markdown::new(description.into(), None, None, cx)));
        self.comments = ticket
            .comments
            .iter()
            .map(|comment| {
                let body = twg::rich_text::project(&comment.body).markdown;
                cx.new(|cx| Markdown::new(body.into(), None, None, cx))
            })
            .collect();
        self.ticket = Some(ticket);
        self.comment_total = None;
        self.comment_cursor = None;
        self.synced = Some(Instant::now());
        self.load_comments(None, window, cx);
    }

    fn load_comments(
        &mut self,
        after: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.writing {
            return;
        }
        let Some(ticket) = &self.ticket else {
            return;
        };
        let key = ticket.key.clone();
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        let generation = self.comment_requests.next();
        self.comments_loading = true;
        self.comments_task = Some(cx.spawn_in(window, async move |this, cx| {
            let append = after.is_some();
            let result = cx
                .background_spawn(async move { client.comments(&key, 25, after.as_deref()).await })
                .await;
            this.update(cx, |this, cx| {
                if !this.comment_requests.is_current(generation) {
                    return;
                }
                this.comments_loading = false;
                if !this.guard_connection(cx) {
                    return;
                }
                match result {
                    Ok(page) => {
                        if let Some(ticket) = &mut this.ticket {
                            if append {
                                append_comments(&mut ticket.comments, page.comments);
                            } else {
                                ticket.comments = page.comments;
                            }
                            this.comment_total = page.total;
                            this.comment_cursor = page.next_cursor;
                            this.comments = ticket
                                .comments
                                .iter()
                                .map(|comment| {
                                    let body = twg::rich_text::project(&comment.body).markdown;
                                    cx.new(|cx| Markdown::new(body.into(), None, None, cx))
                                })
                                .collect();
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "Could not load comments; your draft is retained: {error:#}"
                        ))
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
    }

    pub(super) fn allow_navigation(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.comment.read(cx).text(cx).trim().is_empty() {
            self.error =
                Some("Post or clear your comment draft before leaving this ticket.".into());
            cx.notify();
            return false;
        }
        let dirty = self
            .edit
            .as_ref()
            .map(|draft| draft.dirty_count(cx))
            .unwrap_or(0);
        if !state::can_navigate(dirty, self.writing)
            || self.status.is_some()
            || self.work_log.is_some()
        {
            self.error = Some("Save or cancel your changes before leaving this ticket.".into());
            cx.notify();
            return false;
        }
        self.edit = None;
        true
    }

    pub(super) fn ticket_url(&self, _cx: &App) -> Option<String> {
        let ticket = self.ticket.as_ref()?;
        ticket.url.clone().or_else(|| {
            let site = &self.connection.site;
            (!site.is_empty()).then(|| {
                let site = site.trim_end_matches('/');
                format!(
                    "{}{site}/browse/{}",
                    if site.contains("://") { "" } else { "https://" },
                    ticket.key
                )
            })
        })
    }

    fn send_to_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ticket) = &self.ticket else {
            return;
        };
        let body = twg::rich_text::project(&ticket.description).markdown;
        let prompt = ExternalSourcePrompt::new(&format!(
            "{}: {}\n{}\n\n{}",
            ticket.key,
            ticket.summary,
            self.ticket_url(cx).unwrap_or_default(),
            body
        ));
        let result = self.workspace.update(cx, |workspace, cx| {
            let Some(panel) = workspace.focus_panel::<AgentPanel>(window, cx) else {
                return false;
            };
            window.defer(cx, move |window, cx| {
                panel.update(cx, |panel, cx| {
                    panel.new_agent_thread_with_external_source_prompt(prompt, window, cx)
                });
            });
            true
        });
        match result {
            Ok(true) => {}
            Ok(false) => {
                self.error = Some("Enable the Agent panel before sending a ticket.".into())
            }
            Err(error) => self.error = Some(format!("Could not open the Agent panel: {error:#}")),
        }
        cx.notify();
    }

    fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading
            || self.writing
            || self.edit.is_some()
            || self.status.is_some()
            || self.work_log.is_some()
        {
            return;
        }
        let Some(ticket) = &self.ticket else {
            return;
        };
        let key = ticket.key.clone();
        let body = self.comment.read(cx).text(cx);
        if body.trim().is_empty() {
            return;
        }
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        self.writing = true;
        self.error = None;
        self.notice = None;
        self.requests.next();
        self.comment_requests.next();
        self.comments_loading = false;
        let submitted = body.clone();
        self.write_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { client.add_comment(&key, &body).await })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.writing = false;
                match result {
                    Ok(result) => {
                        if state::submitted_comment_is_current(
                            &submitted,
                            &this.comment.read(cx).text(cx),
                        ) {
                            this.comment
                                .update(cx, |editor, cx| editor.set_text("", window, cx));
                        }
                        this.finish_write(result, "Comment posted to Jira.", window, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Comment failed; draft retained: {error:#}"))
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn render_ticket(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(ticket) = self.ticket.clone() else {
            return v_flex()
                .p_3()
                .gap_3()
                .child(Label::new(self.key.clone()).color(Color::Accent))
                .child(
                    Label::new(if self.loading {
                        "Loading ticket…"
                    } else {
                        "Ticket details unavailable. Refresh to retry."
                    })
                    .color(Color::Muted),
                )
                .child(
                    Button::new("retry-ticket", "Refresh")
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                )
                .into_any_element();
        };
        let related_prs = self.pr_list.read(cx).related(&ticket.key);
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .gap_2()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new(ticket.key.clone()).color(Color::Accent))
                    .child(div().flex_1())
                    .child(
                        IconButton::new("refresh-ticket", IconName::RotateCw)
                            .tab_index(0_isize)
                            .disabled(self.loading || self.writing)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    )
                    .child(
                        Button::new("edit", "Edit")
                            .tab_index(0_isize)
                            .disabled(self.loading || self.writing)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.begin_edit(window, cx)),
                            ),
                    )
                    .child(
                        IconButton::new("copy", IconName::Copy)
                            .tab_index(0_isize)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(ticket) = &this.ticket {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        ticket.key.clone(),
                                    ));
                                }
                            })),
                    )
                    .when(self.ticket_url(cx).is_some(), |this| {
                        this.child(
                            IconButton::new("browser", IconName::ArrowUpRight)
                                .tab_index(0_isize)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(url) = this.ticket_url(cx) {
                                        cx.open_url(&url);
                                    }
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("ticket-details")
                    .flex_1()
                    .overflow_y_scroll()
                    .gap_4()
                    .p_3()
                    .child(Label::new(ticket.summary.clone()).size(LabelSize::Large))
                    .child(
                        h_flex()
                            .gap_4()
                            .child(Label::new("Status").color(Color::Muted))
                            .child(
                                Button::new("change-status", ticket.status.name.clone())
                                    .tab_index(0_isize)
                                    .disabled(self.loading || self.writing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.begin_status(window, cx)
                                    })),
                            ),
                    )
                    .child(metadata_row(
                        "Assignee",
                        ticket
                            .assignee
                            .as_ref()
                            .map(|user| user.display_name.as_str())
                            .unwrap_or("Unassigned"),
                    ))
                    .child(metadata_row(
                        "Priority",
                        ticket
                            .priority
                            .as_ref()
                            .map(|priority| priority.name.as_str())
                            .unwrap_or("None"),
                    ))
                    .child(metadata_row(
                        "Sprint",
                        &sprint_summary(ticket.sprints.as_deref()),
                    ))
                    .child(metadata_row("Labels", &ticket.labels.join(", ")))
                    .children(related_prs.into_iter().map(|reference| {
                        let workspace = self.workspace.clone();
                        Button::new(
                            SharedString::from(format!("ticket-pr-{}", reference.number)),
                            format!("PR #{} · Open diff →", reference.number),
                        )
                        .tab_index(0_isize)
                        .on_click(move |_, window, cx| {
                            pull_request_view::open_pull_request(
                                &workspace,
                                reference.clone(),
                                window,
                                cx,
                            )
                        })
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("log-work", "Log work")
                                    .tab_index(0_isize)
                                    .disabled(self.loading || self.writing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.begin_work_log(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("send-agent", "Send to agent")
                                    .tab_index(0_isize)
                                    .disabled(self.loading || self.writing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.send_to_agent(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        Label::new("DESCRIPTION")
                            .color(Color::Muted)
                            .size(LabelSize::Small),
                    )
                    .children(self.description.clone().map(|description| {
                        MarkdownElement::new(
                            description,
                            MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                        )
                    }))
                    .child(
                        Label::new(match self.comment_total {
                            Some(total) => {
                                format!("ACTIVITY · {} OF {total} COMMENTS", ticket.comments.len())
                            }
                            None => format!("ACTIVITY · {} COMMENTS", ticket.comments.len()),
                        })
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                    )
                    .children(ticket.comments.iter().zip(&self.comments).map(
                        |(comment, markdown)| {
                            v_flex()
                                .gap_1()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(Label::new(
                                            comment
                                                .author
                                                .as_ref()
                                                .map(|author| author.display_name.clone())
                                                .unwrap_or_else(|| "Unknown author".into()),
                                        ))
                                        .child(
                                            Label::new(comment.created.clone().unwrap_or_default())
                                                .color(Color::Muted)
                                                .size(LabelSize::Small),
                                        ),
                                )
                                .child(MarkdownElement::new(
                                    markdown.clone(),
                                    MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                                ))
                        },
                    )),
            )
            .child(
                h_flex()
                    .px_3()
                    .gap_2()
                    .when(self.comments_loading, |this| {
                        this.child(Label::new("Loading comments…").color(Color::Muted))
                    })
                    .when_some(self.comment_cursor.clone(), |this, cursor| {
                        this.child(
                            Button::new("older-comments", "Load older comments")
                                .tab_index(0_isize)
                                .disabled(self.comments_loading || self.writing)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.load_comments(Some(cursor.clone()), window, cx)
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(ticket_editor::editor_field(
                        "",
                        self.comment.clone(),
                        true,
                        cx,
                    ))
                    .child(
                        Button::new(
                            "post-comment",
                            if self.writing {
                                "Posting…"
                            } else {
                                "Comment ⌘↵"
                            },
                        )
                        .tab_index(0_isize)
                        .disabled(self.writing || self.loading)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.submit_comment(window, cx)),
                        ),
                    ),
            )
            .into_any_element()
    }
}

pub(super) fn metadata_row(label: &str, value: &str) -> AnyElement {
    h_flex()
        .gap_4()
        .child(
            div()
                .w_24()
                .child(Label::new(label.to_string()).color(Color::Muted)),
        )
        .child(Label::new(value.to_string()))
        .into_any_element()
}

fn retain_known_sprints(ticket: &mut Ticket, previous: Option<&Ticket>) {
    if ticket.sprints.is_none()
        && let Some(previous) =
            previous.filter(|previous| previous.key.eq_ignore_ascii_case(&ticket.key))
    {
        ticket.sprints = previous.sprints.clone();
    }
}

fn sprint_summary(sprints: Option<&[twg::Sprint]>) -> String {
    match sprints {
        None => "Unavailable".into(),
        Some([]) => "None".into(),
        Some(sprints) => sprints
            .iter()
            .map(|sprint| {
                let mut details = vec![sprint.name.clone()];
                if !sprint.state.is_empty() {
                    details.push(sprint.state.clone());
                }
                if let Some(end_date) = &sprint.end_date {
                    details.push(format!(
                        "ends {}",
                        end_date.split('T').next().unwrap_or(end_date)
                    ));
                }
                details.join(" · ")
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

fn append_comments(comments: &mut Vec<twg::Comment>, older: Vec<twg::Comment>) {
    for comment in older {
        if comment.id.is_empty() || !comments.iter().any(|existing| existing.id == comment.id) {
            comments.push(comment);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_readback_keeps_sprints_only_for_the_same_ticket() {
        let previous = Ticket {
            key: "TEST-1".into(),
            sprints: Some(vec![twg::Sprint {
                name: "Sprint 21".into(),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let mut basic = Ticket {
            key: "test-1".into(),
            ..Default::default()
        };
        retain_known_sprints(&mut basic, Some(&previous));
        assert_eq!(
            basic.sprints.as_ref().expect("retained")[0].name,
            "Sprint 21"
        );
        let mut other = Ticket {
            key: "TEST-2".into(),
            ..Default::default()
        };
        retain_known_sprints(&mut other, Some(&previous));
        assert!(other.sprints.is_none());
        let mut empty = Ticket {
            key: "TEST-1".into(),
            sprints: Some(Vec::new()),
            ..Default::default()
        };
        retain_known_sprints(&mut empty, Some(&previous));
        assert!(empty.sprints.is_some_and(|sprints| sprints.is_empty()));
    }

    #[test]
    fn sprint_row_uses_returned_state_and_end_date() {
        let sprint = twg::Sprint {
            name: "Analytics sprint 21".into(),
            state: "active".into(),
            end_date: Some("2026-10-28T07:00:00.000Z".into()),
            ..Default::default()
        };
        assert_eq!(
            sprint_summary(Some(&[sprint])),
            "Analytics sprint 21 · active · ends 2026-10-28"
        );
        assert_eq!(sprint_summary(None), "Unavailable");
        assert_eq!(sprint_summary(Some(&[])), "None");
    }

    #[test]
    fn older_comment_pages_do_not_repeat_comments_at_page_boundaries() {
        let comment = |id: &str| twg::Comment {
            id: id.into(),
            ..Default::default()
        };
        let mut comments = vec![comment("newest"), comment("overlap")];
        append_comments(&mut comments, vec![comment("overlap"), comment("older")]);
        assert_eq!(
            comments
                .iter()
                .map(|comment| comment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["newest", "overlap", "older"]
        );
    }
}

impl Render for TicketView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if self.work_log.is_some() {
            self.render_work_log(window, cx)
        } else if self.edit.is_some() {
            self.render_edit(window, cx)
        } else if self.status.is_some() {
            self.render_status(window, cx)
        } else {
            self.render_ticket(window, cx)
        };
        v_flex()
            .id("ticket-view")
            .key_context("WorkPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background.opacity(1.0))
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.refresh(window, cx)))
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
            .on_action(
                cx.listener(|this, _: &SubmitComment, window, cx| this.submit_comment(window, cx)),
            )
            .when_some(self.notice.clone(), |this, notice| {
                this.child(div().p_3().child(Label::new(notice).color(Color::Success)))
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().p_3().child(Label::new(error).color(Color::Warning)))
            })
            .child(div().flex_1().min_h_0().child(content))
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

impl Focusable for TicketView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<TicketEvent> for TicketView {}
impl Item for TicketView {
    type Event = TicketEvent;
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        self.key.clone().into()
    }
    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Some(
            format!(
                "{}: {}\n{}",
                self.key,
                self.ticket
                    .as_ref()
                    .map(|ticket| ticket.summary.as_str())
                    .unwrap_or("Loading ticket…"),
                self.ticket_url(cx).unwrap_or_default()
            )
            .into(),
        )
    }
    fn to_item_events(event: &Self::Event, emit: &mut dyn FnMut(ItemEvent)) {
        if let TicketEvent::Item(event) = event {
            emit(*event);
        }
    }
    fn is_dirty(&self, cx: &App) -> bool {
        self.writing || self.has_unsaved_drafts(cx)
    }
    fn can_close(&self, _: &App) -> bool {
        !self.writing
    }
    fn can_save(&self, _: &App) -> bool {
        true
    }
    fn save(
        &mut self,
        _: SaveOptions,
        _: Entity<project::Project>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        if !self.is_dirty(cx) {
            return Task::ready(Ok(()));
        }
        Task::ready(Err(anyhow::anyhow!(
            "Ticket drafts are submitted only through the ticket's Save, Change status, Log work, or Comment actions. Cancel closing to keep editing, or choose Don't Save to discard the draft."
        )))
    }
}
