use std::collections::BTreeMap;

use gpui::PromptLevel;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use serde_json::{Map, Value, json};
use twg::{FieldMetadata, NamedId, Ticket, Transition, User};

use super::*;
use ticket_view::metadata_row;

pub(super) struct EditDraft {
    baseline: Ticket,
    summary: Entity<Editor>,
    description: Entity<Editor>,
    labels: Entity<Editor>,
    user_search: Entity<Editor>,
    assignee: Option<User>,
    priority: Option<NamedId>,
    metadata: Vec<FieldMetadata>,
    users: Vec<User>,
    preview: Entity<Markdown>,
    showing_preview: bool,
    description_editable: bool,
    metadata_loaded: bool,
}

impl EditDraft {
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn for_visual_fixture(
        ticket: &Ticket,
        window: &mut Window,
        cx: &mut Context<TicketView>,
    ) -> Self {
        let description = twg::rich_text::project(&ticket.description).markdown;
        let metadata = ["summary", "description", "labels", "assignee", "priority"]
            .into_iter()
            .map(|key| FieldMetadata {
                id: key.into(),
                key: key.into(),
                name: key.into(),
                operations: vec!["set".into()],
                allowed_values: if key == "priority" {
                    vec![
                        json!({"id":"1","name":"High"}),
                        json!({"id":"2","name":"Medium"}),
                    ]
                } else {
                    Vec::new()
                },
                ..Default::default()
            })
            .collect();
        Self {
            baseline: ticket.clone(),
            summary: make_editor(
                "Summary",
                "Keep duplicate events out of the analytics pipeline",
                1,
                window,
                cx,
            ),
            description: make_editor("Description in Markdown", &description, 8, window, cx),
            labels: make_editor("Comma-separated labels", "analytics, dedupe", 1, window, cx),
            user_search: make_editor("Search users", "", 1, window, cx),
            assignee: ticket.assignee.clone(),
            priority: ticket.priority.clone(),
            metadata,
            users: vec![User {
                account_id: "fixture-reviewer".into(),
                display_name: "Sam Taylor".into(),
            }],
            preview: cx.new(|cx| Markdown::new(description.into(), None, None, cx)),
            showing_preview: false,
            description_editable: true,
            metadata_loaded: true,
        }
    }

    pub(super) fn dirty_count(&self, cx: &App) -> usize {
        let (fields, description) = self.patch(cx);
        fields.len() + usize::from(description.is_some())
    }

    fn patch(&self, cx: &App) -> (Map<String, Value>, Option<String>) {
        let mut fields = Map::new();
        let summary = self.summary.read(cx).text(cx);
        if summary != self.baseline.summary {
            fields.insert("summary".into(), json!(summary));
        }
        let labels = labels(&self.labels.read(cx).text(cx));
        if labels != self.baseline.labels {
            fields.insert("labels".into(), json!(labels));
        }
        if account_id(&self.assignee) != account_id(&self.baseline.assignee) {
            fields.insert(
                "assignee".into(),
                self.assignee
                    .as_ref()
                    .map(|user| json!({"accountId":user.account_id}))
                    .unwrap_or(Value::Null),
            );
        }
        if self.priority.as_ref().map(|priority| &priority.id)
            != self.baseline.priority.as_ref().map(|priority| &priority.id)
        {
            fields.insert(
                "priority".into(),
                self.priority
                    .as_ref()
                    .map(|priority| json!({"id":priority.id}))
                    .unwrap_or(Value::Null),
            );
        }
        let description = self.description.read(cx).text(cx);
        let original = twg::rich_text::project(&self.baseline.description).markdown;
        (
            fields,
            (self.description_editable && description != original).then_some(description),
        )
    }

    fn allows(&self, key: &str) -> bool {
        self.metadata_loaded
            && self.metadata.iter().any(|field| {
                (field.key == key || field.id == key)
                    && field.operations.iter().any(|operation| operation == "set")
            })
    }
}

pub(super) struct StatusDraft {
    transitions: Vec<Transition>,
    selected: Option<usize>,
    values: Map<String, Value>,
    editors: BTreeMap<String, Entity<Editor>>,
    comment: Entity<Editor>,
    loading: bool,
}

