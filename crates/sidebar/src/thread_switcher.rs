use action_log::DiffStats;
#[cfg(test)]
use agent_ui::TerminalId;
use agent_ui::{
    terminal_thread_metadata_store::TerminalThreadMetadata, thread_metadata_store::ThreadMetadata,
    threads_archive_view::fuzzy_match_positions,
};
use editor::Editor;
use gpui::{
    Action as _, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Modifiers,
    ModifiersChangedEvent, Render, ScrollHandle, SharedString, prelude::*,
};
use ui::{AgentThreadStatus, ThreadItem, ThreadItemWorktreeInfo, WithScrollbar, prelude::*};
use workspace::{ModalView, ProjectGroupKey};
use zed_actions::agents_sidebar::ToggleThreadSwitcher;

use super::ThreadEntryWorkspace;

#[derive(Clone)]
pub(crate) struct ThreadSwitcherThreadEntry {
    pub title: SharedString,
    pub icon: IconName,
    pub icon_from_external_svg: Option<SharedString>,
    pub status: AgentThreadStatus,
    pub metadata: ThreadMetadata,
    pub workspace: ThreadEntryWorkspace,
    pub project_group_key: Option<ProjectGroupKey>,
    pub project_name: Option<SharedString>,
    pub worktrees: Vec<ThreadItemWorktreeInfo>,
    pub diff_stats: DiffStats,
    pub is_draft: bool,
    pub is_title_generating: bool,
    pub notified: bool,
    pub timestamp: SharedString,
}

#[derive(Clone)]
pub(crate) struct ThreadSwitcherTerminalEntry {
    pub metadata: TerminalThreadMetadata,
    pub(super) workspace: ThreadEntryWorkspace,
    pub project_group_key: Option<ProjectGroupKey>,
    pub project_name: Option<SharedString>,
    pub worktrees: Vec<ThreadItemWorktreeInfo>,
    pub notified: bool,
    pub timestamp: SharedString,
}

#[derive(Clone)]
pub(crate) enum ThreadSwitcherEntry {
    Thread(ThreadSwitcherThreadEntry),
    Terminal(ThreadSwitcherTerminalEntry),
}

#[derive(Clone)]
pub(super) enum ThreadSwitcherSelection {
    Thread {
        metadata: ThreadMetadata,
        workspace: ThreadEntryWorkspace,
    },
    Terminal {
        metadata: TerminalThreadMetadata,
        workspace: ThreadEntryWorkspace,
    },
}

impl ThreadSwitcherEntry {
    fn project_group_key(&self) -> Option<&ProjectGroupKey> {
        match self {
            Self::Thread(entry) => entry.project_group_key.as_ref(),
            Self::Terminal(entry) => entry.project_group_key.as_ref(),
        }
    }

    fn group(&self) -> usize {
        match self.status() {
            AgentThreadStatus::WaitingForConfirmation
            | AgentThreadStatus::Unconfirmed
            | AgentThreadStatus::Error => 0,
            AgentThreadStatus::Running => 1,
            _ if self.notified() => 0,
            _ => 2,
        }
    }

    fn matches_query(&self, query: &str) -> bool {
        query.is_empty()
            || fuzzy_match_positions(query, &self.title()).is_some()
            || self
                .project_name()
                .is_some_and(|name| fuzzy_match_positions(query, &name).is_some())
            || self.worktrees().iter().any(|worktree| {
                [&worktree.worktree_name, &worktree.branch_name]
                    .into_iter()
                    .flatten()
                    .any(|name| fuzzy_match_positions(query, name).is_some())
                    || fuzzy_match_positions(query, &worktree.full_path).is_some()
            })
    }

    pub(super) fn selection(&self) -> ThreadSwitcherSelection {
        match self {
            Self::Thread(entry) => ThreadSwitcherSelection::Thread {
                metadata: entry.metadata.clone(),
                workspace: entry.workspace.clone(),
            },
            Self::Terminal(entry) => ThreadSwitcherSelection::Terminal {
                metadata: entry.metadata.clone(),
                workspace: entry.workspace.clone(),
            },
        }
    }

