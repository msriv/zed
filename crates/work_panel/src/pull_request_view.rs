use std::collections::{HashMap, HashSet};

use agent_ui::{AgentPanel, ExternalSourcePrompt};
use editor::{Editor, EditorEvent};
use git::repository::{AskPassDelegate, FetchOptions, Remote};
use git_ui_core::askpass_modal::AskPassModal;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Task, WeakEntity,
    Window, prelude::*,
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use settings::Settings;
use twg::{
    TwgSettings,
    bitbucket::{
        DiffAnchor, DiffFile, DiffLine, DiffLineKind, DiffSide, PrWriteResult, PullRequest,
        PullRequestDiff, PullRequestRef, RepositoryRef, ReviewAction, ReviewComment,
    },
};
use ui::{TintColor, Tooltip, prelude::*};
use util::ResultExt;
use workspace::{
    Workspace,
    item::{Item, ItemEvent, SaveOptions},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Overview,
    Files,
    Comments,
    Commits,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InlineCommentContext {
    revision: String,
    anchor: DiffAnchor,
}

struct InlineCommentDraft {
    context: InlineCommentContext,
    editor: Entity<Editor>,
}

#[derive(Clone)]
enum CommentTarget {
    General,
    Reply(u64),
    Inline(InlineCommentContext),
}

enum Mutation {
    Approve,
    RequestChanges,
    Comment { target: CommentTarget, body: String },
    Resolve(u64),
}

pub(super) fn open_pull_request(
    workspace: &WeakEntity<Workspace>,
    reference: PullRequestRef,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| PullRequestView::new(workspace.clone(), reference, window, cx));
    workspace
        .update(cx, |workspace, cx| {
            workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx)
        })
        .log_err();
}

pub(super) struct PullRequestView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    reference: PullRequestRef,
    snapshot: Option<PullRequest>,
    description: Option<Entity<Markdown>>,
    diff: Option<PullRequestDiff>,
    displayed_revision: Option<String>,
    tab: Tab,
    selected_file: usize,
    viewed: HashSet<String>,
    comment: Option<Entity<Editor>>,
    inline_comment: Option<InlineCommentDraft>,
    replies: HashMap<u64, Entity<Editor>>,
    comment_subscriptions: Vec<Subscription>,
    loading: bool,
    writing: bool,
    error: Option<String>,
    task: Option<Task<()>>,
}

