use gpui::{Context, Task, WeakEntity, Window, prelude::*};
use settings::Settings;
use twg::{
    TwgSettings,
    bitbucket::{PullRequest, PullRequestQuery, PullRequestRef, PullRequestRole, RepositoryRef},
};
use ui::prelude::*;
use ui::{Banner, Severity};
use util::ResultExt;
use workspace::Workspace;

use crate::pull_request_view::open_pull_request;

pub(super) struct PrList {
    workspace: WeakEntity<Workspace>,
    pub(super) mine: Vec<PullRequest>,
    reviews: Vec<PullRequest>,
    pub(super) branch: Option<String>,
    repository: Option<RepositoryRef>,
    loading: bool,
    error: Option<String>,
    page: u32,
    has_more: bool,
    task: Option<Task<()>>,
}

impl PrList {
    pub(super) fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut list = Self {
            workspace,
            mine: Vec::new(),
            reviews: Vec::new(),
            branch: None,
            repository: None,
            loading: false,
            error: None,
            page: 0,
            has_more: false,
            task: None,
        };
        list.refresh(false, window, cx);
        list
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn for_visual_fixture(workspace: WeakEntity<Workspace>) -> Self {
        let request = |number, title: &str, branch: &str| {
            twg::bitbucket::parse_pull_request(
            serde_json::json!({"id":number,"title":title,"state":"OPEN","author":{"display_name":"Alex Morgan"},
                "source":{"branch":{"name":branch},"commit":{"hash":"abc123"}},
                "destination":{"commit":{"hash":"base123"}},
                "participants":[{"user":{"display_name":"Sam Taylor"},"state":"changes_requested"}],
                "_statuses":[{"state":"SUCCESSFUL"}]}),
            PullRequestRef { workspace:"example".into(),repository:"review-demo".into(),number },
        ).expect("Static PR fixture must parse")
        };
        Self {
            workspace,
            mine: vec![request(
                141,
                "DAT-1206 · Remove duplicate analytics events",
                "DAT-1206",
            )],
            reviews: vec![request(
                148,
                "Preserve late-arrival events",
                "feature/late-arrivals",
            )],
            branch: Some("DAT-1206".into()),
            repository: None,
            loading: false,
            error: None,
            page: 0,
            has_more: false,
            task: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn for_visual_fixture_error(workspace: WeakEntity<Workspace>) -> Self {
        let mut list = Self::for_visual_fixture(workspace);
        list.error = Some("TWG TWG_TOOL_EXECUTION_FAILED: Connection failed: getaddrinfo ENOTFOUND api.atlassian.com. Verify the site name and your network connectivity.".into());
        list.page = 1;
        list.has_more = true;
        list
    }

    pub(super) fn related(&self, key: &str) -> Vec<PullRequestRef> {
        self.mine
            .iter()
            .chain(&self.reviews)
            .filter(|pr| relates_to_ticket(&pr.title, &pr.source.branch.name, key))
            .fold(Vec::new(), |mut references, pr| {
                if !references.contains(&pr.reference) {
                    references.push(pr.reference.clone());
                }
                references
            })
    }

    pub(super) fn update_snapshot(&mut self, request: &PullRequest, cx: &mut Context<Self>) {
        for existing in self.mine.iter_mut().chain(&mut self.reviews) {
            if existing.reference == request.reference {
                *existing = request.clone();
            }
        }
        cx.notify();
    }

    pub(super) fn refresh(&mut self, more: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let result = self.workspace.read_with(cx, |workspace, cx| {
            let repository = workspace.project().read(cx).active_repository(cx)
                .ok_or_else(|| anyhow::anyhow!("Open a Bitbucket repository to load its pull requests."))?;
            let snapshot = repository.read(cx);
            let reference = snapshot.remote_origin_url.as_deref().and_then(|url| RepositoryRef::from_remote(url).ok())
                .or_else(|| snapshot.remote_upstream_url.as_deref().and_then(|url| RepositoryRef::from_remote(url).ok()))
                .ok_or_else(|| anyhow::anyhow!("PR review currently supports Bitbucket Cloud. Open a project with a Bitbucket remote."))?;
            let branch = snapshot.branch.as_ref().map(|branch| branch.name().to_string());
            anyhow::Ok((reference, branch))
        }).and_then(|result| result);
        let (repository, branch) = match result {
            Ok(result) => result,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        if self.repository.as_ref() != Some(&repository) {
            self.mine.clear();
            self.reviews.clear();
            self.page = 0;
        } else if !more {
            self.page = 0;
        }
        self.branch = branch;
        self.repository = Some(repository.clone());
        self.loading = true;
        self.error = None;
        let requested_page = self.page.saturating_add(1);
        let client = TwgSettings::get_global(cx).client();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let account = client.whoami().await?;
                    let query = |role| PullRequestQuery {
                        role: Some(role),
                        account_id: Some(account.account_id.clone()),
                        state: "OPEN".into(),
                        query: None,
                        page: requested_page,
                        limit: 25,
                    };
                    let author_query = query(PullRequestRole::Author);
                    let reviewer_query = query(PullRequestRole::Reviewer);
                    let (mine, reviews) = futures::join!(
                        client.query_pull_requests(&repository, &author_query),
                        client.query_pull_requests(&repository, &reviewer_query)
                    );
                    anyhow::Ok((mine, reviews))
                })
                .await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((mine, reviews)) => {
                        let mut failures = Vec::new();
                        let complete = mine
                            .as_ref()
                            .is_ok_and(|page| !page.partial && page.failures.is_empty())
                            && reviews
                                .as_ref()
                                .is_ok_and(|page| !page.partial && page.failures.is_empty());
                        this.page = completed_page(this.page, requested_page, complete);
                        this.has_more = false;
                        match mine {
                            Ok(page) => {
                                this.has_more |= page.has_more;
                                if page.partial || !page.failures.is_empty() {
                                    failures
                                        .push("Some authored PRs could not be loaded.".to_owned());
                                }
                                merge_page(&mut this.mine, page.values, requested_page == 1);
                            }
                            Err(error) => failures.push(format!("My PRs: {error}")),
                        }
                        match reviews {
                            Ok(page) => {
                                this.has_more |= page.has_more;
                                if page.partial || !page.failures.is_empty() {
                                    failures.push(
                                        "Some review requests could not be loaded.".to_owned(),
                                    );
                                }
                                merge_page(&mut this.reviews, page.values, requested_page == 1);
                            }
                            Err(error) => failures.push(format!("Review requests: {error}")),
                        }
                        this.has_more |= !complete;
                        this.error = (!failures.is_empty()).then(|| failures.join("\n"));
                    }
                    Err(error) => {
                        this.has_more = true;
                        this.error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn group(&self, title: &str, requests: &[PullRequest], cx: &App) -> AnyElement {
        v_flex()
            .gap_1()
            .child(
                Label::new(format!("{title} · {}", requests.len()))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .children(requests.iter().map(|pr| {
                let reference = pr.reference.clone();
                let workspace = self.workspace.clone();
                v_flex()
                    .gap_1()
                    .p_2()
                    .min_w_0()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Button::new(
                            SharedString::from(format!(
                                "pr-{}-{}-{title}",
                                reference.repository, reference.number
                            )),
                            format!("#{}  {}", reference.number, pr.title),
                        )
                        .tab_index(0_isize)
                        .style(ButtonStyle::Subtle)
                        .on_click(move |_, window, cx| {
                            open_pull_request(&workspace, reference.clone(), window, cx)
                        }),
                    )
                    .child(
                        Label::new(pr_summary(pr))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
            }))
            .into_any_element()
    }
}

fn pr_summary(pr: &PullRequest) -> String {
    let mut parts = vec![pr.author.display_name.clone(), pr.state.clone()];
    let approvals = pr
        .participants
        .iter()
        .filter(|participant| participant.approved)
        .count();
    if pr
        .participants
        .iter()
        .any(|participant| participant.state.eq_ignore_ascii_case("changes_requested"))
    {
        parts.push("Changes requested".into());
    } else if approvals > 0 {
        parts.push(format!("{approvals} approvals"));
    }
    let states = pr
        .statuses
        .iter()
        .filter_map(|status| status.get("state").and_then(|state| state.as_str()))
        .collect::<Vec<_>>();
    if states
        .iter()
        .any(|state| state.eq_ignore_ascii_case("FAILED"))
    {
        parts.push("Build failed".into());
    } else if states
        .iter()
        .any(|state| state.eq_ignore_ascii_case("INPROGRESS"))
    {
        parts.push("Build running".into());
    } else if !states.is_empty()
        && pr.failures.is_empty()
        && states
            .iter()
            .all(|state| state.eq_ignore_ascii_case("SUCCESSFUL"))
    {
        parts.push("Build passed".into());
    }
    parts.retain(|part| !part.is_empty());
    parts.join(" · ")
}

fn merge_page(existing: &mut Vec<PullRequest>, incoming: Vec<PullRequest>, replace: bool) {
    if replace {
        existing.clear();
    }
    for request in incoming {
        if let Some(existing) = existing
            .iter_mut()
            .find(|existing| existing.reference == request.reference)
        {
            *existing = request;
        } else {
            existing.push(request);
        }
    }
}

pub(super) fn relates_to_ticket(title: &str, branch: &str, key: &str) -> bool {
    [title, branch].iter().any(|value| {
        value
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
            .any(|word| word.eq_ignore_ascii_case(key))
    })
}

impl Render for PrList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .p_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Label::new("PULL REQUESTS")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        IconButton::new("refresh-prs", IconName::RotateCw)
                            .tab_index(0_isize)
                            .disabled(self.loading)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.refresh(false, window, cx)),
                            ),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    Banner::new()
                        .severity(Severity::Warning)
                        .wrap_content(true)
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    Label::new("Could not refresh pull requests.")
                                        .size(LabelSize::Small),
                                )
                                .when(!self.mine.is_empty() || !self.reviews.is_empty(), |view| {
                                    view.child(
                                        Label::new("Showing available results.")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                }),
                        )
                        .action_slot(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("pr-error-details", "Details")
                                        .tab_index(0_isize)
                                        .on_click(cx.listener(move |_, _, window, cx| {
                                            let answer = window.prompt(
                                                gpui::PromptLevel::Info,
                                                "Pull request refresh failed",
                                                Some(&error),
                                                &["OK"],
                                                cx,
                                            );
                                            cx.spawn(async move |_, _| {
                                                answer.await.log_err();
                                            })
                                            .detach();
                                        })),
                                )
                                .child(
                                    Button::new("retry-prs", "Retry")
                                        .tab_index(0_isize)
                                        .disabled(self.loading)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.refresh(this.has_more, window, cx);
                                        })),
                                ),
                        ),
                )
            })
            .when(self.loading, |view| {
                view.child(Label::new("Loading pull requests…").color(Color::Muted))
            })
            .when_some(self.repository.as_ref(), |view, repository| {
                view.child(
                    Label::new(format!(
                        "{}/{}",
                        repository.workspace, repository.repository
                    ))
                    .color(Color::Muted),
                )
            })
            .when_some(self.branch.clone(), |view, branch| {
                let current = self
                    .mine
                    .iter()
                    .chain(&self.reviews)
                    .find(|request| request.source.branch.name == branch);
                let mut card = v_flex()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().status().created)
                    .rounded_md()
                    .child(Label::new(format!("On branch {branch}")).color(Color::Success));
                if let Some(request) = current {
                    let reference = request.reference.clone();
                    let workspace = self.workspace.clone();
                    card = card
                        .child(
                            Button::new(
                                "current-branch-pr",
                                format!("#{} · {}", reference.number, request.title),
                            )
                            .tab_index(0_isize)
                            .on_click(move |_, window, cx| {
                                open_pull_request(&workspace, reference.clone(), window, cx)
                            }),
                        )
                        .child(
                            Label::new(pr_summary(request))
                                .color(Color::Muted)
                                .size(LabelSize::Small),
                        );
                }
                view.child(card)
            })
            .child(self.group("REVIEW REQUESTED", &self.reviews, cx))
            .child(self.group("MY PRS", &self.mine, cx))
            .when(
                !self.loading
                    && self.error.is_none()
                    && self.mine.is_empty()
                    && self.reviews.is_empty(),
                |view| {
                    view.child(
                        Label::new("No open pull requests for this account in this repository.")
                            .color(Color::Muted),
                    )
                },
            )
            .when(self.has_more && self.error.is_none(), |view| {
                view.child(
                    Button::new("more-prs", "Load 25 more")
                        .tab_index(0_isize)
                        .disabled(self.loading)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.refresh(true, window, cx)),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::relates_to_ticket;

    #[test]
    fn pr_summary_uses_returned_review_and_build_state() {
        let pr = twg::bitbucket::parse_pull_request(
            serde_json::json!({
                "id":1,"title":"Fix events","state":"OPEN","author":{"display_name":"Alex"},
                "participants":[{"user":{"display_name":"Sam"},"state":"changes_requested"}],
                "_statuses":[{"state":"SUCCESSFUL"}]
            }),
            twg::bitbucket::PullRequestRef {
                workspace: "example".into(),
                repository: "repo".into(),
                number: 1,
            },
        )
        .expect("PR");
        let summary = super::pr_summary(&pr);
        assert!(summary.contains("Alex"));
        assert!(summary.contains("Changes requested"));
        assert!(summary.contains("Build passed"));
    }

    #[test]
    fn ticket_links_require_complete_keys() {
        assert!(relates_to_ticket(
            "[DAT-1206] Fix",
            "feature/other",
            "DAT-1206"
        ));
        assert!(relates_to_ticket("Fix", "feature/DAT-1206", "DAT-1206"));
        assert!(!relates_to_ticket("DAT-12060 fix", "other", "DAT-1206"));
    }
}

fn completed_page(previous: u32, requested: u32, complete: bool) -> u32 {
    if complete { requested } else { previous }
}

#[cfg(test)]
mod pagination_tests {
    use super::completed_page;

    #[test]
    fn failed_or_partial_groups_retry_the_same_page() {
        assert_eq!(completed_page(1, 2, false), 1);
        assert_eq!(completed_page(1, 2, true), 2);
        assert_eq!(completed_page(0, 1, false), 0);
    }
}