    fn element_id(&self) -> SharedString {
        match self {
            Self::Thread(entry) => SharedString::from(format!(
                "thread-switcher-thread-{:?}",
                entry.metadata.thread_id
            )),
            Self::Terminal(entry) => SharedString::from(format!(
                "thread-switcher-terminal-{}",
                entry.metadata.terminal_id
            )),
        }
    }

    fn title(&self) -> SharedString {
        match self {
            Self::Thread(entry) => entry.title.clone(),
            Self::Terminal(entry) => entry.metadata.display_title(),
        }
    }

    fn icon(&self) -> IconName {
        match self {
            Self::Thread(entry) if entry.is_draft => IconName::Circle,
            Self::Thread(entry) => entry.icon,
            Self::Terminal(_) => IconName::Terminal,
        }
    }

    fn icon_from_external_svg(&self) -> Option<SharedString> {
        match self {
            Self::Thread(entry) if entry.is_draft => None,
            Self::Thread(entry) => entry.icon_from_external_svg.clone(),
            Self::Terminal(_) => None,
        }
    }

    fn status(&self) -> AgentThreadStatus {
        match self {
            Self::Thread(entry) => entry.status,
            Self::Terminal(_) => AgentThreadStatus::default(),
        }
    }

    fn project_name(&self) -> Option<SharedString> {
        match self {
            Self::Thread(entry) => entry.project_name.clone(),
            Self::Terminal(entry) => entry.project_name.clone(),
        }
    }

    fn worktrees(&self) -> Vec<ThreadItemWorktreeInfo> {
        match self {
            Self::Thread(entry) => entry.worktrees.clone(),
            Self::Terminal(entry) => entry.worktrees.clone(),
        }
    }

    fn timestamp(&self) -> SharedString {
        match self {
            Self::Thread(entry) => entry.timestamp.clone(),
            Self::Terminal(entry) => entry.timestamp.clone(),
        }
    }

    fn is_draft(&self) -> bool {
        match self {
            Self::Thread(entry) => entry.is_draft,
            Self::Terminal(_) => false,
        }
    }

    fn is_title_generating(&self) -> bool {
        match self {
            Self::Thread(entry) => entry.is_title_generating,
            Self::Terminal(_) => false,
        }
    }

    fn notified(&self) -> bool {
        match self {
            Self::Thread(entry) => entry.notified,
            Self::Terminal(entry) => entry.notified,
        }
    }

    fn diff_stats(&self) -> DiffStats {
        match self {
            Self::Thread(entry) => entry.diff_stats,
            Self::Terminal(_) => DiffStats::default(),
        }
    }

    #[cfg(test)]
    pub fn thread_id(&self) -> Option<agent_ui::ThreadId> {
        match self {
            Self::Thread(entry) => Some(entry.metadata.thread_id),
            Self::Terminal(_) => None,
        }
    }

    #[cfg(test)]
    pub fn terminal_id(&self) -> Option<TerminalId> {
        match self {
            Self::Thread(_) => None,
            Self::Terminal(entry) => Some(entry.metadata.terminal_id),
        }
    }
}

pub(super) enum ThreadSwitcherEvent {
    Preview(ThreadSwitcherSelection),
    Confirmed(ThreadSwitcherSelection),
    Dismissed,
}

pub(crate) struct ThreadSwitcher {
    focus_handle: FocusHandle,
    entries: Vec<ThreadSwitcherEntry>,
    selected_index: usize,
    init_modifiers: Option<Modifiers>,
    scroll_handle: ScrollHandle,
    picker: Option<ThreadPicker>,
}

struct ThreadPicker {
    editor: Entity<Editor>,
    entries: Vec<ThreadSwitcherEntry>,
    current_project: Option<ProjectGroupKey>,
    all_projects: bool,
}