impl PullRequestView {
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn for_visual_fixture(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let reference = PullRequestRef {
            workspace: "example".into(),
            repository: "review-demo".into(),
            number: 42,
        };
        let review_path = "crates/platform/integrations/bitbucket/pull_request_review/pending_drafts/src/review.rs";
        let snapshot = twg::bitbucket::parse_pull_request(
            serde_json::json!({
                "id":42,"title":"Keep pending review drafts when a pull request changes",
                "description":"### Summary\n\nPreserve **unsent replies** when a pull request changes. The displayed `revision` stays bound to its diff.\n\n### Jira\n\n[DAT-1206](https://jira.example.invalid/browse/DAT-1206)\n\n### What changed\n\n- Keep pending review drafts.\n- Refresh metadata before submitting a review.\n- Preserve the original file and line context.\n\n### Testing\n\n1. Open a pull request and type a reply.\n2. Refresh the pull request.\n3. Confirm the draft remains available.\n\n```rust\nfn finish_review() {\n    keep_pending_drafts();\n}\n```\n\n### Notes for reviewers\n\n> A refresh must never discard an unsent reply.",
                "state":"OPEN",
                "author":{"display_name":"Demo author"},
                "source":{"branch":{"name":"feature/review-drafts"},"commit":{"hash":"abc123"},"repository":{"full_name":"example/review-demo"}},
                "destination":{"branch":{"name":"main"},"commit":{"hash":"base123"}},
                "_comments":[{"id":7,"user":{"display_name":"Demo reviewer"},"content":{"raw":"Please preserve this draft until the user submits it."},"inline":{"path":review_path,"to":2},"resolved":false}],
                "_statuses":[{"name":"Unit tests","state":"SUCCESSFUL"}],
                "_commits":[
                    {"hash":"abc123","message":"Preserve review drafts","links":{"html":{"href":"https://bitbucket.org/example/review-demo/commits/abc123"}}},
                    {"hash":"def456","message":"Add regression coverage\n\nKeep unsent comments through a metadata refresh."}
                ]
            }),
            reference.clone(),
        )?;
        let long_line = "preserve_pending_review_drafts_for_the_displayed_revision ".repeat(8);
        let mut patch = format!(
            "diff --git a/{review_path} b/{review_path}\n--- a/{review_path}\n+++ b/{review_path}\n@@ -1,2 +1,4 @@\n fn finish_review() {{\n-    drafts.clear();\n+    keep_pending_drafts();\n+    // {long_line}\n+}}\n"
        );
        for path in [
            "crates/platform/integrations/bitbucket/pull_request_review/pending_drafts/tests/review_draft_retention.rs",
            "crates/platform/integrations/bitbucket/pull_request_review/client/authentication_configuration.rs",
            "shared/sql/user_engagement_fact.sql",
            "shared/README.md",
        ] {
            patch.push_str(&format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old value\n+new value\n"
            ));
        }
        let diff = PullRequestDiff::parse(&patch)?;
        let mut view = Self {
            workspace,
            focus: cx.focus_handle(),
            reference,
            snapshot: None,
            description: None,
            diff: None,
            displayed_revision: None,
            tab: Tab::Files,
            selected_file: 0,
            viewed: HashSet::new(),
            comment: None,
            inline_comment: None,
            replies: HashMap::new(),
            comment_subscriptions: Vec::new(),
            loading: false,
            writing: false,
            error: None,
            task: None,
        };
        view.accept_snapshot(snapshot, window, cx);
        view.displayed_revision = view.snapshot.as_ref().map(PullRequest::revision);
        view.diff = Some(diff);
        Ok(view)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn for_visual_fixture_tab(
        workspace: WeakEntity<Workspace>,
        tab: Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let mut view = Self::for_visual_fixture(workspace, window, cx)?;
        view.tab = tab;
        if tab == Tab::Comments {
            let draft = "I checked the revised query. Please keep the regression coverage for missing roles.";
            if let Some(comment) = &view.comment {
                comment.update(cx, |comment, cx| comment.set_text(draft, window, cx));
            }
            let snapshot = view
                .snapshot
                .clone()
                .ok_or_else(|| anyhow::anyhow!("The PR fixture must have a snapshot"))?;
            view.accept_snapshot(snapshot, window, cx);
            anyhow::ensure!(
                view.comment
                    .as_ref()
                    .is_some_and(|comment| comment.read(cx).text(cx) == draft)
                    && view.is_dirty(cx),
                "Unsent PR comments must survive refresh and mark the item dirty"
            );
        }
        Ok(view)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn for_visual_fixture_inline(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let mut view = Self::for_visual_fixture(workspace, window, cx)?;
        let context = view
            .diff
            .as_ref()
            .and_then(|diff| diff.files.first())
            .and_then(|file| {
                file.hunks
                    .first()
                    .and_then(|hunk| {
                        hunk.lines
                            .iter()
                            .find(|line| line.kind == DiffLineKind::Added)
                    })
                    .and_then(|line| line_comment_anchor(file, line))
            })
            .zip(view.displayed_revision.clone())
            .map(|(anchor, revision)| InlineCommentContext { revision, anchor })
            .ok_or_else(|| anyhow::anyhow!("The inline fixture must have a current added line"))?;
        view.start_inline_comment(context.clone(), window, cx);
        let draft_text = "Could we also cover a missing role in this regression test?";
        if let Some(draft) = &view.inline_comment {
            draft
                .editor
                .update(cx, |editor, cx| editor.set_text(draft_text, window, cx));
        }
        let snapshot = view
            .snapshot
            .clone()
            .ok_or_else(|| anyhow::anyhow!("The inline fixture must have a snapshot"))?;
        view.accept_snapshot(snapshot, window, cx);
        let mut other_context = context.clone();
        other_context.anchor.line += 1;
        view.start_inline_comment(other_context, window, cx);
        anyhow::ensure!(
            view.inline_comment
                .as_ref()
                .is_some_and(|draft| draft.context == context
                    && draft.editor.read(cx).text(cx) == draft_text)
                && view.is_dirty(cx),
            "Inline drafts must retain their anchor and text through refresh and another line selection"
        );
        view.error = None;
        Ok(view)
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        reference: PullRequestRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            workspace,
            focus: cx.focus_handle(),
            reference,
            snapshot: None,
            description: None,
            diff: None,
            displayed_revision: None,
            tab: Tab::Files,
            selected_file: 0,
            viewed: HashSet::new(),
            comment: None,
            inline_comment: None,
            replies: HashMap::new(),
            comment_subscriptions: Vec::new(),
            loading: false,
            writing: false,
            error: None,
            task: None,
        };
        view.refresh(window, cx);
        view
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        let reference = self.reference.clone();
        let client = TwgSettings::get_global(cx).client();
        self.loading = true;
        self.error = None;
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let snapshot = client.get_pull_request(&reference).await?;
                    let diff = client.pull_request_diff(&reference).await;
                    let current = client.get_pull_request(&reference).await?;
                    anyhow::ensure!(
                        snapshot.revision() == current.revision(),
                        "The PR changed while loading. Refresh to view its current revision."
                    );
                    anyhow::Ok((current, diff))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok((snapshot, diff)) => {
                        this.accept_snapshot(snapshot, window, cx);
                        match diff {
                            Ok(diff) => {
                                this.selected_file =
                                    this.selected_file.min(diff.files.len().saturating_sub(1));
                                this.diff = Some(diff);
                                this.displayed_revision =
                                    this.snapshot.as_ref().map(PullRequest::revision);
                            }
                            Err(error) => {
                                this.diff = None;
                                this.displayed_revision = None;
                                this.error = Some(format!("PR loaded; diff unavailable: {error}"));
                            }
                        }
                    }
                    Err(error) => {
                        this.displayed_revision = None;
                        this.error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn mutate(&mut self, mutation: Mutation, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing || self.loading {
            return;
        }
        if !self.can_review() {
            self.error =
                Some("Refresh the current PR diff before submitting a review action.".into());
            cx.notify();
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let expected_revision = snapshot.revision();
        let reference = self.reference.clone();
        let client = TwgSettings::get_global(cx).client();
        let submitted_comment = if let Mutation::Comment { target, body } = &mutation {
            if let CommentTarget::Inline(context) = target {
                if !inline_context_is_current(
                    context,
                    self.displayed_revision.as_deref(),
                    self.diff.as_ref(),
                ) {
                    self.error = Some("This inline draft belongs to an earlier PR revision or an unavailable line. Copy or discard it, then choose a line in the current diff.".into());
                    cx.notify();
                    return;
                }
            }
            Some((target.clone(), body.clone()))
        } else {
            None
        };
        if submitted_comment
            .as_ref()
            .is_some_and(|(_, text)| text.trim().is_empty())
        {
            return;
        }
        self.writing = true;
        cx.emit(ItemEvent::UpdateTab);
        self.error = None;
        for reply in self.comment_editors() {
            reply.update(cx, |reply, _| reply.set_read_only(true));
        }
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match mutation {
                        Mutation::Approve => {
                            client
                                .review_pull_request(
                                    &reference,
                                    &expected_revision,
                                    ReviewAction::Approve,
                                )
                                .await
                        }
                        Mutation::RequestChanges => {
                            client
                                .review_pull_request(
                                    &reference,
                                    &expected_revision,
                                    ReviewAction::RequestChanges,
                                )
                                .await
                        }
                        Mutation::Comment { target, body } => {
                            let (parent, anchor) = match target {
                                CommentTarget::General => (None, None),
                                CommentTarget::Reply(id) => (Some(id), None),
                                CommentTarget::Inline(context) => (None, Some(context.anchor)),
                            };
                            client
                                .comment_pull_request(
                                    &reference,
                                    &expected_revision,
                                    &body,
                                    parent,
                                    anchor.as_ref(),
                                )
                                .await
                        }
                        Mutation::Resolve(id) => {
                            client
                                .resolve_pull_request_comment(&reference, &expected_revision, id)
                                .await
                        }
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.writing = false;
                for reply in this.comment_editors() {
                    reply.update(cx, |reply, _| reply.set_read_only(false));
                }
                match result {
                    Ok(PrWriteResult {
                        pull_request,
                        verification_error,
                    }) => {
                        if let Some((target, submitted)) = submitted_comment {
                            let editor = match &target {
                                CommentTarget::General => this.comment.as_ref(),
                                CommentTarget::Reply(id) => this.replies.get(id),
                                CommentTarget::Inline(context) => this.inline_comment.as_ref()
                                    .filter(|draft| &draft.context == context)
                                    .map(|draft| &draft.editor),
                            };
                            if let Some(reply) = editor {
                                if crate::state::submitted_comment_is_current(&submitted, &reply.read(cx).text(cx)) {
                                    reply.update(cx, |reply, cx| reply.set_text("", window, cx));
                                    if matches!(target, CommentTarget::Inline(_)) {
                                        this.inline_comment = None;
                                    }
                                }
                            }
                        }
                        if let Some(snapshot) = pull_request {
                            this.accept_snapshot(snapshot, window, cx);
                        }
                        if verification_error.is_some() { this.displayed_revision = None; }
                        this.error = Some(
                            verification_error
                                .map(|error| {
                                    format!("Action submitted; refresh to verify: {error}")
                                })
                                .unwrap_or_else(|| if this.can_review() { "Action saved to Bitbucket.".into() } else { "Action saved. The PR revision changed; refresh to load its current diff.".into() }),
                        );
                    }
                    Err(error) => {
                        this.error = Some(format!("Action failed; draft retained: {error}"))
                    }
                }
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn fix_with_agent(
        &mut self,
        comment: ReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_review() || comment.outdated || comment.deleted {
            self.error = Some(
                "Refresh the current PR diff before preparing a fix for this feedback.".into(),
            );
            cx.notify();
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let location = comment
            .inline
            .as_ref()
            .map(|inline| format!("{} old:{:?} new:{:?}", inline.path, inline.from, inline.to))
            .unwrap_or_default();
        let code = comment
            .inline
            .as_ref()
            .and_then(|inline| {
                self.diff.as_ref()?.files.iter().find(|file| {
                    file.new_path.as_ref() == Some(&inline.path)
                        || file.old_path.as_ref() == Some(&inline.path)
                })
            })
            .map(|file| {
                file.hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .map(|line| line.text.trim_end_matches(['\r', '\n']))
                    .take(150)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let prompt = ExternalSourcePrompt::new(&format!(
            "Review feedback for {}\nRevision {}\n{}\n{}\n\n{}\n\nCheck the local checkout before making changes. Prepare a fix for this feedback.\n{}",
            snapshot.url,
            snapshot.revision(),
            location,
            comment.user.display_name,
            comment.content.raw,
            code
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
            Ok(false) => self.error = Some("Enable the Agent panel before preparing a fix.".into()),
            Err(error) => self.error = Some(format!("Could not open the Agent panel: {error}")),
        }
        cx.notify();
    }

    fn can_review(&self) -> bool {
        self.diff.is_some()
            && displayed_revision_is_current(
                self.snapshot.as_ref().map(PullRequest::revision).as_deref(),
                self.displayed_revision.as_deref(),
                self.loading || self.writing,
            )
    }

    fn accept_snapshot(
        &mut self,
        snapshot: PullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.description =
            Some(cx.new(|cx| Markdown::new(snapshot.description.clone().into(), None, None, cx)));
        if self.comment.is_none() {
            self.comment =
                Some(self.new_comment_editor("Add a comment to this pull request…", window, cx));
        }
        if let Some(workspace) = self.workspace.upgrade()
            && let Some(panel) = workspace.read(cx).panel::<crate::WorkPanel>(cx)
        {
            let list = panel.read(cx).pr_list.clone();
            list.update(cx, |list, cx| list.update_snapshot(&snapshot, cx));
        }
        for comment in &snapshot.comments {
            if self.replies.contains_key(&comment.id) || comment.parent.is_some() || comment.deleted
            {
                continue;
            }
            let editor = self.new_comment_editor("Reply…", window, cx);
            self.replies.insert(comment.id, editor);
        }
        adopt_snapshot(
            &mut self.snapshot,
            &mut self.diff,
            &mut self.displayed_revision,
            &mut self.viewed,
            snapshot,
        );
    }

    fn new_comment_editor(
        &mut self,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        let editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(2, 8, window, cx);
            editor.set_placeholder_text(placeholder, window, cx);
            editor
        });
        self.comment_subscriptions
            .push(cx.subscribe(&editor, |_, _, event, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    cx.emit(ItemEvent::Edit);
                    cx.emit(ItemEvent::UpdateTab);
                    cx.notify();
                }
            }));
        editor
    }

    fn comment_editors(&self) -> impl Iterator<Item = &Entity<Editor>> {
        self.comment
            .iter()
            .chain(self.replies.values())
            .chain(self.inline_comment.iter().map(|draft| &draft.editor))
    }

    fn start_inline_comment(
        &mut self,
        context: InlineCommentContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_review()
            || !inline_context_is_current(
                &context,
                self.displayed_revision.as_deref(),
                self.diff.as_ref(),
            )
        {
            self.error =
                Some("Refresh the current PR diff before adding an inline comment.".into());
            cx.notify();
            return;
        }
        if let Some(draft) = &self.inline_comment {
            if draft.context != context && !draft.editor.read(cx).text(cx).is_empty() {
                self.error = Some(
                    "Post or discard the existing inline draft before starting another.".into(),
                );
                draft.editor.focus_handle(cx).focus(window, cx);
                cx.notify();
                return;
            }
        }
        let editor = self
            .inline_comment
            .as_ref()
            .map(|draft| draft.editor.clone())
            .unwrap_or_else(|| self.new_comment_editor("Comment on this line…", window, cx));
        self.inline_comment = Some(InlineCommentDraft {
            context,
            editor: editor.clone(),
        });
        self.error = None;
        editor.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn render_inline_comment(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = &self.inline_comment else {
            return div().into_any_element();
        };
        let current = inline_context_is_current(
            &draft.context,
            self.displayed_revision.as_deref(),
            self.diff.as_ref(),
        );
        let can_post =
            self.can_review() && current && !draft.editor.read(cx).text(cx).trim().is_empty();
        v_flex().gap_2().p_3().border_1().border_color(cx.theme().colors().border).rounded_md()
            .child(Label::new(format!("Inline comment · {}:{} ({})", draft.context.anchor.path, draft.context.anchor.line,
                match draft.context.anchor.side { DiffSide::Old => "old", DiffSide::New => "new" })).color(Color::Muted))
            .when(!current, |view| view.child(Label::new("This draft belongs to an earlier revision or an unavailable line. Copy the text or discard it before choosing a new line.").color(Color::Warning)))
            .child(crate::ticket_editor::editor_field("", draft.editor.clone(), !self.writing, cx))
            .child(h_flex().gap_2()
                .child(Button::new("post-inline-comment", "Post inline comment").tab_index(0_isize).disabled(!can_post)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(draft) = &this.inline_comment {
                            let target = CommentTarget::Inline(draft.context.clone());
                            let body = draft.editor.read(cx).text(cx);
                            this.mutate(Mutation::Comment { target, body }, window, cx);
                        }
                    })))
                .child(Button::new("discard-inline-comment", "Discard draft").tab_index(0_isize).disabled(self.writing)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.writing { return; }
                        this.inline_comment = None;
                        this.error = None;
                        cx.emit(ItemEvent::Edit);
                        cx.emit(ItemEvent::UpdateTab);
                        cx.notify();
                    }))))
            .into_any_element()
    }

