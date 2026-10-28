pub mod bitbucket;
mod models;
mod payload;
mod process;
pub mod rich_text;
pub mod settings;

pub use models::{
    Comment, CommentPage, FieldMetadata, NamedId, SavedFilter, Sprint, Ticket, TicketDetails,
    Transition, User, WriteResult,
};
pub use settings::TwgSettings;

use anyhow::{Context, Result, anyhow, ensure};
use futures::{
    FutureExt,
    future::{Either, select},
};
use serde_json::{Map, Value, json};
use std::{path::PathBuf, time::Duration};
use util::command::{Stdio, new_command};

const TIMEOUT: Duration = Duration::from_secs(30);
const TICKET_FIELDS: &str = "summary,description,status,assignee,priority,labels,updated";

#[derive(Clone, Debug)]
pub struct Client {
    executable: PathBuf,
    site: Option<String>,
}

pub fn discover_executable(configured: Option<&str>) -> PathBuf {
    if let Some(configured) = configured.filter(|path| !path.trim().is_empty()) {
        return PathBuf::from(configured.trim());
    }
    let name = if cfg!(windows) { "twg.exe" } else { "twg" };
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let candidate = PathBuf::from(home).join(".local/bin").join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

impl Client {
    pub fn new(executable: PathBuf, site: Option<String>) -> Self {
        Self {
            executable,
            site: site.filter(|site| !site.trim().is_empty()),
        }
    }

    async fn output(&self, arguments: Vec<String>) -> Result<std::process::Output> {
        let mut command = new_command(&self.executable);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = command.output().boxed();
        let timeout = smol::Timer::after(TIMEOUT + Duration::from_secs(5)).boxed();
        match select(output, timeout).await {
            Either::Left((output, _)) => {
                output.context("Could not run TWG; check its executable in TWG settings")
            }
            Either::Right(_) => Err(anyhow!(
                "TWG timed out; a write may have completed. Refresh the ticket before retrying a write"
            )),
        }
    }

    pub async fn version(&self) -> Result<String> {
        let output = self.output(vec!["--version".into()]).await?;
        ensure!(output.status.success(), "Could not check the TWG version");
        let version = String::from_utf8(output.stdout).context("Invalid TWG version response")?;
        ensure!(version.len() < 200, "Unexpected TWG version response");
        Ok(version.trim().to_owned())
    }

    pub async fn run_json(&self, arguments: Vec<String>) -> Result<Value> {
        let envelope = self.run_envelope(arguments).await?;
        Ok(envelope["data"].clone())
    }

    async fn run_envelope(&self, arguments: Vec<String>) -> Result<Value> {
        let output = self.json_output(arguments).await?;
        process::decode_envelope(output.status.success(), &output.stdout)
    }

    async fn run_native_json(&self, arguments: Vec<String>) -> Result<Value> {
        let output = self.json_output(arguments).await?;
        process::decode_native(output.status.success(), &output.stdout)
    }

    async fn json_output(&self, mut arguments: Vec<String>) -> Result<std::process::Output> {
        arguments.extend([
            "-o".into(),
            "json".into(),
            "--output-summary".into(),
            "none".into(),
            "--timeout-ms".into(),
            TIMEOUT.as_millis().to_string(),
        ]);
        if let Some(site) = &self.site {
            arguments.extend(["--site".into(), site.clone()]);
        }
        self.output(arguments).await
    }

    async fn jira_json(&self, arguments: Vec<String>) -> Result<Value> {
        ensure!(
            self.site.is_some(),
            "Choose a Jira site in TWG settings before accessing tickets"
        );
        self.run_json(arguments).await
    }

    pub async fn whoami(&self) -> Result<User> {
        let data = self.run_json(vec!["whoami".into()]).await?;
        let user = data.get("user").cloned().unwrap_or(data);
        let user: User = serde_json::from_value(user)
            .map_err(|_| anyhow!("Unexpected TWG identity response"))?;
        ensure!(
            !user.account_id.is_empty(),
            "TWG did not return an authenticated account; check authentication in TWG settings"
        );
        Ok(user)
    }

    pub async fn query_tickets(&self, jql: &str, limit: usize) -> Result<Vec<Ticket>> {
        ensure!(!jql.trim().is_empty(), "Enter a JQL query");
        let data = self
            .jira_json(vec![
                "jira".into(),
                "workitem".into(),
                "query".into(),
                "--jql".into(),
                jql.into(),
                "--first".into(),
                limit.clamp(1, 500).to_string(),
                "--fields".into(),
                TICKET_FIELDS.into(),
            ])
            .await?;
        models::parse_tickets(data)
    }

    pub async fn get_ticket(&self, key: &str) -> Result<Ticket> {
        let data = self
            .jira_json(vec![
                "jira".into(),
                "workitem".into(),
                "get".into(),
                key.into(),
                "--fields".into(),
                TICKET_FIELDS.into(),
                "--comments".into(),
            ])
            .await?;
        models::parse_tickets(data)?
            .into_iter()
            .find(|ticket| ticket.key.eq_ignore_ascii_case(key))
            .ok_or_else(|| anyhow!("TWG did not return the requested ticket"))
    }

    pub async fn get_ticket_details(&self, key: &str) -> Result<TicketDetails> {
        let metadata = match self.update_metadata(key).await {
            Ok(metadata) => metadata,
            Err(error) => {
                return Ok(TicketDetails {
                    ticket: self.get_ticket(key).await?,
                    warning: Some(format!(
                        "Ticket loaded; Sprint metadata unavailable: {error:#}"
                    )),
                });
            }
        };
        let Some(sprint_field) = models::sprint_field_id(&metadata) else {
            return Ok(TicketDetails {
                ticket: self.get_ticket(key).await?,
                warning: Some("Sprint metadata is unavailable for this ticket.".into()),
            });
        };
        let result = self
            .jira_json(vec![
                "jira".into(),
                "workitem".into(),
                "get".into(),
                key.into(),
                "--fields".into(),
                format!("{TICKET_FIELDS},{sprint_field}"),
                "--comments".into(),
            ])
            .await;
        match result {
            Ok(data) => models::parse_ticket_details(data, key, Some(sprint_field)),
            Err(error) => Ok(TicketDetails {
                ticket: self.get_ticket(key).await?,
                warning: Some(format!(
                    "Ticket loaded; Sprint details unavailable: {error:#}"
                )),
            }),
        }
    }

    pub async fn comments(
        &self,
        key: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<CommentPage> {
        ensure!(
            self.site.is_some(),
            "Choose a Jira site in TWG settings before accessing tickets"
        );
        let mut arguments = vec![
            "jira".into(),
            "workitem".into(),
            "comment".into(),
            "query".into(),
            "--issue-id".into(),
            key.into(),
            "--first".into(),
            limit.clamp(1, 100).to_string(),
            "--order-by=-created".into(),
        ];
        if let Some(after) = after {
            arguments.extend(["--after".into(), after.into()]);
        }
        models::parse_comments(self.run_envelope(arguments).await?)
    }

    pub async fn filters(&self) -> Result<Vec<SavedFilter>> {
        let data = self
            .jira_json(vec![
                "jira".into(),
                "filter".into(),
                "query".into(),
                "--first".into(),
                "50".into(),
                "--start-at".into(),
                "0".into(),
                "--expand".into(),
                "jql".into(),
            ])
            .await?;
        let rows = data
            .get("values")
            .or_else(|| data.get("filters"))
            .cloned()
            .unwrap_or(data);
        serde_json::from_value(rows).map_err(|_| anyhow!("Unexpected TWG saved filters response"))
    }

    pub async fn update_metadata(&self, key: &str) -> Result<Vec<FieldMetadata>> {
        let data = self
            .jira_json(vec![
                "jira".into(),
                "workitem".into(),
                "field".into(),
                "update-metadata".into(),
                "--id".into(),
                key.into(),
                "--include-system-fields".into(),
            ])
            .await?;
        models::parse_metadata(data)
    }

    pub async fn users(&self, text: &str) -> Result<Vec<User>> {
        ensure!(!text.trim().is_empty(), "Enter a person’s name");
        let data = self
            .run_json(vec![
                "user".into(),
                "search".into(),
                "--name".into(),
                text.into(),
                "--limit".into(),
                "20".into(),
            ])
            .await?;
        let rows = data.get("users").cloned().unwrap_or(data);
        serde_json::from_value(rows).map_err(|_| anyhow!("Unexpected TWG user search response"))
    }

    pub async fn update_ticket(
        &self,
        key: &str,
        mut fields: Map<String, Value>,
        description_markdown: Option<String>,
    ) -> Result<WriteResult> {
        let current = self.get_ticket(key).await?;
        let metadata = self.update_metadata(key).await?;
        fields.retain(|field, value| current_field(&current, field).as_ref() != Some(value));
        validate_fields(&fields, &metadata)?;
        if description_markdown.is_some() {
            ensure!(
                rich_text::project(&current.description).editable,
                "This description contains unsupported rich content; edit it in Jira to preserve formatting"
            );
            ensure!(
                metadata.iter().any(|field| field.key == "description"
                    && field.operations.iter().any(|operation| operation == "set")),
                "Description is not editable on this ticket"
            );
        }
        if fields.is_empty() && description_markdown.is_none() {
            return Ok(WriteResult {
                ticket: Some(current),
                verification_error: None,
            });
        }
        let arguments = payload::update_arguments(key, fields, description_markdown)?;
        self.jira_json(arguments).await?;
        Ok(self.verify_write(key).await)
    }

    pub async fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let data = self
            .jira_json(vec![
                "jira".into(),
                "workitem".into(),
                "transition".into(),
                "--id".into(),
                key.into(),
            ])
            .await?;
        models::parse_transitions(data)
    }

    pub async fn transition(
        &self,
        key: &str,
        id: &str,
        fields: Map<String, Value>,
        comment: Option<&str>,
    ) -> Result<WriteResult> {
        self.get_ticket(key).await?;
        let transition = self
            .transitions(key)
            .await?
            .into_iter()
            .find(|transition| transition.id == id)
            .ok_or_else(|| anyhow!("This transition is no longer available; refresh the ticket"))?;
        validate_transition_fields(&fields, &transition.fields, comment)?;
        let arguments = payload::transition_arguments(key, id, fields, comment)?;
        self.jira_json(arguments).await?;
        Ok(self.verify_write(key).await)
    }

    pub async fn add_comment(&self, key: &str, body: &str) -> Result<WriteResult> {
        ensure!(!body.trim().is_empty(), "Enter a comment");
        self.get_ticket(key).await?;
        self.jira_json(vec![
            "jira".into(),
            "workitem".into(),
            "comment".into(),
            "create".into(),
            "--issue-id".into(),
            key.into(),
            "--body".into(),
            body.into(),
            "--body-format".into(),
            "markdown".into(),
        ])
        .await?;
        Ok(self.verify_write(key).await)
    }

    pub async fn log_work(
        &self,
        key: &str,
        time_spent: &str,
        started: Option<&str>,
        comment: Option<&str>,
    ) -> Result<WriteResult> {
        let arguments = payload::worklog_arguments(key, time_spent, started, comment)?;
        self.get_ticket(key).await?;
        self.jira_json(arguments).await?;
        Ok(self.verify_write(key).await)
    }

    async fn verify_write(&self, key: &str) -> WriteResult {
        match self.get_ticket(key).await {
            Ok(ticket) => WriteResult {
                ticket: Some(ticket),
                verification_error: None,
            },
            Err(error) => WriteResult {
                ticket: None,
                verification_error: Some(format!(
                    "Write succeeded, but verification failed: {error}. Refresh before making another change."
                )),
            },
        }
    }
}

fn current_field(ticket: &Ticket, field: &str) -> Option<Value> {
    match field {
        "summary" => Some(json!(ticket.summary)),
        "description" => Some(ticket.description.clone()),
        "labels" => Some(json!(ticket.labels)),
        "assignee" => Some(
            ticket
                .assignee
                .as_ref()
                .map(|user| json!({"accountId":user.account_id}))
                .unwrap_or(Value::Null),
        ),
        "priority" => Some(
            ticket
                .priority
                .as_ref()
                .map(|priority| json!({"id":priority.id}))
                .unwrap_or(Value::Null),
        ),
        _ => None,
    }
}

fn has_value(value: &Value) -> bool {
    !value.is_null()
        && value.as_str().is_none_or(|text| !text.trim().is_empty())
        && value.as_array().is_none_or(|values| !values.is_empty())
        && value.as_object().is_none_or(|values| !values.is_empty())
        && ["id", "accountId"]
            .iter()
            .all(|key| value.get(key).is_none_or(has_value))
}

fn validate_fields(fields: &Map<String, Value>, metadata: &[FieldMetadata]) -> Result<()> {
    for (key, value) in fields {
        let field = metadata
            .iter()
            .find(|field| &field.key == key || &field.id == key)
            .ok_or_else(|| anyhow!("Field {key} is not editable on this ticket"))?;
        ensure!(
            field.operations.iter().any(|operation| operation == "set"),
            "Field {} does not support setting a value",
            field.name
        );
        ensure!(
            !field.required || has_value(value),
            "Field {} requires a value",
            field.name
        );
        if !field.allowed_values.is_empty() && !value.is_null() {
            let is_allowed = |value: &Value| {
                field.allowed_values.iter().any(|allowed| {
                    if let Some((key, identifier)) = ["id", "accountId", "value", "key", "name"]
                        .iter()
                        .find_map(|key| value.get(key).map(|value| (*key, value)))
                    {
                        allowed.get(key) == Some(identifier)
                    } else {
                        allowed == value
                    }
                })
            };
            let valid = value
                .as_array()
                .map_or_else(|| is_allowed(value), |values| values.iter().all(is_allowed));
            ensure!(valid, "Choose an available value for {}", field.name);
        }
    }
    Ok(())
}

fn validate_transition_fields(
    fields: &Map<String, Value>,
    metadata: &[FieldMetadata],
    comment: Option<&str>,
) -> Result<()> {
    ensure!(
        !fields.contains_key("comment"),
        "Use the transition comment input for a comment"
    );
    validate_fields(fields, metadata)?;
    for field in metadata.iter().filter(|field| field.required) {
        let present = if field.key == "comment" || field.id == "comment" {
            comment.is_some_and(|comment| !comment.trim().is_empty())
        } else {
            fields
                .get(&field.key)
                .or_else(|| fields.get(&field.id))
                .is_some_and(has_value)
        };
        ensure!(present, "Transition requires a value for {}", field.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_transition_comment_supports_add_and_stays_out_of_fields() {
        let metadata = vec![FieldMetadata {
            id: "comment".into(),
            key: "comment".into(),
            name: "Comment".into(),
            required: true,
            operations: vec!["add".into()],
            ..Default::default()
        }];
        assert!(validate_transition_fields(&Map::new(), &metadata, Some("Reason")).is_ok());
        assert!(validate_transition_fields(&Map::new(), &metadata, None).is_err());
        assert!(validate_transition_fields(&Map::new(), &metadata, Some(" ")).is_err());
        assert!(
            validate_transition_fields(
                json!({"comment":"Reason"}).as_object().expect("object"),
                &metadata,
                Some("Reason")
            )
            .is_err()
        );
    }

    #[test]
    fn field_validation_rejects_empty_required_choices_and_unknown_ids() {
        let metadata = vec![FieldMetadata {
            id: "resolution".into(),
            key: "resolution".into(),
            name: "Resolution".into(),
            required: true,
            operations: vec!["set".into()],
            allowed_values: vec![json!({"id":"1","name":"Done"})],
            ..Default::default()
        }];
        for value in [
            Value::Null,
            json!({}),
            json!({"id":""}),
            json!({"id":"99"}),
            json!({"id":"99","name":"Done"}),
        ] {
            assert!(
                validate_fields(
                    json!({"resolution":value}).as_object().expect("object"),
                    &metadata
                )
                .is_err(),
                "accepted {value}"
            );
        }
        assert!(
            validate_fields(
                json!({"resolution":{"id":"1"}})
                    .as_object()
                    .expect("object"),
                &metadata
            )
            .is_ok()
        );
    }
}