impl ThreadSwitcher {
    pub fn new(
        entries: Vec<ThreadSwitcherEntry>,
        select_last: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let init_modifiers = window.modifiers().modified().then_some(window.modifiers());
        let selected_index = if entries.is_empty() {
            0
        } else if select_last {
            entries.len() - 1
        } else {
            1.min(entries.len().saturating_sub(1))
        };

        if let Some(entry) = entries.get(selected_index) {
            cx.emit(ThreadSwitcherEvent::Preview(entry.selection()));
        }

        let focus_handle = cx.focus_handle();
        cx.on_focus_out(&focus_handle, window, |_this, _event, _window, cx| {
            cx.emit(ThreadSwitcherEvent::Dismissed);
            cx.emit(DismissEvent);
        })
        .detach();

        let scroll_handle = ScrollHandle::new();
        scroll_handle.scroll_to_item(selected_index);

        Self {
            focus_handle,
            entries,
            selected_index,
            init_modifiers,
            scroll_handle,
            picker: None,
        }
    }

    pub fn new_picker(
        entries: Vec<ThreadSwitcherEntry>,
        current_project: Option<ProjectGroupKey>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search conversations…", window, cx);
            editor
        });
        cx.subscribe(&editor, |this, _, event, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) {
                this.filter_picker_entries(cx);
            }
        })
        .detach();
        let focus_handle = cx.focus_handle();
        cx.on_focus_out(&focus_handle, window, |_this, _event, _window, cx| {
            cx.emit(ThreadSwitcherEvent::Dismissed);
            cx.emit(DismissEvent);
        })
        .detach();
        let mut switcher = Self {
            focus_handle,
            entries: Vec::new(),
            selected_index: 0,
            init_modifiers: None,
            scroll_handle: ScrollHandle::new(),
            picker: Some(ThreadPicker {
                editor,
                entries,
                current_project,
                all_projects: false,
            }),
        };
        switcher.filter_picker_entries(cx);
        switcher
    }

    pub fn is_picker(&self) -> bool {
        self.picker.is_some()
    }

    pub fn update_picker_entries(
        &mut self,
        entries: Vec<ThreadSwitcherEntry>,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = &mut self.picker {
            picker.entries = entries;
            self.filter_picker_entries(cx);
        }
    }

    fn filter_picker_entries(&mut self, cx: &mut Context<Self>) {
        let selected_id = self.selected_entry().map(ThreadSwitcherEntry::element_id);
        let Some(picker) = &self.picker else {
            return;
        };
        let query = picker.editor.read(cx).text(cx);
        self.entries = picker
            .entries
            .iter()
            .filter(|entry| {
                (picker.all_projects
                    || entry.project_group_key() == picker.current_project.as_ref())
                    && entry.matches_query(&query)
            })
            .cloned()
            .collect();
        self.entries.sort_by_key(ThreadSwitcherEntry::group);
        self.selected_index = selected_id
            .and_then(|selected_id| {
                self.entries
                    .iter()
                    .position(|entry| entry.element_id() == selected_id)
            })
            .unwrap_or(0);
        self.scroll_handle.scroll_to_item(self.selected_index);
        cx.notify();
    }

    pub(super) fn toggle_scope(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.picker {
            picker.all_projects = !picker.all_projects;
        }
        self.filter_picker_entries(cx);
        window.focus(&self.focus_handle(cx), cx);
    }

    #[cfg(test)]
    pub fn set_query(&mut self, query: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(picker) = &self.picker {
            picker
                .editor
                .update(cx, |editor, cx| editor.set_text(query, window, cx));
        }
        self.filter_picker_entries(cx);
    }

    pub fn selected_entry(&self) -> Option<&ThreadSwitcherEntry> {
        self.entries.get(self.selected_index)
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[ThreadSwitcherEntry] {
        &self.entries
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn cycle_selection(&mut self, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        self.selected_index = (self.selected_index + 1) % self.entries.len();
        self.emit_preview(cx);
    }

    pub fn select_last(&mut self, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = self.entries.len() - 1;
        } else {
            self.selected_index -= 1;
        }
        self.emit_preview(cx);
    }

    fn emit_preview(&mut self, cx: &mut Context<Self>) {
        self.scroll_handle.scroll_to_item(self.selected_index);
        if self.picker.is_none()
            && let Some(entry) = self.entries.get(self.selected_index)
        {
            cx.emit(ThreadSwitcherEvent::Preview(entry.selection()));
        }
        cx.notify();
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.confirm_selected(cx);
    }

    fn confirm_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.entries.get(self.selected_index) {
            cx.emit(ThreadSwitcherEvent::Confirmed(entry.selection()));
        } else {
            cx.emit(ThreadSwitcherEvent::Dismissed);
        }
        cx.emit(DismissEvent);
    }

    fn select_and_confirm(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.entries.len() {
            self.selected_index = index;
            self.confirm_selected(cx);
        }
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.entries.len() || index == self.selected_index {
            return;
        }
        self.selected_index = index;
        self.scroll_handle.scroll_to_item(index);
        self.emit_preview(cx);
        cx.notify();
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        cx.emit(ThreadSwitcherEvent::Dismissed);
        cx.emit(DismissEvent);
    }

    fn toggle(
        &mut self,
        action: &ToggleThreadSwitcher,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if action.select_last {
            self.select_last(cx);
        } else {
            self.cycle_selection(cx);
        }
    }

    fn handle_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(init_modifiers) = self.init_modifiers else {
            return;
        };
        if !event.modified() || !init_modifiers.is_subset_of(event) {
            self.init_modifiers = None;
            if self.entries.is_empty() {
                cx.emit(DismissEvent);
            } else {
                window.dispatch_action(menu::Confirm.boxed_clone(), cx);
            }
        }
    }
}