    fn checkout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing || self.loading {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let branch = snapshot.source.branch.name.clone();
        let source_commit = snapshot.source.commit.hash.clone();
        let source_repository = snapshot.source.repository.full_name.clone();
        let expected_revision = snapshot.revision();
        let reference = self.reference.clone();
        let client = TwgSettings::get_global(cx).client();
        let result = self.workspace.read_with(cx, |workspace, cx| {
            anyhow::ensure!(!workspace.project().read(cx).is_via_collab(), "Check out the PR in your own local or remote project, rather than a shared project.");
            anyhow::ensure!(workspace.project().read(cx).dirty_buffers(cx).next().is_none(), "Save or discard unsaved editor changes before checking out this PR.");
            let repository = workspace.project().read(cx).active_repository(cx).ok_or_else(|| anyhow::anyhow!("Open the PR source repository first."))?;
            anyhow::ensure!(repository.read(cx).statuses_by_path.is_empty(), "Commit or stash local changes before checking out this PR.");
            Ok(repository.clone())
        }).and_then(|result| result);
        let repository = match result {
            Ok(repository) => repository,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let askpass = {
            let workspace = self.workspace.clone();
            let handle = window.window_handle();
            AskPassDelegate::new_with_cancellation(
                &mut cx.to_async(),
                move |prompt, response, cancellation, cx| {
                    handle
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    workspace.toggle_modal(window, cx, |window, cx| {
                                        AskPassModal::new(
                                            "Fetch PR source branch".into(),
                                            prompt.into(),
                                            response,
                                            cancellation,
                                            window,
                                            cx,
                                        )
                                    });
                                })
                                .log_err();
                        })
                        .log_err();
                },
            )
        };
        self.writing = true;
        self.error = None;
        cx.emit(ItemEvent::UpdateTab);
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let current = cx.background_spawn(async move { client.get_pull_request(&reference).await }).await?;
                anyhow::ensure!(current.revision() == expected_revision, "The PR changed. Refresh before checking out its source branch.");
                let remotes = repository.update(cx, |repository, _| repository.remote_urls()).await??;
                let mut matching_remotes = remotes.iter().filter_map(|(name, url)| {
                    let reference = RepositoryRef::from_remote(url).ok()?;
                    (format!("{}/{}", reference.workspace, reference.repository) == source_repository).then(|| name.clone())
                }).collect::<Vec<_>>();
                matching_remotes.sort_by_key(|name| (name != "origin", name != "upstream", name.clone()));
                let remote = matching_remotes.into_iter().next().ok_or_else(|| anyhow::anyhow!("Add a Git remote for the PR source repository {source_repository} in the Git panel, then retry."))?;
                let scan = repository.update(cx, |repository, _| repository.branches()).await??;
                anyhow::ensure!(scan.error.is_none(), "Git could not read all branches: {}", scan.error.as_deref().unwrap_or_default());
                let branch_tips = |branches: &[git::repository::Branch]| branches.iter().map(|branch| (branch.ref_name.to_string(), branch.most_recent_commit.as_ref().map(|commit| commit.sha.to_string()))).collect::<Vec<_>>();
                let mut branches = branch_tips(&scan.branches);
                let target = match checkout_target(&branch, &source_commit, &remote, &branches) {
                    Ok(target) => target,
                    Err(error) if branches.iter().any(|(name, _)| name == &format!("refs/heads/{branch}")) => return Err(error),
                    Err(_) => {
                        anyhow::ensure!(!remote.starts_with('-') && !remote.contains('/') && !remote.chars().any(char::is_control), "Invalid Git remote name");
                        repository.update(cx, |repository, cx| repository.fetch(FetchOptions::Remote(Remote { name: remote.clone().into() }), askpass, cx)).await??;
                        let scan = repository.update(cx, |repository, _| repository.branches()).await??;
                        anyhow::ensure!(scan.error.is_none(), "Git could not read all branches after fetching");
                        branches = branch_tips(&scan.branches);
                        checkout_target(&branch, &source_commit, &remote, &branches)?
                    }
                };
                this.read_with(cx, |this, cx| {
                    this.workspace.read_with(cx, |workspace, cx| {
                        anyhow::ensure!(workspace.project().read(cx).dirty_buffers(cx).next().is_none(), "Unsaved editor changes appeared while preparing checkout. Save them before retrying.");
                        Ok(())
                    })?
                })??;
                let checkout = repository.update(cx, |repository, _| {
                    anyhow::ensure!(repository.statuses_by_path.is_empty(), "Local changes appeared while preparing checkout. Commit or stash them first.");
                    Ok(repository.change_branch(target))
                })?;
                checkout.await??;
                let head = repository.update(cx, |repository, _| repository.head_sha()).await??;
                anyhow::ensure!(head.as_deref() == Some(&source_commit), "The checked-out branch does not match the PR source commit. No commits were reset; inspect the Git panel and refresh the PR before editing.");
                Ok::<_, anyhow::Error>(())
            }.await;
            this.update(cx, |this, cx| {
                this.writing = false;
                this.error = Some(match result { Ok(()) => "PR source revision checked out.".into(), Err(error) => format!("Checkout failed: {error}") });
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn render_comment(&self, comment: &ReviewComment, cx: &mut Context<Self>) -> AnyElement {
        let id = comment.id;
        let comment_for_agent = comment.clone();
        v_flex()
            .id(SharedString::from(format!("pr-comment-{id}")))
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_md()
            .child(
                Label::new(format!(
                    "{}{}{}",
                    comment.user.display_name,
                    if comment.resolved { " · Resolved" } else { "" },
                    if comment.outdated {
                        " · Outdated revision"
                    } else {
                        ""
                    }
                ))
                .color(Color::Muted),
            )
            .when_some(comment.inline.as_ref(), |view, inline| {
                view.child(
                    Label::new(format!(
                        "{}:{}",
                        inline.path,
                        inline.to.or(inline.from).unwrap_or_default()
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Accent),
                )
            })
            .child(Label::new(comment.content.raw.clone()))
            .children(
                self.snapshot
                    .iter()
                    .flat_map(|snapshot| &snapshot.comments)
                    .filter(|reply| {
                        reply.parent.as_ref().is_some_and(|parent| parent.id == id)
                            && !reply.deleted
                    })
                    .map(|reply| {
                        v_flex()
                            .pl_4()
                            .child(Label::new(reply.user.display_name.clone()).color(Color::Muted))
                            .child(Label::new(reply.content.raw.clone()))
                    }),
            )
            .children(
                self.replies.get(&id).cloned().map(|editor| {
                    crate::ticket_editor::editor_field("", editor, !self.writing, cx)
                }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("reply-{id}")), "Reply")
                            .tab_index(0_isize)
                            .disabled(!self.can_review() || comment.outdated)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(reply) = this.replies.get(&id) {
                                    let text = reply.read(cx).text(cx);
                                    this.mutate(
                                        Mutation::Comment {
                                            target: CommentTarget::Reply(id),
                                            body: text,
                                        },
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("fix-{id}")), "Fix with agent")
                            .tab_index(0_isize)
                            .disabled(!self.can_review() || comment.outdated || comment.deleted)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fix_with_agent(comment_for_agent.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("resolve-{id}")), "Resolve")
                            .tab_index(0_isize)
                            .disabled(!self.can_review() || comment.resolved || comment.outdated)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.mutate(Mutation::Resolve(id), window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_file(&self, file: &DiffFile, cx: &mut Context<Self>) -> AnyElement {
        let path = file
            .new_path
            .as_ref()
            .or(file.old_path.as_ref())
            .cloned()
            .unwrap_or_else(|| "File".into());
        let viewed = self.viewed.contains(&path);
        let viewed_path = path.clone();
        let comments = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.comments.as_slice())
            .unwrap_or_default();
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .justify_between()
                    .p_2()
                    .child(
                        div()
                            .id("pr-file-path")
                            .flex_1()
                            .min_w_0()
                            .tooltip(Tooltip::text(path.clone()))
                            .child(Label::new(path.clone()).truncate_start()),
                    )
                    .child(
                        Button::new("viewed", if viewed { "✓ Viewed" } else { "Mark viewed" })
                            .tab_index(0_isize)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.viewed.remove(&viewed_path) {
                                    this.viewed.insert(viewed_path.clone());
                                }
                                cx.notify();
                            })),
                    ),
            )
            .when(file.binary, |view| {
                view.child(
                    Label::new("Binary file changed. Open in Bitbucket to view.")
                        .color(Color::Muted),
                )
            })
            .when(!file.binary && file.hunks.is_empty(), |view| {
                view.child(Label::new("File metadata changed; no text hunks.").color(Color::Muted))
            })
            .children(file.hunks.iter().map(|hunk| {
                v_flex()
                    .child(
                        Label::new(format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start))
                            .color(Color::Muted),
                    )
                    .children(hunk.lines.iter().map(|line| {
                        let anchor = line_comment_anchor(file, line);
                        let draft_here = self.inline_comment.as_ref().is_some_and(|draft| {
                            Some(draft.context.revision.as_str())
                                == self.displayed_revision.as_deref()
                                && anchor.as_ref() == Some(&draft.context.anchor)
                        });
                        let color = match line.kind {
                            DiffLineKind::Added => cx.theme().status().created.opacity(0.15),
                            DiffLineKind::Deleted => cx.theme().status().deleted.opacity(0.15),
                            DiffLineKind::Context => cx.theme().colors().editor_background,
                        };
                        v_flex()
                            .child(
                                h_flex()
                                    .bg(color)
                                    .whitespace_nowrap()
                                    .gap_2()
                                    .px_2()
                                    .children(
                                        anchor.clone().zip(self.displayed_revision.clone()).map(
                                            |(anchor, revision)| {
                                                let label = format!(
                                                    "Comment on {} line {}",
                                                    match anchor.side {
                                                        DiffSide::Old => "old",
                                                        DiffSide::New => "new",
                                                    },
                                                    anchor.line
                                                );
                                                let id = SharedString::from(format!(
                                                    "inline-{}-{:?}-{}",
                                                    anchor.path, anchor.side, anchor.line
                                                ));
                                                let context =
                                                    InlineCommentContext { revision, anchor };
                                                IconButton::new(id, IconName::Plus)
                                                    .icon_size(IconSize::Small)
                                                    .tab_index(0_isize)
                                                    .aria_label(label.clone())
                                                    .tooltip(Tooltip::text(label))
                                                    .disabled(!self.can_review())
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.start_inline_comment(
                                                                context.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    ))
                                            },
                                        ),
                                    )
                                    .child(
                                        Label::new(
                                            line.old_line
                                                .map(|number| number.to_string())
                                                .unwrap_or_default(),
                                        )
                                        .color(Color::Muted)
                                        .flex_shrink_0(),
                                    )
                                    .child(
                                        Label::new(
                                            line.new_line
                                                .map(|number| number.to_string())
                                                .unwrap_or_default(),
                                        )
                                        .color(Color::Muted)
                                        .flex_shrink_0(),
                                    )
                                    .child(
                                        Label::new(format!(
                                            "{} {}",
                                            match line.kind {
                                                DiffLineKind::Added => "+",
                                                DiffLineKind::Deleted => "−",
                                                DiffLineKind::Context => " ",
                                            },
                                            line.text.trim_end_matches(['\r', '\n'])
                                        ))
                                        .buffer_font(cx)
                                        .flex_shrink_0(),
                                    ),
                            )
                            .when(draft_here, |view| {
                                view.child(self.render_inline_comment(cx))
                            })
                            .children(
                                comments
                                    .iter()
                                    .filter(|comment| {
                                        !comment.deleted
                                            && comment.parent.is_none()
                                            && !comment.outdated
                                            && comment.inline.as_ref().is_some_and(|inline| {
                                                inline_matches(file, line, inline)
                                            })
                                    })
                                    .map(|comment| self.render_comment(comment, cx)),
                            )
                    }))
            }))
            .into_any_element()
    }
}