impl StatusDraft {
    pub(super) fn is_dirty(&self, cx: &App) -> bool {
        self.selected.is_some()
            || !self.values.is_empty()
            || !self.comment.read(cx).text(cx).trim().is_empty()
            || self
                .editors
                .values()
                .any(|editor| !editor.read(cx).text(cx).trim().is_empty())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl StatusDraft {
    pub(super) fn for_visual_fixture(window: &mut Window, cx: &mut Context<TicketView>) -> Self {
        Self {
            transitions: vec![Transition {
                id: "fixture-done".into(),
                name: "Done".into(),
                to: Some(NamedId {
                    id: "done".into(),
                    name: "Done".into(),
                }),
                fields: vec![
                    FieldMetadata {
                        id: "resolution".into(),
                        key: "resolution".into(),
                        name: "Resolution".into(),
                        required: true,
                        operations: vec!["set".into()],
                        allowed_values: vec![
                            json!({"id":"fixture-fixed","name":"Fixed"}),
                            json!({"id":"fixture-duplicate","name":"Duplicate"}),
                        ],
                        ..Default::default()
                    },
                    FieldMetadata {
                        id: "comment".into(),
                        key: "comment".into(),
                        name: "Comment".into(),
                        schema: json!({"type":"string"}),
                        operations: vec!["set".into()],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            selected: Some(0),
            values: Map::new(),
            editors: BTreeMap::new(),
            comment: make_editor(
                "Add a transition comment",
                "Verified duplicate events are no longer ingested.",
                3,
                window,
                cx,
            ),
            loading: false,
        }
    }
}

impl TicketView {
    pub(super) fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        let Some(ticket) = self.ticket.clone() else {
            return;
        };
        let projection = twg::rich_text::project(&ticket.description);
        let summary = make_editor("Summary", &ticket.summary, 1, window, cx);
        let description = make_editor(
            "Description in Markdown",
            &projection.markdown,
            8,
            window,
            cx,
        );
        let label_editor = make_editor(
            "Comma-separated labels",
            &ticket.labels.join(", "),
            1,
            window,
            cx,
        );
        let user_search = make_editor("Search users", "", 1, window, cx);
        for editor in [&summary, &description, &label_editor] {
            self.subscriptions
                .push(cx.subscribe(editor, |_: &mut Self, _, _: &EditorEvent, cx| cx.notify()));
        }
        let preview = cx.new(|cx| Markdown::new(projection.markdown.into(), None, None, cx));
        description.update(cx, |editor, _| editor.set_read_only(!projection.editable));
        self.edit = Some(EditDraft {
            summary,
            description,
            labels: label_editor,
            user_search,
            assignee: ticket.assignee.clone(),
            priority: ticket.priority.clone(),
            baseline: ticket.clone(),
            metadata: Vec::new(),
            users: Vec::new(),
            preview,
            showing_preview: false,
            description_editable: projection.editable,
            metadata_loaded: false,
        });
        self.status = None;
        self.error = None;
        let generation = self.requests.next();
        self.fetch_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { client.update_metadata(&ticket.key).await })
                .await;
            this.update(cx, |this, cx| {
                if !this.requests.is_current(generation) {
                    return;
                }
                if !this.guard_connection(cx) {
                    return;
                }
                match result {
                    Ok(metadata) => {
                        if this.writing {
                            return;
                        }
                        if let Some(edit) = &mut this.edit {
                            edit.metadata = metadata;
                            edit.metadata_loaded = true;
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not load editable fields: {error:#}"))
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn search_assignees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else {
            return;
        };
        let query = edit.user_search.read(cx).text(cx);
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        let generation = self.requests.next();
        self.fetch_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { client.users(&query).await })
                .await;
            this.update(cx, |this, cx| {
                if !this.requests.is_current(generation) {
                    return;
                }
                if !this.guard_connection(cx) {
                    return;
                }
                match result {
                    Ok(users) => {
                        if this.writing {
                            return;
                        }
                        if let Some(edit) = &mut this.edit {
                            edit.users = users;
                        }
                    }
                    Err(error) => this.error = Some(format!("User search failed: {error:#}")),
                }
                cx.notify();
            })
            .log_err();
        }));
    }

    pub(super) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        let dirty = self
            .edit
            .as_ref()
            .is_some_and(|edit| edit.dirty_count(cx) > 0)
            || self
                .work_log
                .as_ref()
                .is_some_and(|draft| draft.is_dirty(cx))
            || self
                .status
                .as_ref()
                .is_some_and(|status| status.is_dirty(cx));
        if dirty {
            let answer = window.prompt(
                PromptLevel::Warning,
                "Discard unsaved ticket changes?",
                None,
                &["Discard", "Keep editing"],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                if answer.await == Ok(0) {
                    this.update(cx, |this, cx| {
                        this.edit = None;
                        this.status = None;
                        this.work_log = None;
                        this.error = None;
                        this.requests.next();
                        cx.notify();
                    })
                    .log_err();
                }
            })
            .detach();
        } else {
            self.edit = None;
            self.status = None;
            self.work_log = None;
            self.error = None;
            self.requests.next();
            cx.notify();
        }
    }

    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        if self.work_log.is_some() {
            self.submit_work_log(window, cx);
            return;
        }
        if self.status.is_some() {
            self.save_status(window, cx);
            return;
        }
        let Some(edit) = &self.edit else {
            return;
        };
        let (fields, description) = edit.patch(cx);
        if fields.is_empty() && description.is_none() {
            return;
        }
        if !edit.metadata_loaded {
            self.error = Some("Wait for editable field metadata before saving.".into());
            cx.notify();
            return;
        }
        if let Some(key) = fields.keys().find(|key| !edit.allows(key)) {
            self.error = Some(format!("Jira does not allow editing {key} on this ticket."));
            cx.notify();
            return;
        }
        if description.is_some() && !edit.allows("description") {
            self.error = Some("Jira does not allow editing the description on this ticket.".into());
            cx.notify();
            return;
        }
        if edit.summary.read(cx).text(cx).trim().is_empty() {
            self.error = Some("Summary is required.".into());
            cx.notify();
            return;
        }
        let baseline = edit.baseline.clone();
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        self.requests.next();
        self.comment_requests.next();
        self.comments_loading = false;
        self.loading = false;
        self.writing = true;
        self.error = None;
        self.notice = None;
        self.write_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let remote = client.get_ticket(&baseline.key).await?;
                    check_stale(&baseline, &remote, &fields, description.is_some())?;
                    client
                        .update_ticket(&baseline.key, fields, description)
                        .await
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.writing = false;
                match result {
                    Ok(result) => {
                        this.edit = None;
                        this.finish_write(result, "Changes saved to Jira.", window, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Not saved; changes retained: {error:#}"))
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    pub(super) fn begin_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        if !self.allow_navigation(cx) {
            return;
        }
        let Some(ticket) = &self.ticket else {
            return;
        };
        let key = ticket.key.clone();
        self.status = Some(StatusDraft {
            transitions: Vec::new(),
            selected: None,
            values: Map::new(),
            editors: BTreeMap::new(),
            comment: make_editor("Add a transition comment", "", 3, window, cx),
            loading: true,
        });
        let generation = self.requests.next();
        self.fetch_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { client.transitions(&key).await })
                .await;
            this.update(cx, |this, cx| {
                if !this.requests.is_current(generation) {
                    return;
                }
                if !this.guard_connection(cx) {
                    return;
                }
                if let Some(status) = &mut this.status {
                    status.loading = false;
                    match result {
                        Ok(transitions) => status.transitions = transitions,
                        Err(error) => {
                            this.error = Some(format!("Could not load transitions: {error:#}"))
                        }
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn select_transition(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        let Some(status) = &self.status else {
            return;
        };
        if status.selected == Some(index) {
            return;
        }
        if !status.values.is_empty()
            || status
                .editors
                .values()
                .any(|editor| !editor.read(cx).text(cx).is_empty())
        {
            let answer = window.prompt(
                PromptLevel::Warning,
                "Discard values for this transition?",
                None,
                &["Discard and switch", "Keep editing"],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                if answer.await == Ok(0) {
                    this.update_in(cx, |this, window, cx| {
                        this.apply_transition(index, window, cx)
                    })
                    .log_err();
                }
            })
            .detach();
        } else {
            self.apply_transition(index, window, cx);
        }
    }

    fn apply_transition(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = &mut self.status else {
            return;
        };
        if self.writing {
            return;
        }
        let Some(transition) = status.transitions.get(index) else {
            return;
        };
        status.selected = Some(index);
        status.values.clear();
        status.editors.clear();
        for field in transition.fields.iter().filter(|field| !is_comment(field)) {
            if field.allowed_values.is_empty() && supported_text(field) {
                let editor = make_editor(&field.name, "", 1, window, cx);
                self.subscriptions.push(
                    cx.subscribe(&editor, |_: &mut Self, _, _: &EditorEvent, cx| cx.notify()),
                );
                status.editors.insert(field.key.clone(), editor);
            }
        }
        self.error = None;
        cx.notify();
    }

    fn save_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = &self.status else {
            return;
        };
        let Some(transition) = status
            .selected
            .and_then(|index| status.transitions.get(index))
        else {
            return;
        };
        let mut fields = status.values.clone();
        for (key, editor) in &status.editors {
            let text = editor.read(cx).text(cx);
            if !text.trim().is_empty() {
                fields.insert(key.clone(), Value::String(text));
            }
        }
        let comment = status.comment.read(cx).text(cx);
        if let Err(error) = validate_transition(transition, &fields, Some(&comment)) {
            self.error = Some(error.to_string());
            cx.notify();
            return;
        }
        let Some(ticket) = &self.ticket else {
            return;
        };
        let key = ticket.key.clone();
        let original_status = ticket.status.id.clone();
        let transition_id = transition.id.clone();
        let Some(client) = self.bound_client(cx) else {
            return;
        };
        self.requests.next();
        self.comment_requests.next();
        self.comments_loading = false;
        self.loading = false;
        self.writing = true;
        self.error = None;
        self.notice = None;
        self.write_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move {
                let remote = client.get_ticket(&key).await?;
                anyhow::ensure!(remote.status.id == original_status, "The ticket status changed remotely. Cancel and reload before changing status.");
                let live = client.transitions(&key).await?;
                let transition = live.iter().find(|transition| transition.id == transition_id)
                    .ok_or_else(|| anyhow::anyhow!("This transition is no longer available. Cancel and reload."))?;
                validate_transition(transition, &fields, Some(&comment))?;
                client.transition(&key, &transition_id, fields, (!comment.trim().is_empty()).then_some(comment.as_str())).await
            }).await;
            this.update_in(cx, |this, window, cx| {
                this.writing = false;
                match result {
                    Ok(result) => {
                        this.status = None;
                        this.finish_write(result, "Status changed in Jira.", window, cx);
                    }
                    Err(error) => this.error = Some(format!("Status not changed; values retained: {error:#}")),
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    pub(super) fn render_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(edit) = &self.edit else {
            return div().into_any_element();
        };
        let dirty = edit.dirty_count(cx);
        let assignee_label = edit
            .assignee
            .as_ref()
            .map(|user| user.display_name.clone())
            .unwrap_or_else(|| "Unassigned".into());
        let priority_label = edit
            .priority
            .as_ref()
            .map(|priority| priority.name.clone())
            .unwrap_or_else(|| "None".into());
        let mut users = edit.users.clone();
        if let Some(field) = edit.metadata.iter().find(|field| field.key == "assignee") {
            users.extend(field.allowed_values.iter().filter_map(|value| {
                Some(User {
                    account_id: value.get("accountId")?.as_str()?.into(),
                    display_name: value.get("displayName")?.as_str()?.into(),
                })
            }));
        }
        let priority_values = edit
            .metadata
            .iter()
            .find(|field| field.key == "priority")
            .map(|field| field.allowed_values.clone())
            .unwrap_or_default();
        let panel = cx.entity().downgrade();
        let assignee_menu = ContextMenu::build(window, cx, |menu, _, _| {
            let unassigned_panel = panel.clone();
            let mut menu = menu.entry("Unassigned", None, move |_, cx| {
                unassigned_panel
                    .update(cx, |this, cx| {
                        if this.writing {
                            return;
                        }
                        if let Some(edit) = &mut this.edit {
                            edit.assignee = None;
                        }
                        cx.notify();
                    })
                    .log_err();
            });
            for user in users {
                let panel = panel.clone();
                menu = menu.entry(user.display_name.clone(), None, move |_, cx| {
                    panel
                        .update(cx, |this, cx| {
                            if this.writing {
                                return;
                            }
                            if let Some(edit) = &mut this.edit {
                                edit.assignee = Some(user.clone());
                            }
                            cx.notify();
                        })
                        .log_err();
                });
            }
            menu
        });
        let priority_menu = ContextMenu::build(window, cx, |mut menu, _, _| {
            for value in priority_values {
                if let (Some(id), Some(name)) = (
                    value.get("id").and_then(Value::as_str),
                    value.get("name").and_then(Value::as_str),
                ) {
                    let priority = NamedId {
                        id: id.to_string(),
                        name: name.to_string(),
                    };
                    let panel = panel.clone();
                    menu = menu.entry(priority.name.clone(), None, move |_, cx| {
                        panel
                            .update(cx, |this, cx| {
                                if this.writing {
                                    return;
                                }
                                if let Some(edit) = &mut this.edit {
                                    edit.priority = Some(priority.clone());
                                }
                                cx.notify();
                            })
                            .log_err();
                    });
                }
            }
            menu
        });
        let Some(edit) = &self.edit else {
            return div().into_any_element();
        };
        v_flex().size_full()
            .child(h_flex().p_3().justify_between().border_b_1().border_color(cx.theme().colors().border)
                .child(Label::new(format!("Editing {}", edit.baseline.key)))
                .child(Label::new(format!("{dirty} unsaved changes")).color(if dirty > 0 { Color::Warning } else { Color::Muted })))
            .child(v_flex().id("ticket-edit").flex_1().overflow_y_scroll().gap_4().p_3()
                .when(!edit.metadata_loaded, |this| this.child(Label::new("Loading editable fields…").color(Color::Muted)))
                .child(editor_field("Summary *", edit.summary.clone(), !self.writing && edit.allows("summary"), cx))
                .child(h_flex().gap_2()
                    .child(v_flex().flex_1().gap_1().child(Label::new("Assignee").color(Color::Muted))
                        .child(DropdownMenu::new("assignee", assignee_label, assignee_menu).tab_index(0_isize).style(DropdownStyle::Outlined).disabled(self.writing || !edit.allows("assignee"))))
                    .child(v_flex().flex_1().gap_1().child(Label::new("Priority").color(Color::Muted))
                        .child(DropdownMenu::new("priority", priority_label, priority_menu).tab_index(0_isize).style(DropdownStyle::Outlined).disabled(self.writing || !edit.allows("priority")))))
                .when(edit.allows("assignee"), |this| this.child(h_flex().gap_2()
                    .child(div().flex_1().child(editor_field("", edit.user_search.clone(), true, cx)))
                    .child(Button::new("search-users", "Find users").tab_index(0_isize).disabled(self.writing).on_click(cx.listener(|this, _, window, cx| this.search_assignees(window, cx))))))
                .child(editor_field("Labels (comma separated)", edit.labels.clone(), !self.writing && edit.allows("labels"), cx))
                .child(h_flex().justify_between().child(Label::new("Description").color(Color::Muted))
                    .child(h_flex().gap_1()
                        .child(Button::new("write-desc", "Write").tab_index(0_isize).style(if !edit.showing_preview { ButtonStyle::Tinted(ui::TintColor::Accent) } else { ButtonStyle::Subtle }).on_click(cx.listener(|this, _, _, cx| {
                            if this.writing { return; }
                    if let Some(edit) = &mut this.edit { edit.showing_preview = false; } cx.notify();
                        })))
                        .child(Button::new("preview-desc", "Preview").tab_index(0_isize).style(if edit.showing_preview { ButtonStyle::Tinted(ui::TintColor::Accent) } else { ButtonStyle::Subtle }).on_click(cx.listener(|this, _, _, cx| {
                            if this.writing { return; }
                    if let Some(edit) = &mut this.edit {
                                let text = edit.description.read(cx).text(cx);
                                edit.preview.update(cx, |preview, cx| preview.replace(text, cx));
                                edit.showing_preview = true;
                            } cx.notify();
                        })))))
                .when(!edit.description_editable, |this| this
                    .child(Label::new("This description contains rich Jira content. Edit it in Jira to preserve formatting. Other edits preserve it.").color(Color::Muted)))
                .child(if edit.showing_preview { div().child(MarkdownElement::new(edit.preview.clone(), MarkdownStyle::themed(MarkdownFont::Agent, window, cx))).into_any_element() }
                    else { editor_field("", edit.description.clone(), !self.writing && edit.allows("description") && edit.description_editable, cx) })
                .child(Label::new("Status changes use Change status because Jira may require additional fields.").color(Color::Muted)))
            .child(h_flex().p_3().gap_2().justify_end().border_t_1().border_color(cx.theme().colors().border)
                .child(Button::new("cancel-edit", "Cancel").tab_index(0_isize).disabled(self.writing).on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))))
                .child(Button::new("save-ticket", if self.writing { "Saving…" } else { "Save to Jira ⌘S" }).tab_index(0_isize).disabled(self.writing || dirty == 0 || !edit.metadata_loaded)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)))))
            .into_any_element()
    }

    pub(super) fn render_status(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(status) = &self.status else {
            return div().into_any_element();
        };
        let transition = status
            .selected
            .and_then(|index| status.transitions.get(index))
            .cloned();
        let mut fields = Vec::new();
        if let Some(transition) = &transition {
            for field in transition.fields.iter().filter(|field| !is_comment(field)) {
                let label = format!("{}{}", field.name, if field.required { " *" } else { "" });
                if !field.allowed_values.is_empty() {
                    let selected = status
                        .values
                        .get(&field.key)
                        .map(value_label)
                        .unwrap_or_else(|| "Choose…".into());
                    let key = field.key.clone();
                    let options = field.allowed_values.clone();
                    let array = field.schema.get("type").and_then(Value::as_str) == Some("array");
                    let panel = cx.entity().downgrade();
                    let menu = ContextMenu::build(window, cx, |mut menu, _, _| {
                        for option in options {
                            let key = key.clone();
                            let panel = panel.clone();
                            menu = menu.entry(value_label(&option), None, move |_, cx| {
                                panel
                                    .update(cx, |this, cx| {
                                        if this.writing {
                                            return;
                                        }
                                        if let Some(status) = &mut this.status {
                                            status.values.insert(
                                                key.clone(),
                                                if array {
                                                    json!([option.clone()])
                                                } else {
                                                    option.clone()
                                                },
                                            );
                                        }
                                        cx.notify();
                                    })
                                    .log_err();
                            });
                        }
                        menu
                    });
                    fields.push(
                        v_flex()
                            .gap_1()
                            .child(Label::new(label))
                            .child(
                                DropdownMenu::new(
                                    SharedString::from(format!("transition-{}", field.key)),
                                    selected,
                                    menu,
                                )
                                .tab_index(0_isize)
                                .style(DropdownStyle::Outlined)
                                .disabled(self.writing),
                            )
                            .into_any_element(),
                    );
                } else if let Some(editor) = status.editors.get(&field.key) {
                    fields.push(editor_field(&label, editor.clone(), !self.writing, cx));
                } else {
                    fields.push(
                        Label::new(format!("{label}: complete this field in Jira."))
                            .color(Color::Warning)
                            .into_any_element(),
                    );
                }
            }
        }
        let Some(status) = &self.status else {
            return div().into_any_element();
        };
        let blocked = transition.as_ref().is_some_and(|transition| {
            transition.fields.iter().any(|field| {
                field.required
                    && !is_comment(field)
                    && field.allowed_values.is_empty()
                    && !supported_text(field)
            })
        });
        v_flex().size_full()
            .child(h_flex().p_3().gap_2().border_b_1().border_color(cx.theme().colors().border)
                .child(Button::new("status-back", "‹").tab_index(0_isize).disabled(self.writing).on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))))
                .child(Label::new("Change status")))
            .child(v_flex().id("ticket-status").flex_1().overflow_y_scroll().p_3().gap_4()
                .child(metadata_row("Now", self.ticket.as_ref().map(|ticket| ticket.status.name.as_str()).unwrap_or("")))
                .child(Label::new("AVAILABLE TRANSITIONS").color(Color::Muted).size(LabelSize::Small))
                .when(status.loading, |this| this.child(Label::new("Loading workflow…").color(Color::Muted)))
                .when(!status.loading && status.transitions.is_empty(), |this| this.child(Label::new("No available transitions.").color(Color::Muted)))
                .children(status.transitions.iter().enumerate().map(|(index, transition)| {
                    Button::new(SharedString::from(format!("transition-{}", transition.id)), format!("{} → {}", transition.name,
                        transition.to.as_ref().map(|to| to.name.as_str()).unwrap_or(""))).tab_index(0_isize)
                        .style(if status.selected == Some(index) { ButtonStyle::Tinted(ui::TintColor::Accent) } else { ButtonStyle::Subtle }).disabled(self.writing)
                        .on_click(cx.listener(move |this, _, window, cx| this.select_transition(index, window, cx)))
                }))
                .children(fields)
                .child(editor_field(if transition.as_ref().is_some_and(|transition| transition.fields.iter().any(|field| is_comment(field) && field.required)) { "Comment *" } else { "Comment (optional)" }, status.comment.clone(), !self.writing, cx))
                .when(blocked, |this| this.child(Button::new("jira-status", "Open in Jira to complete required fields").tab_index(0_isize)
                    .on_click(cx.listener(|this, _, _, cx| { if let Some(url) = this.ticket_url(cx) { cx.open_url(&url); } }))))
                .child(Label::new("Transitions and required fields come from this ticket’s live Jira workflow.").color(Color::Muted)))
            .child(h_flex().p_3().gap_2().justify_end().border_t_1().border_color(cx.theme().colors().border)
                .child(Button::new("cancel-status", "Cancel").tab_index(0_isize).disabled(self.writing).on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))))
                .child(Button::new("apply-status", if self.writing { "Changing…".to_string() } else { transition.as_ref().map(|transition| transition.name.clone()).unwrap_or_else(|| "Change status".into()) }).tab_index(0_isize)
                    .disabled(self.writing || transition.is_none() || blocked).on_click(cx.listener(|this, _, window, cx| this.save_status(window, cx)))))
            .into_any_element()
    }
}