impl ModalView for ThreadSwitcher {}

impl EventEmitter<DismissEvent> for ThreadSwitcher {}
impl EventEmitter<ThreadSwitcherEvent> for ThreadSwitcher {}

impl Focusable for ThreadSwitcher {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.picker
            .as_ref()
            .map(|picker| picker.editor.focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl Render for ThreadSwitcher {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_index = self.selected_index;
        let query = self
            .picker
            .as_ref()
            .map(|picker| picker.editor.read(cx).text(cx))
            .unwrap_or_default();
        let is_picker = self.is_picker();

        v_flex()
            .key_context("ThreadSwitcher")
            .track_focus(&self.focus_handle)
            .p_1p5()
            .w(rems_from_px(440_f32))
            .elevation_3(cx)
            .on_modifiers_changed(cx.listener(Self::handle_modifiers_changed))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::toggle))
            .on_action(cx.listener(|this, _: &menu::SecondaryConfirm, window, cx| {
                if this.is_picker() {
                    this.toggle_scope(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &menu::SelectNext, _window, cx| this.cycle_selection(cx)),
            )
            .on_action(
                cx.listener(|this, _: &menu::SelectPrevious, _window, cx| this.select_last(cx)),
            )
            .capture_action(
                cx.listener(|this, _: &editor::actions::Newline, _window, cx| {
                    this.confirm_selected(cx)
                }),
            )
            .when_some(self.picker.as_ref(), |this, picker| {
                this.child(
                    h_flex()
                        .gap_2()
                        .p_2()
                        .child(div().flex_1().min_w_0().child(picker.editor.clone()))
                        .child(
                            Button::new(
                                "thread-picker-scope",
                                if picker.all_projects {
                                    "All projects"
                                } else {
                                    "Current project"
                                },
                            )
                            .label_size(LabelSize::Small)
                            .tooltip(|_, cx| {
                                ui::Tooltip::for_action(
                                    "Toggle project scope",
                                    &menu::SecondaryConfirm,
                                    cx,
                                )
                            })
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_scope(window, cx)),
                            ),
                        ),
                )
            })
            .when(is_picker && self.entries.is_empty(), |this| {
                this.child(
                    Label::new(if query.is_empty() {
                        "No conversations in this project"
                    } else {
                        "No matching conversations"
                    })
                    .color(Color::Muted)
                    .size(LabelSize::Small),
                )
            })
            .child(
                v_flex()
                    .id("thread-switcher-list")
                    .gap_0p5()
                    .max_h_128()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .children(self.entries.iter().enumerate().map(|(ix, entry)| {
                        let diff_stats = entry.diff_stats();

                        let item = ThreadItem::new(entry.element_id(), entry.title())
                            .rounded(true)
                            .highlight_positions(if query.is_empty() {
                                Vec::new()
                            } else {
                                fuzzy_match_positions(&query, &entry.title()).unwrap_or_default()
                            })
                            .icon(entry.icon())
                            .when(entry.is_draft(), |this| {
                                this.icon_color(Color::Custom(
                                    cx.theme().colors().icon_muted.opacity(0.2),
                                ))
                            })
                            .status(entry.status())
                            .when_some(entry.icon_from_external_svg(), |this, svg| {
                                this.custom_icon_from_external_svg(svg)
                            })
                            .when_some(entry.project_name(), |this, name| this.project_name(name))
                            .worktrees(entry.worktrees())
                            .timestamp(entry.timestamp())
                            .title_generating(entry.is_title_generating())
                            .notified(entry.notified())
                            .when(diff_stats.lines_added > 0, |this| {
                                this.added(diff_stats.lines_added as usize)
                            })
                            .when(diff_stats.lines_removed > 0, |this| {
                                this.removed(diff_stats.lines_removed as usize)
                            })
                            .selected(ix == selected_index)
                            .base_bg(cx.theme().colors().elevated_surface_background)
                            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                                if *hovered {
                                    this.select_index(ix, cx);
                                }
                            }))
                            // TODO: This is not properly propagating to the tread item.
                            .on_click(cx.listener(
                                move |this, _event: &gpui::ClickEvent, _window, cx| {
                                    this.select_and_confirm(ix, cx);
                                },
                            ))
                            .into_any_element();
                        let group_start = is_picker
                            && (ix == 0
                                || self
                                    .entries
                                    .get(ix - 1)
                                    .is_some_and(|previous| previous.group() != entry.group()));
                        v_flex()
                            .when(group_start, |this| {
                                this.child(
                                    div().px_2().py_1().child(
                                        Label::new(match entry.group() {
                                            0 => "Needs You",
                                            1 => "Running",
                                            _ => "Recent",
                                        })
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                    ),
                                )
                            })
                            .child(item)
                            .into_any_element()
                    })),
            )
            .vertical_scrollbar_for(&self.scroll_handle, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_ui::thread_metadata_store::WorktreePaths;
    use util::path_list::PathList;

    #[test]
    fn picker_groups_statuses_and_searches_worktree_context() {
        let mut entry = ThreadSwitcherEntry::Thread(ThreadSwitcherThreadEntry {
            title: "Review changes".into(),
            icon: IconName::ZedAgent,
            icon_from_external_svg: None,
            status: AgentThreadStatus::WaitingForConfirmation,
            metadata: ThreadMetadata {
                thread_id: agent_ui::ThreadId::new(),
                session_id: None,
                agent_id: agent::ZED_AGENT_ID.clone(),
                title: None,
                title_override: None,
                updated_at: chrono::Utc::now(),
                created_at: None,
                interacted_at: None,
                worktree_paths: WorktreePaths::default(),
                archived: false,
                remote_connection: None,
            },
            workspace: ThreadEntryWorkspace::Closed {
                folder_paths: PathList::default(),
                project_group_key: ProjectGroupKey::default(),
            },
            project_group_key: Some(ProjectGroupKey::default()),
            project_name: Some("Compiler".into()),
            worktrees: vec![ThreadItemWorktreeInfo {
                branch_name: Some("feature/parser".into()),
                full_path: "/worktrees/compiler/parser".into(),
                ..Default::default()
            }],
            diff_stats: DiffStats::default(),
            is_draft: false,
            is_title_generating: false,
            notified: false,
            timestamp: "".into(),
        });
        assert!(entry.matches_query("Review"));
        assert!(entry.matches_query("Compiler"));
        assert!(entry.matches_query("parser"));
        assert!(!entry.matches_query("unrelated"));
        assert_eq!(entry.group(), 0);
        for (status, expected) in [
            (AgentThreadStatus::Error, 0),
            (AgentThreadStatus::Unconfirmed, 0),
            (AgentThreadStatus::Running, 1),
            (AgentThreadStatus::Completed, 2),
        ] {
            if let ThreadSwitcherEntry::Thread(thread) = &mut entry {
                thread.status = status;
            }
            assert_eq!(entry.group(), expected);
        }
    }
}