impl Render for PullRequestView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.loading || self.writing;
        let mut body = v_flex()
            .id("pr-content")
            .text_color(cx.theme().colors().text)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .p_3();
        if let Some(snapshot) = &self.snapshot {
            match self.tab {
                Tab::Overview => {
                    let view = cx.entity().downgrade();
                    body = body.children(self.description.clone().map(|description| {
                        MarkdownElement::new(
                            description,
                            MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                        )
                        .on_url_click(move |url, _, cx| {
                            if allowed_description_link(&url) {
                                cx.open_url(&url);
                            } else {
                                view.update(cx, |view, cx| {
                                    view.error = Some("Only web and email links can be opened from pull request descriptions.".into());
                                    cx.notify();
                                }).log_err();
                            }
                        })
                    }))
                        .children(snapshot.participants.iter().map(|participant| Label::new(format!("{} · {}", participant.user.display_name, if participant.approved { "Approved" } else { participant.state.as_str() }))))
                        .children(snapshot.statuses.iter().map(|status| Label::new(format!("{} · {}", status.get("name").and_then(|value| value.as_str()).unwrap_or("Build"), status.get("state").and_then(|value| value.as_str()).unwrap_or("Unknown")))))
                        .when(!snapshot.failures.is_empty(), |view| view.child(Label::new("Some PR details could not be loaded. Open Bitbucket for the complete view.").color(Color::Warning)));
                }
                Tab::Files => {
                    if let Some(diff) = &self.diff {
                        let files = v_flex()
                            .w_56()
                            .flex_shrink_0()
                            .min_w_0()
                            .overflow_hidden()
                            .pr_2()
                            .border_r_1()
                            .border_color(cx.theme().colors().border_variant)
                            .gap_1()
                            .children(diff.files.iter().enumerate().map(|(index, file)| {
                                let path = file
                                    .new_path
                                    .as_ref()
                                    .or(file.old_path.as_ref())
                                    .cloned()
                                    .unwrap_or_default();
                                ui::ButtonLike::new(SharedString::from(format!("file-{index}")))
                                    .tab_index(0_isize)
                                    .full_width()
                                    .aria_label(path.clone())
                                    .tooltip(Tooltip::text(path.clone()))
                                    .style(if self.selected_file == index {
                                        ButtonStyle::Tinted(TintColor::Accent)
                                    } else {
                                        ButtonStyle::Subtle
                                    })
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w_0()
                                            .text_left()
                                            .child(Label::new(path).truncate_start()),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_file = index;
                                        cx.notify();
                                    }))
                            }));
                        let file = diff
                            .files
                            .get(self.selected_file)
                            .map(|file| self.render_file(file, cx));
                        body = body.child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_start()
                                .gap_3()
                                .child(files)
                                .child(
                                    div()
                                        .id(("pr-selected-diff", self.selected_file))
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_x_scroll()
                                        .restrict_scroll_to_axis()
                                        .children(file),
                                ),
                        );
                    }
                }
                Tab::Comments => {
                    body = body.children(self.comment.clone().map(|comment| {
                        let can_post =
                            self.can_review() && !comment.read(cx).text(cx).trim().is_empty();
                        v_flex()
                            .gap_2()
                            .pb_3()
                            .border_b_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(crate::ticket_editor::editor_field(
                                "New comment",
                                comment,
                                !busy,
                                cx,
                            ))
                            .child(
                                h_flex().justify_end().child(
                                    Button::new("post-pr-comment", "Post comment")
                                        .tab_index(0_isize)
                                        .disabled(!can_post)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if let Some(comment) = &this.comment {
                                                let text = comment.read(cx).text(cx);
                                                this.mutate(
                                                    Mutation::Comment {
                                                        target: CommentTarget::General,
                                                        body: text,
                                                    },
                                                    window,
                                                    cx,
                                                );
                                            }
                                        })),
                                ),
                            )
                            .when(!self.can_review() && !busy, |view| {
                                view.child(
                                    Label::new("Refresh this PR before posting a comment.")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            })
                    }));
                    body = body.children(
                        snapshot
                            .comments
                            .iter()
                            .filter(|comment| !comment.deleted && comment.parent.is_none())
                            .map(|comment| self.render_comment(comment, cx)),
                    );
                }
                Tab::Commits => {
                    body = body.children(snapshot.commits.iter().map(|commit| {
                        let url = commit
                            .browser_url(&snapshot.source.repository, &self.reference.repository());
                        let tooltip = match &url {
                            Ok(url) => format!("Open in Bitbucket: {url}"),
                            Err(error) => format!("Commit link unavailable: {error}"),
                        };
                        ui::ButtonLike::new(SharedString::from(format!("commit-{}", commit.hash)))
                            .tab_index(0_isize)
                            .full_width()
                            .auto_height()
                            .style(ButtonStyle::Subtle)
                            .disabled(url.is_err())
                            .aria_label(format!("Open commit {} in Bitbucket", commit.hash))
                            .tooltip(Tooltip::text(tooltip))
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .items_start()
                                    .text_left()
                                    .gap_2()
                                    .p_2()
                                    .border_b_1()
                                    .border_color(cx.theme().colors().border_variant)
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                Label::new(commit.hash.clone())
                                                    .color(Color::Accent),
                                            )
                                            .child(Label::new(commit.message.clone())),
                                    )
                                    .child(
                                        Icon::new(IconName::ArrowUpRight)
                                            .size(IconSize::Small)
                                            .color(Color::Muted),
                                    ),
                            )
                            .on_click(move |_, _, cx| {
                                if let Ok(url) = &url {
                                    cx.open_url(url);
                                }
                            })
                    }));
                }
            }
        }
        if self.tab == Tab::Files
            && let Some(draft) = &self.inline_comment
        {
            let selected = self
                .diff
                .as_ref()
                .and_then(|diff| diff.files.get(self.selected_file));
            let visible = inline_context_is_current(&draft.context, self.displayed_revision.as_deref(), self.diff.as_ref())
                && selected.is_some_and(|file| match draft.context.anchor.side {
                    DiffSide::Old => file.old_path.as_ref(),
                    DiffSide::New => file.new_path.as_ref(),
                } == Some(&draft.context.anchor.path));
            if !visible {
                body = body.child(self.render_inline_comment(cx));
            }
        }
        for (id, draft) in &self.replies {
            let available = self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .comments
                    .iter()
                    .any(|comment| comment.id == *id && !comment.deleted)
            });
            if !available && !draft.read(cx).text(cx).is_empty() {
                body = body.child(v_flex().gap_2().child(Label::new(format!("Draft for unavailable comment #{id}. Copy or clear it before closing.")).color(Color::Warning)).child(draft.clone()));
            }
        }
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            Label::new(
                                self.snapshot
                                    .as_ref()
                                    .map(|snapshot| snapshot.title.clone())
                                    .unwrap_or_else(|| format!("PR #{}", self.reference.number)),
                            )
                            .size(LabelSize::Large),
                        ),
                    )
                    .child(
                        Button::new("pr-checkout", "Check out")
                            .tab_index(0_isize)
                            .disabled(busy || self.snapshot.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.checkout(window, cx))),
                    )
                    .child(
                        Button::new("pr-request-changes", "Request changes")
                            .tab_index(0_isize)
                            .disabled(
                                !self.can_review()
                                    || self
                                        .snapshot
                                        .as_ref()
                                        .is_none_or(|snapshot| snapshot.state != "OPEN"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.mutate(Mutation::RequestChanges, window, cx)
                            })),
                    )
                    .child(
                        Button::new("pr-approve", "Approve")
                            .tab_index(0_isize)
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .disabled(
                                !self.can_review()
                                    || self
                                        .snapshot
                                        .as_ref()
                                        .is_none_or(|snapshot| snapshot.state != "OPEN"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.mutate(Mutation::Approve, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("pr-refresh", IconName::RotateCw)
                            .tab_index(0_isize)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    )
                    .child(
                        IconButton::new("pr-browser", IconName::ArrowUpRight)
                            .tab_index(0_isize)
                            .on_click(
                                cx.listener(|this, _, _, cx| cx.open_url(&this.reference.url())),
                            ),
                    ),
            )
            .when_some(self.snapshot.as_ref(), |view, snapshot| {
                view.child(
                    Label::new(format!(
                        "  {} → {} · {} · revision {}",
                        snapshot.source.branch.name,
                        snapshot.destination.branch.name,
                        snapshot.state,
                        snapshot.source.commit.hash
                    ))
                    .color(Color::Muted),
                )
            })
            .child(
                h_flex().gap_2().p_2().children(
                    [
                        (Tab::Overview, "Overview"),
                        (Tab::Files, "Files"),
                        (Tab::Comments, "Comments"),
                        (Tab::Commits, "Commits"),
                    ]
                    .into_iter()
                    .map(|(tab, label)| {
                        Button::new(label, label)
                            .tab_index(0_isize)
                            .style(if self.tab == tab {
                                ButtonStyle::Tinted(TintColor::Accent)
                            } else {
                                ButtonStyle::Subtle
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.tab = tab;
                                cx.notify();
                            }))
                    }),
                ),
            )
            .when(busy, |view| {
                view.child(
                    Label::new(if self.writing {
                        "Saving to Bitbucket…"
                    } else {
                        "Loading PR…"
                    })
                    .color(Color::Muted),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(div().p_2().child(Label::new(error).color(Color::Warning)))
            })
            .child(body)
    }
}