fn labels(text: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for label in text
        .split(',')
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        if !labels.iter().any(|existing| existing == label) {
            labels.push(label.to_string());
        }
    }
    labels
}

fn account_id(user: &Option<User>) -> Option<&str> {
    user.as_ref().map(|user| user.account_id.as_str())
}

pub(super) fn editor_field(
    label: &str,
    editor: Entity<Editor>,
    enabled: bool,
    cx: &mut App,
) -> AnyElement {
    editor.update(cx, |editor, _| editor.set_read_only(!enabled));
    let focus = editor.focus_handle(cx).tab_index(0).tab_stop(enabled);
    v_flex()
        .gap_1()
        .when(!label.is_empty(), |this| {
            this.child(Label::new(label.to_string()).color(Color::Muted))
        })
        .child(
            div()
                .p_2()
                .border_1()
                .border_color(cx.theme().colors().border)
                .bg(cx.theme().colors().editor_background)
                .rounded_md()
                .track_focus(&focus)
                .focus(|style| style.border_color(cx.theme().colors().border_focused))
                .when(!enabled, |this| this.opacity(0.6))
                .child(editor),
        )
        .into_any_element()
}

fn check_stale(
    original: &Ticket,
    remote: &Ticket,
    fields: &Map<String, Value>,
    description: bool,
) -> anyhow::Result<()> {
    let changed = |key: &str| fields.contains_key(key);
    let conflicts = state::field_conflicts(changed("summary"), &original.summary, &remote.summary)
        || state::field_conflicts(changed("labels"), &original.labels, &remote.labels)
        || state::field_conflicts(
            changed("assignee"),
            &account_id(&original.assignee),
            &account_id(&remote.assignee),
        )
        || state::field_conflicts(
            changed("priority"),
            &original.priority.as_ref().map(|priority| &priority.id),
            &remote.priority.as_ref().map(|priority| &priority.id),
        )
        || state::field_conflicts(description, &original.description, &remote.description);
    anyhow::ensure!(
        !conflicts,
        "A field you edited changed remotely. Your draft is retained. Cancel and reload before saving."
    );
    Ok(())
}

