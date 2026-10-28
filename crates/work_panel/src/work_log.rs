use super::*;
use ticket_editor::editor_field;

pub(super) struct WorkLogDraft {
    time_spent: Entity<Editor>,
    started: Entity<Editor>,
    comment: Entity<Editor>,
}

impl WorkLogDraft {
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn for_visual_fixture(window: &mut Window, cx: &mut Context<TicketView>) -> Self {
        Self {
            time_spent: make_editor("For example, 1h 30m", "1h 30m", 1, window, cx),
            started: make_editor("Optional: YYYY-MM-DDTHH:MM:SS+HHMM", "", 1, window, cx),
            comment: make_editor(
                "What did you work on?",
                "Verified the deduplication fix and added regression coverage.",
                3,
                window,
                cx,
            ),
        }
    }

    pub(super) fn is_dirty(&self, cx: &App) -> bool {
        [&self.time_spent, &self.started, &self.comment]
            .iter()
            .any(|editor| !editor.read(cx).text(cx).trim().is_empty())
    }
}

impl TicketView {
    pub(super) fn begin_work_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.writing || self.ticket.is_none() {
            return;
        }
        if !self.guard_connection(cx) {
            return;
        }
        let time_spent = make_editor("For example, 1h 30m", "", 1, window, cx);
        let started = make_editor("Optional: YYYY-MM-DDTHH:MM:SS+HHMM", "", 1, window, cx);
        let comment = make_editor("What did you work on?", "", 3, window, cx);
        for editor in [&time_spent, &started, &comment] {
            self.subscriptions
                .push(cx.subscribe(editor, |_: &mut Self, _, _: &EditorEvent, cx| cx.notify()));
        }
        self.work_log = Some(WorkLogDraft {
            time_spent,
            started,
            comment,
        });
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    pub(super) fn submit_work_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        let (Some(draft), Some(ticket)) = (&self.work_log, &self.ticket) else {
            return;
        };
        let time_spent = draft.time_spent.read(cx).text(cx).trim().to_string();
        if time_spent.is_empty() {
            self.error = Some("Enter the time spent before logging work.".into());
            cx.notify();
            return;
        }
        let started = draft.started.read(cx).text(cx).trim().to_string();
        let comment = draft.comment.read(cx).text(cx).trim().to_string();
        let key = ticket.key.clone();
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        self.requests.next();
        self.comment_requests.next();
        self.comments_loading = false;
        self.writing = true;
        self.error = None;
        self.notice = None;
        self.write_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    client
                        .log_work(
                            &key,
                            &time_spent,
                            (!started.is_empty()).then_some(started.as_str()),
                            (!comment.is_empty()).then_some(comment.as_str()),
                        )
                        .await
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.writing = false;
                match result {
                    Ok(result) => {
                        this.work_log = None;
                        this.finish_write(result, "Work logged in Jira.", window, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Work not logged; draft retained: {error:#}"))
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    pub(super) fn render_work_log(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(draft) = &self.work_log else {
            return div().into_any_element();
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new(format!(
                        "Log work · {}",
                        self.ticket
                            .as_ref()
                            .map(|ticket| ticket.key.as_str())
                            .unwrap_or("")
                    ))),
            )
            .child(
                v_flex()
                    .id("log-work-form")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_4()
                    .child(editor_field(
                        "Time spent *",
                        draft.time_spent.clone(),
                        !self.writing,
                        cx,
                    ))
                    .child(editor_field(
                        "Started (optional)",
                        draft.started.clone(),
                        !self.writing,
                        cx,
                    ))
                    .child(
                        Label::new("Leave Started blank to use the current time.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(editor_field(
                        "Work description (optional)",
                        draft.comment.clone(),
                        !self.writing,
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .justify_end()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Button::new("cancel-log-work", "Cancel")
                            .tab_index(0_isize)
                            .disabled(self.writing)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        Button::new(
                            "submit-log-work",
                            if self.writing {
                                "Logging…"
                            } else {
                                "Log work to Jira"
                            },
                        )
                        .tab_index(0_isize)
                        .disabled(
                            self.writing || draft.time_spent.read(cx).text(cx).trim().is_empty(),
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.submit_work_log(window, cx)),
                        ),
                    ),
            )
            .into_any_element()
    }
}
