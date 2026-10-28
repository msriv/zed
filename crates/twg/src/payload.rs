use anyhow::{Result, ensure};
use serde_json::{Map, Value};

pub(crate) fn update_arguments(
    key: &str,
    fields: Map<String, Value>,
    description: Option<String>,
) -> Result<Vec<String>> {
    ensure!(
        !key.trim().is_empty() && !key.starts_with('-'),
        "A ticket key is required"
    );
    ensure!(
        !fields.contains_key("status"),
        "Use an available workflow transition to change status"
    );
    ensure!(
        !(fields.contains_key("description") && description.is_some()),
        "Supply only one description format"
    );
    if let Some(value) = fields.get("description") {
        ensure!(
            value.is_null() || (value["type"] == "doc" && value.is_object()),
            "Description fields must contain raw ADF; use the Markdown description input for text"
        );
    }
    let mut arguments = vec![
        "jira".into(),
        "workitem".into(),
        "update".into(),
        "--id".into(),
        key.into(),
    ];
    if !fields.is_empty() {
        arguments.extend(["--fields-json".into(), serde_json::to_string(&fields)?]);
    }
    if let Some(description) = description {
        arguments.extend([
            "--description".into(),
            description,
            "--description-format".into(),
            "markdown".into(),
        ]);
    }
    Ok(arguments)
}

pub(crate) fn transition_arguments(
    key: &str,
    id: &str,
    fields: Map<String, Value>,
    comment: Option<&str>,
) -> Result<Vec<String>> {
    ensure!(
        !key.trim().is_empty() && !key.starts_with('-'),
        "A ticket key is required"
    );
    ensure!(
        !id.trim().is_empty() && !id.starts_with('-'),
        "A transition ID is required"
    );
    ensure!(
        !fields.contains_key("comment"),
        "Use the transition comment input for a comment"
    );
    let mut arguments = vec![
        "jira".into(),
        "workitem".into(),
        "transition".into(),
        "--id".into(),
        key.into(),
        "--transition-id".into(),
        id.into(),
    ];
    if !fields.is_empty() {
        arguments.extend(["--fields-json".into(), serde_json::to_string(&fields)?]);
    }
    if let Some(comment) = comment.filter(|comment| !comment.trim().is_empty()) {
        arguments.extend([
            "--transition-comment".into(),
            comment.into(),
            "--transition-comment-format".into(),
            "plain".into(),
        ]);
    }
    Ok(arguments)
}

pub(crate) fn worklog_arguments(
    key: &str,
    time_spent: &str,
    started: Option<&str>,
    comment: Option<&str>,
) -> Result<Vec<String>> {
    ensure!(
        !key.trim().is_empty() && !key.starts_with('-'),
        "A ticket key is required"
    );
    ensure!(
        !time_spent.trim().is_empty(),
        "Enter time spent, for example 1h 30m"
    );
    // Logging time must not also change the ticket's remaining estimate.
    let mut arguments = vec![
        "jira".into(),
        "workitem".into(),
        "worklog".into(),
        "add".into(),
        "--issue-id".into(),
        key.into(),
        "--time-spent".into(),
        time_spent.into(),
        "--adjust-estimate".into(),
        "leave".into(),
    ];
    if let Some(started) = started {
        ensure!(!started.trim().is_empty(), "Enter a work start timestamp");
        arguments.extend(["--started".into(), started.into()]);
    }
    if let Some(comment) = comment.filter(|comment| !comment.trim().is_empty()) {
        arguments.extend([
            "--comment".into(),
            comment.into(),
            "--comment-format".into(),
            "plain".into(),
        ]);
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transition_comment_uses_plain_argument_and_is_never_a_field() {
        let comment = "Reason <plain> $(literal)";
        let arguments = transition_arguments(
            "TEST-1",
            "31",
            json!({"resolution":{"id":"1"}})
                .as_object()
                .expect("object")
                .clone(),
            Some(comment),
        )
        .expect("arguments");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--transition-comment", comment])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--transition-comment-format", "plain"])
        );
        let fields = arguments
            .windows(2)
            .find(|pair| pair[0] == "--fields-json")
            .expect("fields");
        assert_eq!(
            serde_json::from_str::<Value>(&fields[1]).expect("json"),
            json!({"resolution":{"id":"1"}})
        );
        assert!(
            transition_arguments(
                "TEST-1",
                "31",
                json!({"comment":"unsafe raw body"})
                    .as_object()
                    .expect("object")
                    .clone(),
                None
            )
            .is_err()
        );
        assert!(
            !transition_arguments("TEST-1", "31", Map::new(), Some(" "))
                .expect("arguments")
                .iter()
                .any(|argument| argument == "--transition-comment")
        );
    }

    #[test]
    fn worklog_uses_explicit_plain_values_and_never_adjusts_estimates() {
        let comment = "Review $(literal) <not markup>";
        let arguments = worklog_arguments(
            "TEST-1",
            "2h 30m",
            Some("2026-10-28T10:00:00.000+0000"),
            Some(comment),
        )
        .expect("arguments");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--time-spent", "2h 30m"])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--comment", comment])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--comment-format", "plain"])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--adjust-estimate", "leave"])
        );
        assert!(worklog_arguments("TEST-1", " ", None, None).is_err());
        assert!(worklog_arguments("--help", "1h", None, None).is_err());
        assert!(worklog_arguments("TEST-1", "1h", Some(" "), None).is_err());
    }

    #[test]
    fn summary_edit_does_not_resend_description() {
        let arguments = update_arguments(
            "TEST-1",
            json!({"summary":"Changed"})
                .as_object()
                .expect("object")
                .clone(),
            None,
        )
        .expect("arguments");
        let payload = arguments
            .windows(2)
            .find(|pair| pair[0] == "--fields-json")
            .expect("fields");
        assert_eq!(
            serde_json::from_str::<Value>(&payload[1]).expect("json"),
            json!({"summary":"Changed"})
        );
        assert!(!arguments.iter().any(|argument| argument == "--description"));
    }

    #[test]
    fn raw_adf_is_preserved_without_conversion() {
        let document = json!({"type":"doc","version":1,"content":[{"type":"table","content":[]}]});
        let arguments = update_arguments(
            "TEST-1",
            json!({"description":document})
                .as_object()
                .expect("object")
                .clone(),
            None,
        )
        .expect("arguments");
        let payload = arguments
            .windows(2)
            .find(|pair| pair[0] == "--fields-json")
            .expect("fields");
        assert_eq!(
            serde_json::from_str::<Value>(&payload[1]).expect("json")["description"],
            document
        );
    }

    #[test]
    fn rejects_direct_status_and_ambiguous_description_updates() {
        assert!(
            update_arguments(
                "TEST-1",
                json!({"status":{"id":"1"}})
                    .as_object()
                    .expect("object")
                    .clone(),
                None
            )
            .is_err()
        );
        assert!(
            update_arguments(
                "TEST-1",
                json!({"description":null})
                    .as_object()
                    .expect("object")
                    .clone(),
                Some("text".into())
            )
            .is_err()
        );
        assert!(
            update_arguments(
                "TEST-1",
                json!({"description":"lossy projection"})
                    .as_object()
                    .expect("object")
                    .clone(),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn markdown_is_explicit_and_shell_characters_are_single_arguments() {
        let text = "hello $(touch /tmp/no) `literal`\nnext";
        let arguments =
            update_arguments("TEST-1", Map::new(), Some(text.into())).expect("arguments");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--description", text])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--description-format", "markdown"])
        );
    }
}