fn supported_text(field: &FieldMetadata) -> bool {
    field.schema.get("type").and_then(Value::as_str) == Some("string")
        && !matches!(
            field.schema.get("system").and_then(Value::as_str),
            Some("user" | "option")
        )
}

fn value_label(value: &Value) -> String {
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .map(value_label)
            .collect::<Vec<_>>()
            .join(", ");
    }
    value
        .get("name")
        .or_else(|| value.get("displayName"))
        .or_else(|| value.get("value"))
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .unwrap_or("Option")
        .to_string()
}

fn is_comment(field: &FieldMetadata) -> bool {
    field.key == "comment"
        || field.id == "comment"
        || field.schema.get("system").and_then(Value::as_str) == Some("comment")
}

fn validate_transition(
    transition: &Transition,
    values: &Map<String, Value>,
    comment: Option<&str>,
) -> anyhow::Result<()> {
    for field in &transition.fields {
        if is_comment(field) {
            anyhow::ensure!(
                !field.required || comment.is_some_and(|comment| !comment.trim().is_empty()),
                "{} is required.",
                field.name
            );
            continue;
        }
        let value = values.get(&field.key);
        if field.required {
            anyhow::ensure!(
                !field.allowed_values.is_empty() || supported_text(field),
                "Complete required field {} in Jira.",
                field.name
            );
            anyhow::ensure!(
                value.is_some_and(|value| !value.is_null()
                    && value.as_str().is_none_or(|text| !text.trim().is_empty())
                    && value.as_array().is_none_or(|items| !items.is_empty())),
                "{} is required.",
                field.name
            );
        }
        if let Some(value) = value {
            if !field.allowed_values.is_empty() {
                let selected = value
                    .as_array()
                    .map(|items| items.as_slice())
                    .unwrap_or_else(|| std::slice::from_ref(value));
                anyhow::ensure!(
                    selected
                        .iter()
                        .all(|value| field.allowed_values.iter().any(|allowed| allowed == value)),
                    "{} is no longer an available option. Reload the workflow.",
                    field.name
                );
            } else {
                anyhow::ensure!(
                    supported_text(field) && value.is_string(),
                    "Complete {} in Jira.",
                    field.name
                );
            }
        }
    }
    anyhow::ensure!(
        values.keys().all(|key| transition
            .fields
            .iter()
            .any(|field| &field.key == key && !is_comment(field))),
        "The workflow fields changed. Reload before changing status."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_transition_comment_uses_separate_input_even_for_add_operation() {
        let transition = Transition {
            fields: vec![FieldMetadata {
                key: "comment".into(),
                name: "Comment".into(),
                required: true,
                schema: json!({"type":"string", "system":"comment"}),
                operations: vec!["add".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate_transition(&transition, &Map::new(), None).is_err());
        assert!(validate_transition(&transition, &Map::new(), Some(" ")).is_err());
        assert!(validate_transition(&transition, &Map::new(), Some("Verified")).is_ok());
        assert!(
            validate_transition(
                &transition,
                &Map::from_iter([("comment".into(), json!("Wrong channel"))]),
                Some("Verified")
            )
            .is_err()
        );
    }

    #[test]
    fn unrelated_remote_changes_do_not_reject_summary_patch() {
        let original = Ticket {
            summary: "old".into(),
            ..Ticket::default()
        };
        let remote = Ticket {
            labels: vec!["new remote label".into()],
            ..original.clone()
        };
        let fields = Map::from_iter([("summary".into(), json!("new"))]);
        assert!(check_stale(&original, &remote, &fields, false).is_ok());
        let remote = Ticket {
            summary: "remote summary".into(),
            ..remote
        };
        assert!(check_stale(&original, &remote, &fields, false).is_err());
    }

    #[test]
    fn transition_requires_live_option_without_guessing_ids() {
        let transition = Transition {
            fields: vec![FieldMetadata {
                key: "resolution".into(),
                name: "Resolution".into(),
                required: true,
                allowed_values: vec![json!({"id":"10000","name":"Fixed"})],
                ..FieldMetadata::default()
            }],
            ..Transition::default()
        };
        assert!(validate_transition(&transition, &Map::new(), None).is_err());
        assert!(
            validate_transition(
                &transition,
                &Map::from_iter([("resolution".into(), json!({"id":"1"}))]),
                None
            )
            .is_err()
        );
        assert!(
            validate_transition(
                &transition,
                &Map::from_iter([("resolution".into(), json!({"id":"10000","name":"Fixed"}))]),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn unsupported_required_transition_fields_use_jira_fallback() {
        let transition = Transition {
            fields: vec![FieldMetadata {
                key: "customfield_1".into(),
                required: true,
                schema: json!({"type":"object"}),
                ..FieldMetadata::default()
            }],
            ..Transition::default()
        };
        assert!(validate_transition(&transition, &Map::new(), None).is_err());
    }

    #[test]
    fn labels_preserve_order_and_remove_empty_duplicates() {
        assert_eq!(labels("one, two, one, ,"), vec!["one", "two"]);
    }

    #[test]
    fn selected_array_options_show_their_labels() {
        assert_eq!(value_label(&json!([{"id":"1", "name":"Done"}])), "Done");
    }
}