impl Focusable for PullRequestView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl EventEmitter<ItemEvent> for PullRequestView {}
impl Item for PullRequestView {
    type Event = ItemEvent;
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        format!("PR #{}", self.reference.number).into()
    }
    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        Some(self.reference.url().into())
    }
    fn to_item_events(event: &Self::Event, emit: &mut dyn FnMut(ItemEvent)) {
        emit(*event);
    }
    fn is_dirty(&self, cx: &App) -> bool {
        let drafts = self
            .comment_editors()
            .map(|reply| reply.read(cx).text(cx))
            .collect::<Vec<_>>();
        has_pending_work(self.writing, drafts.iter().map(String::as_str))
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
            "PR drafts are never posted by Save. Cancel closing to keep editing, use Post comment, Post inline comment, or Reply to submit, or choose Don't Save to discard the draft."
        )))
    }
}

fn displayed_revision_is_current(
    snapshot: Option<&str>,
    displayed: Option<&str>,
    busy: bool,
) -> bool {
    !busy && snapshot.is_some() && snapshot == displayed
}

fn line_comment_anchor(file: &DiffFile, line: &DiffLine) -> Option<DiffAnchor> {
    if file.binary {
        return None;
    }
    file.anchor(line, DiffSide::New)
        .or_else(|| file.anchor(line, DiffSide::Old))
}

