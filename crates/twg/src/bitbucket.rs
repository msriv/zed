use crate::Client;
use anyhow::{Context, Result, ensure};
use diffy::{Line, Patch};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRef {
    pub workspace: String,
    pub repository: String,
}

impl RepositoryRef {
    pub fn from_remote(remote: &str) -> Result<Self> {
        let path = if let Some(path) = remote.strip_prefix("git@bitbucket.org:") {
            path.to_owned()
        } else {
            let url = url::Url::parse(remote).context("Enter a Bitbucket repository URL")?;
            ensure!(
                matches!(url.scheme(), "https" | "ssh")
                    && url.host_str() == Some("bitbucket.org")
                    && url.password().is_none()
                    && url.port().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "Only Bitbucket Cloud repository URLs are supported"
            );
            url.path().trim_matches('/').to_owned()
        };
        let parts = path.trim_end_matches('/').split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() == 2,
            "Expected a Bitbucket workspace/repository URL"
        );
        let reference = Self {
            workspace: parts[0].to_owned(),
            repository: parts[1].strip_suffix(".git").unwrap_or(parts[1]).to_owned(),
        };
        reference.validate()?;
        Ok(reference)
    }

    pub fn validate(&self) -> Result<()> {
        for slug in [&self.workspace, &self.repository] {
            ensure!(
                !slug.is_empty()
                    && !slug.starts_with(['-', '.'])
                    && slug
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
                "Invalid Bitbucket workspace or repository slug"
            );
        }
        Ok(())
    }

    fn scope_arguments(&self) -> Result<Vec<String>> {
        self.validate()?;
        Ok(vec![
            "--workspace".into(),
            self.workspace.clone(),
            "--repo".into(),
            self.repository.clone(),
        ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub workspace: String,
    pub repository: String,
    pub number: u64,
}

impl PullRequestRef {
    pub fn from_url(value: &str) -> Result<Self> {
        let url = url::Url::parse(value).context("Enter a Bitbucket pull request URL")?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("bitbucket.org")
                && url.port().is_none()
                && url.username().is_empty(),
            "Only Bitbucket Cloud pull request URLs are supported"
        );
        let parts = url.path().trim_matches('/').split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() >= 4 && parts[2] == "pull-requests",
            "Expected a Bitbucket pull request URL"
        );
        let reference = Self {
            workspace: parts[0].into(),
            repository: parts[1].into(),
            number: parts[3].parse().context("Invalid pull request number")?,
        };
        reference.validate()?;
        Ok(reference)
    }

    pub fn repository(&self) -> RepositoryRef {
        RepositoryRef {
            workspace: self.workspace.clone(),
            repository: self.repository.clone(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.repository().validate()?;
        ensure!(self.number > 0, "Invalid pull request number");
        Ok(())
    }

    pub fn url(&self) -> String {
        format!(
            "https://bitbucket.org/{}/{}/pull-requests/{}",
            self.workspace, self.repository, self.number
        )
    }

    fn arguments(&self, action: &str) -> Result<Vec<String>> {
        self.validate()?;
        let mut arguments = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            action.into(),
            self.number.to_string(),
        ];
        arguments.extend(self.repository().scope_arguments()?);
        Ok(arguments)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BitbucketUser {
    #[serde(default, alias = "accountId")]
    pub account_id: String,
    #[serde(default)]
    pub uuid: String,
    #[serde(default, alias = "displayName")]
    pub display_name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Branch {
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Commit {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub author: Value,
    #[serde(default)]
    pub links: Value,
}

impl Commit {
    pub fn browser_url(
        &self,
        source_repository: &Repository,
        fallback_repository: &RepositoryRef,
    ) -> Result<String> {
        ensure!(
            !self.hash.is_empty() && self.hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid Bitbucket commit hash"
        );
        ensure!(
            self.links.is_null() || self.links.is_object(),
            "Invalid Bitbucket commit links"
        );
        if let Some(html) = self.links.get("html") {
            let href = html
                .get("href")
                .and_then(Value::as_str)
                .context("Invalid Bitbucket commit HTML link")?;
            let url = url::Url::parse(href).context("Invalid Bitbucket commit URL")?;
            ensure!(
                href.starts_with("https://bitbucket.org/")
                    && url.scheme() == "https"
                    && url.host_str() == Some("bitbucket.org")
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.port().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "Only Bitbucket Cloud commit URLs are supported"
            );
            let parts = url
                .path()
                .trim_start_matches('/')
                .split('/')
                .collect::<Vec<_>>();
            ensure!(
                parts.len() == 4 && parts[2] == "commits" && parts[3] == self.hash,
                "Expected a URL for this Bitbucket commit"
            );
            RepositoryRef {
                workspace: parts[0].into(),
                repository: parts[1].into(),
            }
            .validate()?;
            return Ok(url.into());
        }
        let repository = if source_repository.full_name.is_empty() {
            fallback_repository.clone()
        } else {
            let (workspace, repository) = source_repository
                .full_name
                .split_once('/')
                .context("Invalid Bitbucket source repository")?;
            RepositoryRef {
                workspace: workspace.into(),
                repository: repository.into(),
            }
        };
        repository.validate()?;
        Ok(format!(
            "https://bitbucket.org/{}/{}/commits/{}",
            repository.workspace, repository.repository, self.hash
        ))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Repository {
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub links: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PullRequestBranch {
    #[serde(default)]
    pub branch: Branch,
    #[serde(default)]
    pub commit: Commit,
    #[serde(default)]
    pub repository: Repository,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Participant {
    #[serde(default)]
    pub user: BitbucketUser,
    #[serde(default)]
    pub approved: bool,
    #[serde(default, deserialize_with = "deserialize_participant_state")]
    pub state: String,
    #[serde(default)]
    pub role: String,
}

fn deserialize_participant_state<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CommentParent {
    pub id: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CommentContent {
    #[serde(default)]
    pub raw: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineLocation {
    pub path: String,
    #[serde(default)]
    pub from: Option<u32>,
    #[serde(default)]
    pub to: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: u64,
    #[serde(default)]
    pub user: BitbucketUser,
    #[serde(default)]
    pub content: CommentContent,
    #[serde(default)]
    pub parent: Option<CommentParent>,
    #[serde(default)]
    pub inline: Option<InlineLocation>,
    #[serde(
        default,
        alias = "resolution",
        deserialize_with = "deserialize_resolution"
    )]
    pub resolved: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub outdated: bool,
}

fn deserialize_resolution<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<bool, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Bool(resolved) => Ok(resolved),
        Value::Null => Ok(false),
        Value::Object(_) => Ok(true),
        _ => Err(serde::de::Error::custom(
            "Invalid Bitbucket comment resolution",
        )),
    }
}

#[derive(Clone, Debug)]
pub struct PullRequest {
    pub reference: PullRequestRef,
    pub id: u64,
    pub title: String,
    pub description: String,
    pub state: String,
    pub url: String,
    pub author: BitbucketUser,
    pub source: PullRequestBranch,
    pub destination: PullRequestBranch,
    pub participants: Vec<Participant>,
    pub reviewers: Vec<BitbucketUser>,
    pub comments: Vec<ReviewComment>,
    pub commits: Vec<Commit>,
    pub statuses: Vec<Value>,
    pub failures: Vec<Value>,
}

impl PullRequest {
    pub fn revision(&self) -> String {
        format!(
            "{}:{}",
            self.source.commit.hash, self.destination.commit.hash
        )
    }
}

#[derive(Clone, Debug)]
pub struct Page<T> {
    pub values: Vec<T>,
    pub partial: bool,
    pub has_more: bool,
    pub warnings: Vec<Value>,
    pub failures: Vec<Value>,
}

#[derive(Clone, Copy, Debug)]
pub enum PullRequestRole {
    Author,
    Reviewer,
}

#[derive(Clone, Debug)]
pub struct PullRequestQuery {
    pub role: Option<PullRequestRole>,
    pub account_id: Option<String>,
    pub state: String,
    pub query: Option<String>,
    pub page: u32,
    pub limit: u32,
}

impl Default for PullRequestQuery {
    fn default() -> Self {
        Self {
            role: None,
            account_id: None,
            state: "OPEN".into(),
            query: None,
            page: 1,
            limit: 25,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ReviewAction {
    Approve,
    RequestChanges,
}

#[derive(Clone, Debug)]
pub struct PrWriteResult {
    pub pull_request: Option<PullRequest>,
    pub verification_error: Option<String>,
}

fn value_list(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(values)) => values.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(value) if value.get("values").is_some() => value_list(value.get("values")),
        Some(value) => vec![value.clone()],
    }
}

fn parse_page<T>(data: Value, mut parse: impl FnMut(Value) -> Result<T>) -> Result<Page<T>> {
    let partial = data
        .get("partial")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let failures = value_list(data.get("failures"));
    let warnings = value_list(data.get("warnings"));
    let has_more = ["hasNextPage", "hasMore"]
        .iter()
        .any(|key| data.get(key).and_then(Value::as_bool) == Some(true))
        || data.get("next").is_some_and(|next| !next.is_null());
    let rows = data.get("values").cloned().unwrap_or(data);
    let rows = rows
        .as_array()
        .context("Unexpected Bitbucket list response")?;
    Ok(Page {
        values: rows
            .iter()
            .cloned()
            .map(&mut parse)
            .collect::<Result<_>>()?,
        partial: partial || !failures.is_empty(),
        has_more,
        warnings,
        failures,
    })
}

pub fn parse_pull_request(data: Value, reference: PullRequestRef) -> Result<PullRequest> {
    #[derive(Deserialize)]
    struct Native {
        id: u64,
        title: String,
        #[serde(default)]
        description: String,
        state: String,
        #[serde(default)]
        links: Value,
        #[serde(default)]
        author: BitbucketUser,
        #[serde(default)]
        source: PullRequestBranch,
        #[serde(default)]
        destination: PullRequestBranch,
        #[serde(default)]
        participants: Vec<Participant>,
        #[serde(default)]
        reviewers: Vec<BitbucketUser>,
        #[serde(default, rename = "_comments", alias = "comments")]
        comments: Value,
        #[serde(default, rename = "_commits", alias = "commits")]
        commits: Value,
        #[serde(default, rename = "_statuses", alias = "statuses")]
        statuses: Value,
        #[serde(default, rename = "_failures", alias = "failures")]
        failures: Value,
    }
    reference.validate()?;
    let native: Native =
        serde_json::from_value(data).context("Unexpected Bitbucket pull request response")?;
    ensure!(
        native.id == reference.number,
        "Bitbucket returned a different pull request"
    );
    let comments = value_list(Some(&native.comments))
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()
        .context("Invalid Bitbucket comments")?;
    let commits = value_list(Some(&native.commits))
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()
        .context("Invalid Bitbucket commits")?;
    Ok(PullRequest {
        url: native
            .links
            .pointer("/html/href")
            .and_then(Value::as_str)
            .unwrap_or(&reference.url())
            .to_owned(),
        reference,
        id: native.id,
        title: native.title,
        description: native.description,
        state: native.state,
        author: native.author,
        source: native.source,
        destination: native.destination,
        participants: native.participants,
        reviewers: native.reviewers,
        comments,
        commits,
        statuses: value_list(Some(&native.statuses)),
        failures: value_list(Some(&native.failures)),
    })
}

impl Client {
    pub async fn query_pull_requests(
        &self,
        repository: &RepositoryRef,
        query: &PullRequestQuery,
    ) -> Result<Page<PullRequest>> {
        let mut arguments = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            "query".into(),
            "--scope".into(),
            "repo".into(),
        ];
        arguments.extend(repository.scope_arguments()?);
        ensure!(
            matches!(
                query.state.as_str(),
                "OPEN" | "MERGED" | "DECLINED" | "SUPERSEDED"
            ),
            "Unsupported Bitbucket pull request state"
        );
        arguments.extend([
            "--state".into(),
            query.state.clone(),
            "--limit".into(),
            query.limit.clamp(1, 100).to_string(),
            "--page".into(),
            query.page.max(1).to_string(),
        ]);
        if let Some(role) = query.role {
            let account_id = query
                .account_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .context("Choose an account for the author or reviewer filter")?;
            arguments.extend([
                match role {
                    PullRequestRole::Author => "--author",
                    PullRequestRole::Reviewer => "--reviewer",
                }
                .into(),
                account_id.into(),
            ]);
        }
        if let Some(query) = &query.query {
            arguments.extend(["--query".into(), query.clone()]);
        }
        parse_page(self.run_native_json(arguments).await?, |value| {
            let number = value
                .get("id")
                .and_then(Value::as_u64)
                .context("Pull request is missing its number")?;
            parse_pull_request(
                value,
                PullRequestRef {
                    workspace: repository.workspace.clone(),
                    repository: repository.repository.clone(),
                    number,
                },
            )
        })
    }

    pub async fn get_pull_request(&self, reference: &PullRequestRef) -> Result<PullRequest> {
        let mut arguments = reference.arguments("get")?;
        arguments.push("--full".into());
        parse_pull_request(self.run_native_json(arguments).await?, reference.clone())
    }

    pub async fn pull_request_comments(
        &self,
        reference: &PullRequestRef,
    ) -> Result<Page<ReviewComment>> {
        let mut arguments = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            "comment".into(),
            "query".into(),
            reference.number.to_string(),
        ];
        reference.validate()?;
        arguments.extend(reference.repository().scope_arguments()?);
        arguments.extend(["--limit".into(), "50".into()]);
        parse_page(self.run_native_json(arguments).await?, |value| {
            Ok(serde_json::from_value(value)?)
        })
    }

    pub async fn pull_request_commits(&self, reference: &PullRequestRef) -> Result<Page<Commit>> {
        let mut arguments = reference.arguments("commits")?;
        arguments.extend(["--limit".into(), "50".into()]);
        parse_page(self.run_native_json(arguments).await?, |value| {
            Ok(serde_json::from_value(value)?)
        })
    }

    pub async fn pull_request_comment(
        &self,
        reference: &PullRequestRef,
        comment_id: u64,
    ) -> Result<ReviewComment> {
        reference.validate()?;
        ensure!(comment_id > 0, "Invalid comment number");
        let mut arguments = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            "comment".into(),
            "get".into(),
            reference.number.to_string(),
            "--comment".into(),
            comment_id.to_string(),
        ];
        arguments.extend(reference.repository().scope_arguments()?);
        serde_json::from_value(self.run_native_json(arguments).await?)
            .context("Unexpected Bitbucket comment response")
    }

    pub async fn pull_request_diffstat(&self, reference: &PullRequestRef) -> Result<Page<Value>> {
        let mut arguments = reference.arguments("diffstat")?;
        arguments.extend(["--limit".into(), "100".into()]);
        parse_page(self.run_native_json(arguments).await?, Ok)
    }

    pub async fn pull_request_diff(&self, reference: &PullRequestRef) -> Result<PullRequestDiff> {
        let data = self.run_native_json(reference.arguments("diff")?).await?;
        PullRequestDiff::from_response(&data)
    }

    async fn preflight_pull_request(
        &self,
        reference: &PullRequestRef,
        expected_revision: &str,
    ) -> Result<PullRequest> {
        let current = self.get_pull_request(reference).await?;
        validate_revision(&current, expected_revision)?;
        Ok(current)
    }

    async fn verify_pull_request_write(
        &self,
        reference: &PullRequestRef,
        expected_revision: &str,
    ) -> PrWriteResult {
        match self.get_pull_request(reference).await {
            Ok(pull_request) => {
                let verification_error = validate_revision(&pull_request, expected_revision).err().map(|error| {
                    format!("Write succeeded, but the pull request changed during submission: {error}. The action may apply to a newer revision. Refresh before retrying.")
                });
                PrWriteResult {
                    pull_request: Some(pull_request),
                    verification_error,
                }
            }
            Err(error) => PrWriteResult {
                pull_request: None,
                verification_error: Some(format!(
                    "Write succeeded, but verification failed: {error}. Refresh before retrying."
                )),
            },
        }
    }

    pub async fn review_pull_request(
        &self,
        reference: &PullRequestRef,
        expected_revision: &str,
        action: ReviewAction,
    ) -> Result<PrWriteResult> {
        self.preflight_pull_request(reference, expected_revision)
            .await?;
        let response = self
            .run_native_json(reference.arguments(match action {
                ReviewAction::Approve => "approve",
                ReviewAction::RequestChanges => "request-changes",
            })?)
            .await?;
        let mut result = self
            .verify_pull_request_write(reference, expected_revision)
            .await;
        if let Some(pull_request) = &result.pull_request {
            let actor: Option<BitbucketUser> = response
                .get("user")
                .or_else(|| response.pointer("/participant/user"))
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            let verified = actor.as_ref().is_some_and(|actor| {
                pull_request.participants.iter().any(|participant| {
                    let same_user = (!actor.account_id.is_empty()
                        && actor.account_id == participant.user.account_id)
                        || (!actor.uuid.is_empty() && actor.uuid == participant.user.uuid);
                    same_user
                        && match action {
                            ReviewAction::Approve => participant.approved,
                            ReviewAction::RequestChanges => {
                                participant.state.eq_ignore_ascii_case("changes_requested")
                            }
                        }
                })
            });
            if !verified && result.verification_error.is_none() {
                result.verification_error = Some("Write succeeded, but the current participant state could not be verified. Refresh before retrying.".into());
            }
        }
        Ok(result)
    }

    pub async fn comment_pull_request(
        &self,
        reference: &PullRequestRef,
        expected_revision: &str,
        body: &str,
        reply_to: Option<u64>,
        anchor: Option<&DiffAnchor>,
    ) -> Result<PrWriteResult> {
        ensure!(!body.trim().is_empty(), "Enter a review comment");
        ensure!(
            reply_to.is_none() || anchor.is_none(),
            "A reply inherits its thread's location"
        );
        self.preflight_pull_request(reference, expected_revision)
            .await?;
        if let Some(parent) = reply_to {
            let comment = self.pull_request_comment(reference, parent).await?;
            ensure!(
                !comment.deleted && !comment.outdated,
                "The comment is unavailable or outdated; refresh the thread"
            );
        }
        if let Some(anchor) = anchor {
            let diff = self.pull_request_diff(reference).await?;
            ensure!(
                diff.contains_anchor(anchor),
                "This line is not part of the current diff"
            );
            self.preflight_pull_request(reference, expected_revision)
                .await?;
        }
        let arguments = comment_arguments(reference, body, reply_to, anchor)?;
        let response = self.run_native_json(arguments).await?;
        let mut result = self
            .verify_pull_request_write(reference, expected_revision)
            .await;
        let verification = async {
            let id = response
                .get("id")
                .and_then(Value::as_u64)
                .context("The comment write did not return its native ID")?;
            let comment = self.pull_request_comment(reference, id).await?;
            ensure!(
                comment.content.raw == body
                    && comment.parent.as_ref().map(|parent| parent.id) == reply_to
                    && !comment.deleted,
                "The saved comment differs from the requested comment"
            );
            if let Some(anchor) = anchor {
                ensure!(
                    comment
                        .inline
                        .as_ref()
                        .is_some_and(|inline| inline.path == anchor.path
                            && match anchor.side {
                                DiffSide::Old => inline.from == Some(anchor.line),
                                DiffSide::New => inline.to == Some(anchor.line),
                            }),
                    "The saved inline anchor differs from the requested line"
                );
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = verification {
            result.verification_error.get_or_insert_with(|| format!(
                "Write succeeded, but comment verification failed: {error}. Refresh before retrying."
            ));
        }
        Ok(result)
    }

    pub async fn resolve_pull_request_comment(
        &self,
        reference: &PullRequestRef,
        expected_revision: &str,
        comment_id: u64,
    ) -> Result<PrWriteResult> {
        self.preflight_pull_request(reference, expected_revision)
            .await?;
        let comment = self.pull_request_comment(reference, comment_id).await?;
        ensure!(
            !comment.deleted && !comment.outdated && comment.parent.is_none(),
            "The thread is unavailable or outdated; refresh before resolving"
        );
        let mut arguments = vec![
            "bitbucket".into(),
            "pull-requests".into(),
            "comment".into(),
            "resolve".into(),
            "--pull-request".into(),
            reference.number.to_string(),
            "--comment".into(),
            comment_id.to_string(),
        ];
        arguments.extend(reference.repository().scope_arguments()?);
        self.run_native_json(arguments).await?;
        let mut result = self
            .verify_pull_request_write(reference, expected_revision)
            .await;
        match self.pull_request_comment(reference, comment_id).await {
            Ok(comment) if comment.resolved => {}
            Ok(_) => {
                result.verification_error.get_or_insert_with(|| "Write succeeded, but the thread is still reported unresolved. Refresh before retrying.".into());
            }
            Err(error) => {
                result.verification_error.get_or_insert_with(|| format!("Write succeeded, but thread verification failed: {error}. Refresh before retrying."));
            }
        }
        Ok(result)
    }
}

fn validate_revision(current: &PullRequest, expected_revision: &str) -> Result<()> {
    ensure!(
        current.state == "OPEN",
        "This pull request is no longer open; refresh before reviewing"
    );
    ensure!(
        !current.source.commit.hash.is_empty()
            && !current.destination.commit.hash.is_empty()
            && current.revision() == expected_revision,
        "The pull request changed; refresh and review the current revision before submitting"
    );
    Ok(())
}

fn comment_arguments(
    reference: &PullRequestRef,
    body: &str,
    reply_to: Option<u64>,
    anchor: Option<&DiffAnchor>,
) -> Result<Vec<String>> {
    reference.validate()?;
    ensure!(!body.trim().is_empty(), "Enter a review comment");
    ensure!(
        reply_to.is_none() || anchor.is_none(),
        "A reply inherits its thread's location"
    );
    let mut arguments = vec![
        "bitbucket".into(),
        "pull-requests".into(),
        "comment".into(),
        "create".into(),
        "--pull-request".into(),
        reference.number.to_string(),
        "--text".into(),
        body.into(),
    ];
    arguments.extend(reference.repository().scope_arguments()?);
    if let Some(parent) = reply_to {
        ensure!(parent > 0, "Invalid parent comment");
        arguments.extend(["--reply-to".into(), parent.to_string()]);
    }
    if let Some(anchor) = anchor {
        validate_path(&anchor.path)?;
        ensure!(anchor.line > 0, "Invalid inline comment line");
        arguments.extend([
            "--path".into(),
            anchor.path.clone(),
            match anchor.side {
                DiffSide::Old => "--from-line",
                DiffSide::New => "--line",
            }
            .into(),
            anchor.line.to_string(),
        ]);
    }
    Ok(arguments)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSide {
    Old,
    New,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffAnchor {
    pub path: String,
    pub side: DiffSide,
    pub line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Deleted,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug)]
pub struct DiffFile {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
}

impl DiffFile {
    pub fn anchor(&self, line: &DiffLine, side: DiffSide) -> Option<DiffAnchor> {
        let (path, number) = match side {
            DiffSide::Old => (self.old_path.as_ref()?, line.old_line?),
            DiffSide::New => (self.new_path.as_ref()?, line.new_line?),
        };
        Some(DiffAnchor {
            path: path.clone(),
            side,
            line: number,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PullRequestDiff {
    pub text: String,
    pub files: Vec<DiffFile>,
}

impl PullRequestDiff {
    fn from_response(data: &Value) -> Result<Self> {
        let text = data
            .as_str()
            .or_else(|| data.get("content").and_then(Value::as_str))
            .or_else(|| data.get("diff").and_then(Value::as_str))
            .context("Unexpected Bitbucket diff response")?;
        Self::parse(text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        if text.is_empty() {
            return Ok(Self {
                text: String::new(),
                files: Vec::new(),
            });
        }
        ensure!(
            text.starts_with("diff --git ") || text.starts_with("--- "),
            "Unsupported Bitbucket diff format"
        );
        let boundaries = std::iter::once(0)
            .chain(
                text.match_indices("\ndiff --git ")
                    .map(|(offset, _)| offset + 1),
            )
            .chain(std::iter::once(text.len()))
            .collect::<Vec<_>>();
        let mut files = Vec::new();
        for offsets in boundaries.windows(2) {
            let section = &text[offsets[0]..offsets[1]];
            check_coordinates(section)?;
            let normalized = normalize_patch_headers(section)?;
            let patch = Patch::from_str(&normalized).context("Invalid Bitbucket patch")?;
            let binary = section
                .lines()
                .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch");
            let mut old_path = patch
                .original()
                .map(|path| patch_path(path, "a/"))
                .transpose()?
                .flatten();
            let mut new_path = patch
                .modified()
                .map(|path| patch_path(path, "b/"))
                .transpose()?
                .flatten();
            if patch.original().is_none() && patch.modified().is_none() {
                let header = section
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("diff --git "))
                    .context("Missing diff filenames")?;
                let (old, new) = if header.starts_with('"') {
                    let end = quoted_path_end(header)?;
                    (
                        decode_git_path(&header[..end])?,
                        decode_git_path(header[end..].trim_start())?,
                    )
                } else {
                    if let Some((old, new)) = header.split_once(" \"b/") {
                        (old.to_owned(), decode_git_path(&format!("\"b/{new}"))?)
                    } else {
                        let (old, new) =
                            header.split_once(" b/").context("Missing diff filenames")?;
                        (old.to_owned(), format!("b/{new}"))
                    }
                };
                old_path = patch_path(&old, "a/")?;
                new_path = patch_path(&new, "b/")?;
                if section
                    .lines()
                    .any(|line| line.starts_with("new file mode "))
                {
                    old_path = None;
                }
                if section
                    .lines()
                    .any(|line| line.starts_with("deleted file mode "))
                {
                    new_path = None;
                }
                for line in section.lines() {
                    if let Some(path) = line.strip_prefix("rename from ") {
                        old_path = patch_path(&decode_git_path(path)?, "")?;
                    }
                    if let Some(path) = line.strip_prefix("rename to ") {
                        new_path = patch_path(&decode_git_path(path)?, "")?;
                    }
                }
            }
            let mut hunks = Vec::new();
            for hunk in patch.hunks() {
                let old_start = u32::try_from(hunk.old_range().start())?;
                let new_start = u32::try_from(hunk.new_range().start())?;
                let (mut old_line, mut new_line) = (old_start, new_start);
                let mut lines = Vec::new();
                for line in hunk.lines() {
                    let (kind, text, old, new) = match line {
                        Line::Context(text) => {
                            (DiffLineKind::Context, *text, Some(old_line), Some(new_line))
                        }
                        Line::Delete(text) => (DiffLineKind::Deleted, *text, Some(old_line), None),
                        Line::Insert(text) => (DiffLineKind::Added, *text, None, Some(new_line)),
                    };
                    if old.is_some() {
                        old_line = old_line.checked_add(1).context("Diff line overflow")?;
                    }
                    if new.is_some() {
                        new_line = new_line.checked_add(1).context("Diff line overflow")?;
                    }
                    lines.push(DiffLine {
                        kind,
                        text: text.to_owned(),
                        old_line: old,
                        new_line: new,
                    });
                }
                hunks.push(DiffHunk {
                    old_start,
                    new_start,
                    lines,
                });
            }
            files.push(DiffFile {
                old_path,
                new_path,
                binary,
                hunks,
            });
        }
        Ok(Self {
            text: text.into(),
            files,
        })
    }

    pub fn contains_anchor(&self, anchor: &DiffAnchor) -> bool {
        anchor.line > 0
            && self.files.iter().filter(|file| !file.binary).any(|file| {
                file.hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .any(|line| file.anchor(line, anchor.side).as_ref() == Some(anchor))
            })
    }
}

fn validate_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.starts_with(['/', '\\', '-'])
            && !path.contains(['\0', '\n', '\r', '\\'])
            && !path.split('/').any(|part| matches!(part, ".." | "." | "")),
        "Unsafe repository-relative diff path"
    );
    Ok(())
}

fn patch_path(path: &str, prefix: &str) -> Result<Option<String>> {
    if path == "/dev/null" {
        return Ok(None);
    }
    let path = path.strip_prefix(prefix).unwrap_or(path);
    validate_path(path)?;
    Ok(Some(path.into()))
}

fn quoted_path_end(path: &str) -> Result<usize> {
    let mut escaped = false;
    for (index, byte) in path.bytes().enumerate().skip(1) {
        if !escaped && byte == b'"' {
            return Ok(index + 1);
        }
        escaped = !escaped && byte == b'\\';
    }
    anyhow::bail!("Unterminated Git filename")
}

fn decode_git_path(path: &str) -> Result<String> {
    if !path.starts_with('"') {
        return Ok(path.to_owned());
    }
    ensure!(
        quoted_path_end(path)? == path.len(),
        "Invalid quoted Git filename"
    );
    let mut bytes = path[1..path.len() - 1].bytes();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let byte = match bytes.next().context("Incomplete Git filename escape")? {
            digit @ b'0'..=b'7' => {
                let mut value = digit - b'0';
                for _ in 0..2 {
                    let digit = bytes
                        .next()
                        .filter(|digit| matches!(digit, b'0'..=b'7'))
                        .context("Invalid Git filename octal escape")?
                        - b'0';
                    value = value
                        .checked_mul(8)
                        .and_then(|value| value.checked_add(digit))
                        .context("Git filename escape overflow")?;
                }
                value
            }
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            byte @ (b'\\' | b'"') => byte,
            _ => anyhow::bail!("Unsupported Git filename escape"),
        };
        decoded.push(byte);
    }
    String::from_utf8(decoded).context("Git filename is not UTF-8")
}

fn normalize_patch_headers(section: &str) -> Result<String> {
    // diffy does not decode Git's octal filename escapes; normalize only headers.
    let header_end = section
        .find("\n@@ ")
        .map_or(section.len(), |index| index + 1);
    let (headers, hunks) = section.split_at(header_end);
    let mut normalized = String::with_capacity(section.len());
    for line in headers.split_inclusive('\n') {
        if let Some((prefix, path)) = line
            .split_once(' ')
            .filter(|(prefix, path)| matches!(*prefix, "---" | "+++") && path.starts_with('"'))
        {
            let end = quoted_path_end(path)?;
            normalized.push_str(prefix);
            normalized.push(' ');
            normalized.push_str(&serde_json::to_string(&decode_git_path(&path[..end])?)?);
            normalized.push_str(&path[end..]);
        } else {
            normalized.push_str(line);
        }
    }
    normalized.push_str(hunks);
    Ok(normalized)
}

fn check_coordinates(section: &str) -> Result<()> {
    // Bound coordinates before diffy adds them while validating hunk ordering.
    for line in section.lines().filter(|line| line.starts_with("@@ ")) {
        for range in line.split_whitespace().skip(1).take(2) {
            let range = range.trim_start_matches(['-', '+']);
            let (start, count) = range.split_once(',').unwrap_or((range, "1"));
            let start = start.parse::<u32>().context("Invalid hunk start")?;
            let count = count.parse::<u32>().context("Invalid hunk length")?;
            ensure!(start > 0 || count == 0, "Nonempty hunk starts at zero");
            start
                .checked_add(count)
                .context("Hunk coordinate overflow")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commit_browser_url_prefers_native_html_link() {
        let commit: Commit = serde_json::from_value(json!({
            "hash": "abc123",
            "links": {"html": {"href": "https://bitbucket.org/native/repo/commits/abc123"}}
        }))
        .expect("native commit");
        let fallback = RepositoryRef {
            workspace: "team".into(),
            repository: "repo".into(),
        };
        assert_eq!(
            commit
                .browser_url(&Repository::default(), &fallback)
                .unwrap(),
            "https://bitbucket.org/native/repo/commits/abc123"
        );
    }

    #[test]
    fn commit_browser_url_uses_source_repository_or_pr_fallback() {
        let commit: Commit = serde_json::from_value(json!({"hash": "abc123"})).unwrap();
        let fallback = RepositoryRef {
            workspace: "team".into(),
            repository: "repo".into(),
        };
        for (name, expected) in [
            ("", "team/repo"),
            ("team/repo", "team/repo"),
            ("fork/source", "fork/source"),
        ] {
            let source = Repository {
                full_name: name.into(),
                ..Default::default()
            };
            assert_eq!(
                commit.browser_url(&source, &fallback).unwrap(),
                format!("https://bitbucket.org/{expected}/commits/abc123")
            );
        }
        for name in [
            "../repo",
            "team/repo/extra",
            "team/repo?query",
            "https://evil.example/team/repo",
        ] {
            let source = Repository {
                full_name: name.into(),
                ..Default::default()
            };
            assert!(commit.browser_url(&source, &fallback).is_err(), "{name}");
        }
        let invalid = RepositoryRef {
            workspace: "..".into(),
            repository: "repo".into(),
        };
        assert!(
            commit
                .browser_url(&Repository::default(), &invalid)
                .is_err()
        );
    }

    #[test]
    fn commit_browser_url_rejects_unsafe_or_malformed_metadata() {
        let fallback = RepositoryRef {
            workspace: "team".into(),
            repository: "repo".into(),
        };
        for href in [
            "http://bitbucket.org/team/repo/commits/abc123",
            "https://evil.example/team/repo/commits/abc123",
            "javascript:alert(1)",
            "https://user@bitbucket.org/team/repo/commits/abc123",
            "https://user:pass@bitbucket.org/team/repo/commits/abc123",
            "https://bitbucket.org:443/team/repo/commits/abc123",
            "https://bitbucket.org:8443/team/repo/commits/abc123",
            "https://bitbucket.org/team/repo/commits/def456",
            "https://bitbucket.org/team/repo/commits/abc123/diff",
            "https://bitbucket.org/team/repo/commits/abc123?token=secret",
            "https://bitbucket.org/team/repo/commits/abc123#fragment",
        ] {
            let commit: Commit = serde_json::from_value(
                json!({"hash": "abc123", "links": {"html": {"href": href}}}),
            )
            .unwrap();
            assert!(
                commit
                    .browser_url(&Repository::default(), &fallback)
                    .is_err(),
                "{href}"
            );
        }
        for links in [
            json!({"html": {}}),
            json!({"html": {"href": 42}}),
            json!({"html": null}),
            json!("invalid"),
        ] {
            let commit: Commit =
                serde_json::from_value(json!({"hash": "abc123", "links": links})).unwrap();
            assert!(
                commit
                    .browser_url(&Repository::default(), &fallback)
                    .is_err()
            );
        }
        for hash in [
            "",
            "../abc",
            "abc/123",
            "abc?123",
            "abc 123",
            "ａｂｃ",
            "xyz123",
        ] {
            let commit: Commit = serde_json::from_value(json!({"hash": hash})).unwrap();
            assert!(
                commit
                    .browser_url(&Repository::default(), &fallback)
                    .is_err(),
                "{hash}"
            );
        }
    }

    #[test]
    fn parses_native_diff_content_and_comment_resolution() {
        let participant: Participant =
            serde_json::from_value(json!({"state":null,"approved":false,"role":"REVIEWER"}))
                .expect("native participant");
        assert!(participant.state.is_empty());
        let diff = PullRequestDiff::from_response(&json!({"kind":"diff","pullRequest":42,"content":"--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new\n"})).expect("native diff");
        assert_eq!(diff.files[0].new_path.as_deref(), Some("file.rs"));
        for (resolution, expected) in [
            (json!({"type":"comment_resolution"}), true),
            (Value::Null, false),
        ] {
            let comment: ReviewComment =
                serde_json::from_value(json!({"id":1,"resolution":resolution}))
                    .expect("native comment");
            assert_eq!(comment.resolved, expected);
        }
        let legacy: ReviewComment =
            serde_json::from_value(json!({"id":1,"resolved":true})).expect("normalized comment");
        assert!(legacy.resolved);
    }

    #[test]
    fn validates_bitbucket_references_and_rejects_other_providers() {
        for remote in [
            "git@bitbucket.org:team/repo.git",
            "ssh://git@bitbucket.org/team/repo.git",
            "https://bitbucket.org/team/repo",
        ] {
            assert_eq!(
                RepositoryRef::from_remote(remote)
                    .expect("repository")
                    .repository,
                "repo"
            );
        }
        assert_eq!(
            PullRequestRef::from_url("https://bitbucket.org/team/repo/pull-requests/42/diff")
                .expect("PR")
                .number,
            42
        );
        for remote in [
            "https://github.com/team/repo",
            "git@bitbucket.org:team/../repo",
            "https://bitbucket.org/team/repo?token=secret",
            "file:///tmp/repo",
        ] {
            assert!(RepositoryRef::from_remote(remote).is_err());
        }
        assert!(
            PullRequestRef::from_url("https://bitbucket.org/team/repo/pull-requests/0").is_err()
        );
    }

    #[test]
    fn parses_native_metadata_comments_and_partial_page() {
        let data = json!({"id":42,"title":"Fix","state":"OPEN","description":"Details","author":{"account_id":"account","display_name":"A"},"source":{"branch":{"name":"fix"},"commit":{"hash":"abc"},"repository":{"full_name":"fork/repo"}},"destination":{"branch":{"name":"main"},"commit":{"hash":"def"}},"_comments":{"values":[{"id":7,"user":{"display_name":"B"},"content":{"raw":"hello"},"parent":{"id":6},"inline":{"path":"file.rs","from":4,"to":null},"resolved":true,"deleted":false,"outdated":true}]},"_commits":[{"hash":"abc"}],"_statuses":{"values":[{"state":"SUCCESSFUL"}]},"_failures":[{"resource":"tasks"}]});
        let reference = PullRequestRef {
            workspace: "team".into(),
            repository: "repo".into(),
            number: 42,
        };
        let pr = parse_pull_request(data.clone(), reference.clone()).expect("native PR");
        assert_eq!(pr.revision(), "abc:def");
        assert_eq!(pr.comments[0].parent.as_ref().expect("parent").id, 6);
        assert_eq!(
            pr.comments[0].inline.as_ref().expect("inline").from,
            Some(4)
        );
        assert!(pr.comments[0].resolved && pr.comments[0].outdated);
        assert_eq!(pr.statuses.len(), 1);
        assert_eq!(pr.failures.len(), 1);
        let page = parse_page(json!({"values":[data],"partial":true,"hasNextPage":true,"warnings":["truncated"],"failures":["comments"]}), |value| parse_pull_request(value,reference.clone())).expect("page");
        assert!(page.partial && page.has_more);
        assert_eq!(page.values.len(), 1);
    }

    #[test]
    fn blocks_reviewing_a_closed_or_changed_revision() {
        let reference = PullRequestRef {
            workspace: "team".into(),
            repository: "repo".into(),
            number: 42,
        };
        let mut current = parse_pull_request(json!({"id":42,"title":"Fix","state":"OPEN","source":{"commit":{"hash":"abc"}},"destination":{"commit":{"hash":"def"}}}), reference).expect("PR");
        assert!(validate_revision(&current, "abc:def").is_ok());
        assert!(validate_revision(&current, "old:def").is_err());
        assert!(validate_revision(&current, "abc:old").is_err());
        current.state = "MERGED".into();
        assert!(validate_revision(&current, "abc:def").is_err());
        current.state = "OPEN".into();
        current.source.commit.hash.clear();
        assert!(validate_revision(&current, ":def").is_err());
    }

    #[test]
    fn parses_multifile_diff_with_exact_line_sides_and_file_operations() {
        let text = "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -10,2 +10,3 @@\n context\n-old\n+new\n+added\ndiff --git a/new b/new\nnew file mode 100644\n--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+new\ndiff --git a/old b/old\ndeleted file mode 100644\n--- a/old\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\ndiff --git a/before b/after\nsimilarity index 100%\nrename from before\nrename to after\ndiff --git a/image b/image\nBinary files a/image and b/image differ\n";
        let diff = PullRequestDiff::parse(text).expect("patch");
        assert_eq!(diff.files.len(), 5);
        let lines = &diff.files[0].hunks[0].lines;
        assert_eq!((lines[1].old_line, lines[1].new_line), (Some(11), None));
        assert_eq!((lines[3].old_line, lines[3].new_line), (None, Some(12)));
        assert!(diff.contains_anchor(&DiffAnchor {
            path: "one.rs".into(),
            side: DiffSide::Old,
            line: 11
        }));
        assert!(!diff.contains_anchor(&DiffAnchor {
            path: "one.rs".into(),
            side: DiffSide::Old,
            line: 12
        }));
        assert!(diff.files[1].old_path.is_none());
        let added = &diff.files[1].hunks[0].lines[0];
        assert!(diff.files[1].anchor(added, DiffSide::Old).is_none());
        assert_eq!(
            diff.files[1].anchor(added, DiffSide::New),
            Some(DiffAnchor {
                path: "new".into(),
                side: DiffSide::New,
                line: 1,
            })
        );
        assert!(diff.files[2].new_path.is_none());
        let deleted = &diff.files[2].hunks[0].lines[0];
        assert!(diff.files[2].anchor(deleted, DiffSide::New).is_none());
        assert_eq!(
            diff.files[2].anchor(deleted, DiffSide::Old),
            Some(DiffAnchor {
                path: "old".into(),
                side: DiffSide::Old,
                line: 1,
            })
        );
        assert_eq!(diff.files[3].new_path.as_deref(), Some("after"));
        assert!(diff.files[4].binary);
        for text in [
            "--- a/../secret\n+++ b/../secret\n@@ -1 +1 @@\n-a\n+b\n",
            "--- a/a\n+++ b/a\n@@ -4294967295,2 +1 @@\n-a\n-b\n+c\n",
            "--- a/a\n+++ b/a\n@@ -1,2 +1 @@\n-a\n+b\n",
        ] {
            assert!(PullRequestDiff::parse(text).is_err());
        }
    }

    #[test]
    fn inline_anchors_preserve_renamed_paths_and_exact_line_sides() {
        let mut diff = PullRequestDiff::parse(
            "diff --git a/before.rs b/after.rs\nrename from before.rs\nrename to after.rs\n--- a/before.rs\n+++ b/after.rs\n@@ -10,2 +20,2 @@\n context\n-old\n+new\n",
        ).expect("renamed patch");
        let file = &diff.files[0];
        let lines = &file.hunks[0].lines;
        for (index, side, path, line) in [
            (0, DiffSide::Old, "before.rs", 10),
            (0, DiffSide::New, "after.rs", 20),
            (1, DiffSide::Old, "before.rs", 11),
            (2, DiffSide::New, "after.rs", 21),
        ] {
            let expected = DiffAnchor {
                path: path.into(),
                side,
                line,
            };
            assert_eq!(file.anchor(&lines[index], side), Some(expected.clone()));
            assert!(diff.contains_anchor(&expected));
        }
        assert!(file.anchor(&lines[1], DiffSide::New).is_none());
        assert!(file.anchor(&lines[2], DiffSide::Old).is_none());
        for anchor in [
            DiffAnchor {
                path: "before.rs".into(),
                side: DiffSide::New,
                line: 21,
            },
            DiffAnchor {
                path: "missing.rs".into(),
                side: DiffSide::Old,
                line: 11,
            },
            DiffAnchor {
                path: "before.rs".into(),
                side: DiffSide::Old,
                line: 0,
            },
            DiffAnchor {
                path: "after.rs".into(),
                side: DiffSide::New,
                line: 22,
            },
        ] {
            assert!(!diff.contains_anchor(&anchor));
        }
        let anchor = file.anchor(&lines[0], DiffSide::New).unwrap();
        // Binary files cannot be commented on even if a response supplies text hunks.
        diff.files[0].binary = true;
        assert!(!diff.contains_anchor(&anchor));
    }

    #[test]
    fn comment_payload_keeps_text_structured_and_uses_correct_side() {
        let reference = PullRequestRef {
            workspace: "team".into(),
            repository: "repo".into(),
            number: 42,
        };
        let body = "quote ' and $(touch /tmp/no)\nsecond line";
        for (side, flag, other_flag) in [
            (DiffSide::Old, "--from-line", "--line"),
            (DiffSide::New, "--line", "--from-line"),
        ] {
            let anchor = DiffAnchor {
                path: "file.rs".into(),
                side,
                line: 8,
            };
            let args =
                comment_arguments(&reference, body, None, Some(&anchor)).expect("inline arguments");
            assert!(args.windows(2).any(|pair| pair == ["--text", body]));
            assert!(args.windows(2).any(|pair| pair == ["--path", "file.rs"]));
            assert!(args.windows(2).any(|pair| pair == [flag, "8"]));
            assert!(
                !args
                    .iter()
                    .any(|arg| arg == other_flag || arg == "--reply-to")
            );
            assert!(comment_arguments(&reference, body, Some(7), Some(&anchor)).is_err());
        }
        assert!(comment_arguments(&reference, "", None, None).is_err());
    }

    #[test]
    fn general_and_reply_comment_payloads_keep_text_without_inline_location() {
        let reference = PullRequestRef {
            workspace: "team".into(),
            repository: "repo".into(),
            number: 42,
        };
        let body = "quote ' and $(touch /tmp/no)\nsecond line";
        let expected = vec![
            "bitbucket",
            "pull-requests",
            "comment",
            "create",
            "--pull-request",
            "42",
            "--text",
            body,
            "--workspace",
            "team",
            "--repo",
            "repo",
        ];
        assert_eq!(
            comment_arguments(&reference, body, None, None).expect("general comment"),
            expected
        );
        let mut expected_reply = expected;
        expected_reply.extend(["--reply-to", "7"]);
        assert_eq!(
            comment_arguments(&reference, body, Some(7), None).expect("reply"),
            expected_reply
        );
    }

    #[test]
    fn handles_git_quoted_unicode_rename_and_binary_paths() {
        let diff = PullRequestDiff::parse("diff --git \"a/\\303\\251.rs\" \"b/\\303\\251.rs\"\n--- \"a/\\303\\251.rs\"\n+++ \"b/\\303\\251.rs\"\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/plain \"b/\\303\\251\"\nsimilarity index 100%\nrename from plain\nrename to \"\\303\\251\"\ndiff --git \"a/image\\tfile\" \"b/image\\tfile\"\nBinary files differ\n").expect("quoted patch");
        assert_eq!(diff.files[0].new_path.as_deref(), Some("é.rs"));
        assert_eq!(diff.files[1].new_path.as_deref(), Some("é"));
        assert_eq!(diff.files[2].new_path.as_deref(), Some("image\tfile"));
        assert!(diff.files[2].binary);
    }
}
