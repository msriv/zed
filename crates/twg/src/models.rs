use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(default)]
    pub account_id: String,
    #[serde(default, alias = "name")]
    pub display_name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NamedId {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Ticket {
    #[serde(default)]
    pub id: String,
    pub key: String,
    pub summary: String,
    #[serde(default)]
    pub description: Value,
    #[serde(default)]
    pub status: NamedId,
    #[serde(default)]
    pub priority: Option<NamedId>,
    #[serde(default)]
    pub assignee: Option<User>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub sprints: Option<Vec<Sprint>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sprint {
    #[serde(default)]
    pub id: Option<u64>,
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub end_date: Option<String>,
}

pub struct TicketDetails {
    pub ticket: Ticket,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Comment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub author: Option<User>,
    #[serde(default)]
    pub body: Value,
    #[serde(default)]
    pub created: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CommentPage {
    pub comments: Vec<Comment>,
    pub total: Option<usize>,
    pub next_cursor: Option<String>,
}

pub(crate) fn parse_comments(envelope: Value) -> Result<CommentPage> {
    let comments = serde_json::from_value(envelope["data"].clone())
        .map_err(|_| anyhow!("Unexpected TWG comments response"))?;
    let next_cursor = envelope["pageInfo"]["nextCursor"]
        .as_str()
        .map(str::to_owned);
    if envelope["pageInfo"]["hasNextPage"] == true && next_cursor.is_none() {
        return Err(anyhow!(
            "TWG comments response is missing its next-page cursor"
        ));
    }
    Ok(CommentPage {
        comments,
        total: envelope["meta"]["pagination"]["total"]
            .as_u64()
            .and_then(|total| total.try_into().ok()),
        next_cursor,
    })
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SavedFilter {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub jql: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldMetadata {
    #[serde(default, alias = "fieldId")]
    pub id: String,
    #[serde(default)]
    pub key: String,
    #[serde(default, alias = "fieldName")]
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub schema: Value,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub allowed_values: Vec<Value>,
    #[serde(default)]
    pub accepted_input: Value,
}

#[derive(Clone, Debug, Default)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to: Option<NamedId>,
    pub fields: Vec<FieldMetadata>,
    pub requirements: Value,
}

#[derive(Clone, Debug, Default)]
pub struct WriteResult {
    pub ticket: Option<Ticket>,
    pub verification_error: Option<String>,
}

pub(crate) fn parse_tickets(value: Value) -> Result<Vec<Ticket>> {
    let rows = value.get("issues").cloned().unwrap_or(value);
    serde_json::from_value(rows).map_err(|_| anyhow!("Unexpected TWG ticket response"))
}

pub(crate) fn sprint_field_id(metadata: &[FieldMetadata]) -> Option<&str> {
    metadata
        .iter()
        .find(|field| {
            field.schema["custom"].as_str() == Some("com.pyxis.greenhopper.jira:gh-sprint")
                && !field.id.is_empty()
        })
        .map(|field| field.id.as_str())
}

pub(crate) fn parse_ticket_details(
    value: Value,
    key: &str,
    sprint_field: Option<&str>,
) -> Result<TicketDetails> {
    let rows = value
        .get("issues")
        .unwrap_or(&value)
        .as_array()
        .ok_or_else(|| anyhow!("Unexpected TWG ticket response"))?;
    let row = rows
        .iter()
        .find(|row| {
            row["key"]
                .as_str()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(key))
        })
        .ok_or_else(|| anyhow!("TWG did not return the requested ticket"))?;
    let mut ticket: Ticket = serde_json::from_value(row.clone())
        .map_err(|_| anyhow!("Unexpected TWG ticket response"))?;
    let mut warning = None;
    if let Some(field) = sprint_field {
        match row.get(field) {
            Some(Value::Null) => ticket.sprints = Some(Vec::new()),
            Some(value) => match serde_json::from_value(value.clone()) {
                Ok(sprints) => ticket.sprints = Some(sprints),
                Err(_) => {
                    warning = Some(
                        "Sprint data has an unsupported format; open Jira for Sprint details."
                            .into(),
                    )
                }
            },
            None => warning = Some("Jira did not return the Sprint field for this ticket.".into()),
        }
    }
    Ok(TicketDetails { ticket, warning })
}

pub(crate) fn parse_metadata(value: Value) -> Result<Vec<FieldMetadata>> {
    let value = value.get("fields").cloned().unwrap_or(value);
    let mut fields: Vec<FieldMetadata> = match value {
        Value::Null => Vec::new(),
        Value::Array(_) => {
            serde_json::from_value(value).map_err(|_| anyhow!("Unexpected TWG field metadata"))?
        }
        Value::Object(entries) => entries
            .into_iter()
            .map(|(key, value)| {
                let mut field: FieldMetadata = serde_json::from_value(value)
                    .map_err(|_| anyhow!("Unexpected TWG field metadata"))?;
                if field.id.is_empty() {
                    field.id = key.clone();
                }
                if field.key.is_empty() {
                    field.key = key;
                }
                Ok(field)
            })
            .collect::<Result<_>>()?,
        _ => return Err(anyhow!("Unexpected TWG field metadata")),
    };
    for field in &mut fields {
        if field.id.is_empty() {
            field.id = field.key.clone();
        }
        if field.key.is_empty() {
            field.key = field.id.clone();
        }
    }
    Ok(fields)
}

pub(crate) fn parse_transitions(value: Value) -> Result<Vec<Transition>> {
    let rows = value
        .get("transitions")
        .unwrap_or(&value)
        .as_array()
        .ok_or_else(|| anyhow!("Unexpected TWG transitions response"))?;
    rows.iter()
        .map(|row| {
            let mut fields = parse_metadata(row["fields"].clone())?;
            if row["requirements"].is_array() {
                for requirement in parse_metadata(row["requirements"].clone())? {
                    if let Some(field) =
                        fields.iter_mut().find(|field| field.key == requirement.key)
                    {
                        field.required |= requirement.required;
                    } else {
                        fields.push(requirement);
                    }
                }
            }
            Ok(Transition {
                id: row["id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Transition is missing its ID"))?
                    .to_owned(),
                name: row["name"].as_str().unwrap_or_default().to_owned(),
                to: if let Some(name) = row["toName"].as_str() {
                    Some(NamedId {
                        id: String::new(),
                        name: name.to_owned(),
                    })
                } else {
                    serde_json::from_value(row["to"].clone())
                        .map_err(|_| anyhow!("Unexpected transition status"))?
                },
                fields,
                requirements: row["requirements"].clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sprint_discovery_uses_schema_and_returned_field_id() {
        let fields = parse_metadata(json!({"fields":[
            {"id":"customfield_99999","name":"Sprint","schema":{"type":"string"}},
            {"id":"customfield_24680","schema":{"custom":"com.pyxis.greenhopper.jira:gh-sprint"}}
        ]}))
        .expect("metadata");
        assert_eq!(sprint_field_id(&fields), Some("customfield_24680"));
    }

    #[test]
    fn dynamic_sprints_preserve_description_and_distinguish_unknown_from_empty() {
        let description = json!({"type":"doc","version":1,"content":[]});
        let row = json!({"key":"TEST-1","summary":"Ticket", "description":description,
            "customfield_24680":[{"id":49508,"name":"Analytics sprint 21","state":"active","endDate":"2026-10-28T07:00:00.000Z"}]});
        let details =
            parse_ticket_details(json!([row.clone()]), "TEST-1", Some("customfield_24680"))
                .expect("ticket");
        let sprints = details.ticket.sprints.expect("known sprints");
        assert_eq!(sprints[0].name, "Analytics sprint 21");
        assert_eq!(sprints[0].state, "active");
        assert_eq!(
            sprints[0].end_date.as_deref(),
            Some("2026-10-28T07:00:00.000Z")
        );
        assert_eq!(details.ticket.description, description);
        let empty = parse_ticket_details(
            json!([{ "key":"TEST-1","summary":"Ticket","customfield_24680":[] }]),
            "TEST-1",
            Some("customfield_24680"),
        )
        .expect("empty");
        assert!(
            empty
                .ticket
                .sprints
                .is_some_and(|sprints| sprints.is_empty())
        );
        let unknown = parse_ticket_details(json!([row.clone()]), "TEST-1", None).expect("unknown");
        assert!(unknown.ticket.sprints.is_none());
        let malformed = parse_ticket_details(json!([{ "key":"TEST-1","summary":"Ticket","customfield_24680":["legacy representation"] }]), "TEST-1", Some("customfield_24680")).expect("basic ticket retained");
        assert!(malformed.ticket.sprints.is_none());
        assert!(malformed.warning.is_some());
    }

    #[test]
    fn parses_comment_pagination_without_inventing_totals() {
        let page = parse_comments(json!({"data":[{"id":"1","body":{"type":"doc","content":[]}}],"pageInfo":{"hasNextPage":true,"nextCursor":"cursor-2"},"meta":{"pagination":{"startAt":0,"maxResults":1,"total":3}}})).expect("page");
        assert_eq!(page.comments.len(), 1);
        assert_eq!(page.total, Some(3));
        assert_eq!(page.next_cursor.as_deref(), Some("cursor-2"));
        let empty =
            parse_comments(json!({"data":[],"pageInfo":{"hasNextPage":false,"nextCursor":null}}))
                .expect("page");
        assert_eq!(empty.total, None);
        assert_eq!(empty.next_cursor, None);
        assert!(parse_comments(json!({"data":[],"pageInfo":{"hasNextPage":true}})).is_err());
    }

    #[test]
    fn parses_native_query_and_get_shapes_without_losing_adf() {
        let ticket = json!({"key":"TEST-1","summary":"Summary","description":{"type":"doc","content":[{"type":"media"}]},"assignee":{"accountId":"abc","displayName":"Person"}});
        for value in [json!({"issues":[ticket.clone()]}), json!([ticket.clone()])] {
            let tickets = parse_tickets(value).expect("tickets");
            assert_eq!(tickets.len(), 1);
            assert_eq!(tickets[0].description, ticket["description"]);
            assert_eq!(
                tickets[0].assignee.as_ref().expect("assignee").account_id,
                "abc"
            );
        }
        assert!(parse_tickets(json!({"unexpected":[]})).is_err());
    }

    #[test]
    fn parses_array_and_transition_object_metadata() {
        let field = json!({"name":"Resolution","required":true,"schema":{"type":"resolution"},"operations":["set"],"allowedValues":[{"id":"1","name":"Done"}]});
        let fields = parse_metadata(json!({"resolution":field.clone()})).expect("fields");
        assert_eq!(fields[0].id, "resolution");
        assert!(fields[0].required);
        let fields = parse_metadata(
            json!({"fields":[{"id":"summary","key":"summary","name":"Summary","required":true}]}),
        )
        .expect("fields");
        assert_eq!(fields[0].key, "summary");
    }

    #[test]
    fn parses_live_transition_field_aliases_and_target_name() {
        let transitions = parse_transitions(json!({"transitions":[{"id":"31","name":"Resolve","toName":"Done","fields":[{"fieldId":"resolution","fieldName":"Resolution","required":true,"operations":["set"],"schema":{"type":"resolution"},"allowedValues":[{"id":"1","name":"Done"}]}],"requirements":[{"fieldId":"resolution","fieldName":"Resolution","required":true}]}]})).expect("transitions");
        assert_eq!(transitions[0].fields[0].id, "resolution");
        assert_eq!(transitions[0].fields[0].name, "Resolution");
        assert_eq!(transitions[0].to.as_ref().expect("target").name, "Done");
        assert!(transitions[0].requirements.is_array());
    }

    #[test]
    fn transition_requirements_without_fields_still_require_user_input() {
        let transitions = parse_transitions(json!({"transitions":[{"id":"31","name":"Resolve","requirements":[{"fieldId":"resolution","fieldName":"Resolution","required":true,"operations":["set"]}]}]})).expect("transitions");
        assert!(
            transitions[0]
                .fields
                .iter()
                .any(|field| field.key == "resolution" && field.required)
        );
    }

    #[test]
    fn transition_requirements_are_preserved() {
        let transitions = parse_transitions(json!({"transitions":[{"id":"31","name":"Done","to":{"id":"3","name":"Done"},"fields":{"resolution":{"required":true,"name":"Resolution"}},"requirements":{"requiredFields":["resolution"]}}]})).expect("transitions");
        assert_eq!(transitions[0].id, "31");
        assert!(transitions[0].fields[0].required);
        assert_eq!(
            transitions[0].requirements["requiredFields"][0],
            "resolution"
        );
    }
}