fn inline_context_is_current(
    context: &InlineCommentContext,
    displayed: Option<&str>,
    diff: Option<&PullRequestDiff>,
) -> bool {
    displayed == Some(context.revision.as_str())
        && diff.is_some_and(|diff| diff.contains_anchor(&context.anchor))
}

fn adopt_snapshot(
    snapshot: &mut Option<PullRequest>,
    diff: &mut Option<PullRequestDiff>,
    displayed: &mut Option<String>,
    viewed: &mut HashSet<String>,
    incoming: PullRequest,
) {
    if displayed.as_deref() != Some(incoming.revision().as_str()) {
        *diff = None;
        *displayed = None;
        viewed.clear();
    }
    *snapshot = Some(incoming);
}

fn inline_matches(
    file: &DiffFile,
    line: &twg::bitbucket::DiffLine,
    inline: &twg::bitbucket::InlineLocation,
) -> bool {
    if let Some(to) = inline.to {
        file.new_path.as_ref() == Some(&inline.path) && line.new_line == Some(to)
    } else if let Some(from) = inline.from {
        file.old_path.as_ref() == Some(&inline.path) && line.old_line == Some(from)
    } else {
        false
    }
}

fn checkout_target(
    branch: &str,
    commit: &str,
    remote: &str,
    branches: &[(String, Option<String>)],
) -> anyhow::Result<String> {
    anyhow::ensure!(
        !branch.is_empty() && !branch.starts_with('-') && !branch.chars().any(char::is_control),
        "Invalid PR branch name"
    );
    anyhow::ensure!(
        !remote.is_empty()
            && !remote.starts_with('-')
            && !remote.contains('/')
            && !remote.chars().any(char::is_control),
        "Invalid Git remote name"
    );
    anyhow::ensure!(
        !commit.is_empty(),
        "The PR source commit is unavailable; refresh before checkout"
    );
    let local_ref = format!("refs/heads/{branch}");
    if let Some((_, tip)) = branches.iter().find(|(name, _)| name == &local_ref) {
        anyhow::ensure!(
            tip.as_deref() == Some(commit),
            "Local branch {branch} contains different commits. Use another worktree or update it in the Git panel; no branch was reset."
        );
        return Ok(branch.to_owned());
    }
    let remote_ref = format!("refs/remotes/{remote}/{branch}");
    anyhow::ensure!(
        branches
            .iter()
            .any(|(name, tip)| name == &remote_ref && tip.as_deref() == Some(commit)),
        "Remote branch {remote}/{branch} is missing or differs from the PR source commit. Fetch this remote and refresh the PR before retrying."
    );
    Ok(format!("{remote}/{branch}"))
}

fn has_pending_work<'a>(writing: bool, mut drafts: impl Iterator<Item = &'a str>) -> bool {
    writing || drafts.any(|draft| !draft.is_empty())
}

fn allowed_description_link(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "mailto"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use twg::bitbucket::{DiffLine, DiffLineKind, InlineLocation, parse_pull_request};

    #[test]
    fn inline_context_requires_the_same_revision_and_an_existing_line() {
        let diff = PullRequestDiff::parse("diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new\n").expect("diff");
        let mut context = InlineCommentContext {
            revision: "revision-a".into(),
            anchor: DiffAnchor {
                path: "file.rs".into(),
                side: DiffSide::New,
                line: 1,
            },
        };
        assert!(inline_context_is_current(
            &context,
            Some("revision-a"),
            Some(&diff)
        ));
        assert!(!inline_context_is_current(
            &context,
            Some("revision-b"),
            Some(&diff)
        ));
        assert!(!inline_context_is_current(&context, None, Some(&diff)));
        assert!(!inline_context_is_current(
            &context,
            Some("revision-a"),
            None
        ));
        context.anchor.line = 2;
        assert!(!inline_context_is_current(
            &context,
            Some("revision-a"),
            Some(&diff)
        ));
    }

    #[test]
    fn inline_controls_use_new_context_lines_and_old_deleted_paths() {
        let diff = PullRequestDiff::parse("diff --git a/old.rs b/new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n").expect("diff");
        let file = &diff.files[0];
        let lines = &file.hunks[0].lines;
        assert_eq!(
            line_comment_anchor(file, &lines[0]),
            Some(DiffAnchor {
                path: "new.rs".into(),
                side: DiffSide::New,
                line: 1
            })
        );
        assert_eq!(
            line_comment_anchor(file, &lines[1]),
            Some(DiffAnchor {
                path: "old.rs".into(),
                side: DiffSide::Old,
                line: 2
            })
        );
        assert_eq!(
            line_comment_anchor(file, &lines[2]),
            Some(DiffAnchor {
                path: "new.rs".into(),
                side: DiffSide::New,
                line: 2
            })
        );
    }

    #[test]
    fn description_links_only_open_web_or_email_urls() {
        for url in [
            "https://jira.example.invalid/browse/DAT-1206",
            "http://example.invalid/review",
            "HTTPS://example.invalid/review",
            "mailto:reviewer@example.invalid",
        ] {
            assert!(super::allowed_description_link(url), "{url}");
        }
        for url in [
            "file:///tmp/private.txt",
            "zed://open/a/project",
            "javascript:alert(1)",
            "data:text/html,hello",
            "/relative/path",
            "//example.invalid/review",
        ] {
            assert!(!super::allowed_description_link(url), "{url}");
        }
    }

    fn snapshot(source: &str) -> PullRequest {
        parse_pull_request(serde_json::json!({"id":42,"title":"Fix","state":"OPEN","source":{"commit":{"hash":source}},"destination":{"commit":{"hash":"base"}}}), PullRequestRef {workspace:"team".into(), repository:"repo".into(), number:42}).expect("PR")
    }

    #[test]
    fn review_and_agent_context_require_the_displayed_revision() {
        assert!(displayed_revision_is_current(
            Some("a:base"),
            Some("a:base"),
            false
        ));
        assert!(!displayed_revision_is_current(
            Some("b:base"),
            Some("a:base"),
            false
        ));
        assert!(!displayed_revision_is_current(Some("a:base"), None, false));
        assert!(!displayed_revision_is_current(
            Some("a:base"),
            Some("a:base"),
            true
        ));
    }

    #[test]
    fn postwrite_revision_change_invalidates_diff_and_viewed_files() {
        let mut current = Some(snapshot("a"));
        let mut diff = Some(PullRequestDiff::parse("").expect("diff"));
        let mut displayed = Some("a:base".to_owned());
        let mut viewed = HashSet::from(["file.rs".to_owned()]);
        adopt_snapshot(
            &mut current,
            &mut diff,
            &mut displayed,
            &mut viewed,
            snapshot("b"),
        );
        assert!(diff.is_none());
        assert!(displayed.is_none());
        assert!(viewed.is_empty());
        assert_eq!(current.as_ref().expect("snapshot").revision(), "b:base");
    }

    #[test]
    fn metadata_refresh_on_same_revision_preserves_review_progress() {
        let mut current = Some(snapshot("a"));
        let mut diff = Some(PullRequestDiff::parse("").expect("diff"));
        let mut displayed = Some("a:base".to_owned());
        let mut viewed = HashSet::from(["file.rs".to_owned()]);
        adopt_snapshot(
            &mut current,
            &mut diff,
            &mut displayed,
            &mut viewed,
            snapshot("a"),
        );
        assert!(diff.is_some());
        assert_eq!(viewed.len(), 1);
    }

    #[test]
    fn renamed_old_side_comments_match_the_old_path_and_line() {
        let file = DiffFile {
            old_path: Some("before.rs".into()),
            new_path: Some("after.rs".into()),
            binary: false,
            hunks: Vec::new(),
        };
        let deleted = DiffLine {
            kind: DiffLineKind::Deleted,
            text: "old".into(),
            old_line: Some(8),
            new_line: None,
        };
        let added = DiffLine {
            kind: DiffLineKind::Added,
            text: "new".into(),
            old_line: None,
            new_line: Some(9),
        };
        let old = InlineLocation {
            path: "before.rs".into(),
            from: Some(8),
            to: None,
        };
        let new = InlineLocation {
            path: "after.rs".into(),
            from: None,
            to: Some(9),
        };
        assert!(inline_matches(&file, &deleted, &old));
        assert!(inline_matches(&file, &added, &new));
        assert!(!inline_matches(&file, &added, &old));
    }

    #[test]
    fn checkout_only_selects_the_exact_source_commit() {
        let branches = vec![("refs/remotes/origin/feature".into(), Some("abc".into()))];
        assert_eq!(
            checkout_target("feature", "abc", "origin", &branches).expect("remote"),
            "origin/feature"
        );
        let local = vec![("refs/heads/feature".into(), Some("abc".into()))];
        assert_eq!(
            checkout_target("feature", "abc", "origin", &local).expect("local"),
            "feature"
        );
        let conflicting = vec![
            ("refs/heads/feature".into(), Some("user-commit".into())),
            ("refs/remotes/origin/feature".into(), Some("abc".into())),
        ];
        assert!(checkout_target("feature", "abc", "origin", &conflicting).is_err());
        assert!(checkout_target("feature", "new-tip", "origin", &branches).is_err());
        assert!(checkout_target("-malicious", "abc", "origin", &branches).is_err());
    }

    #[test]
    fn pending_reply_or_write_requires_close_confirmation() {
        assert!(has_pending_work(false, ["draft"].into_iter()));
        assert!(has_pending_work(true, [""].into_iter()));
        assert!(!has_pending_work(false, [""].into_iter()));
    }
}
