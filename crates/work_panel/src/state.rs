#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_query_filters_before_sorting_and_preserves_or_scope() {
        assert_eq!(
            status_query(
                "assignee = currentUser() ORDER BY updated DESC",
                Some("In Progress")
            ),
            "(assignee = currentUser()) AND status = \"In Progress\" ORDER BY updated DESC"
        );
        assert_eq!(
            status_query(
                "project = A OR project = B order\n by priority ASC",
                Some("Blocked")
            ),
            "(project = A OR project = B) AND status = \"Blocked\" order\n by priority ASC"
        );
        assert_eq!(
            status_query("ORDER BY updated", Some("Done")),
            "status = \"Done\" ORDER BY updated"
        );
        assert_eq!(
            status_query("(project = DAT)ORDER BY updated", Some("Done")),
            "((project = DAT)) AND status = \"Done\" ORDER BY updated"
        );
        assert_eq!(
            status_query(r#"project = DAT ORDER BY"Rank" ASC"#, Some("Done")),
            r#"(project = DAT) AND status = "Done" ORDER BY"Rank" ASC"#
        );
    }

    #[test]
    fn status_query_keeps_quoted_order_by_text_and_escapes_status_names() {
        assert_eq!(
            status_query(
                r#"text ~ "ORDER BY" AND summary ~ 'order by' ORDER BY updated DESC"#,
                Some(r#"Needs "owner"\review"#)
            ),
            r#"(text ~ "ORDER BY" AND summary ~ 'order by') AND status = "Needs \"owner\"\\review" ORDER BY updated DESC"#
        );
        assert_eq!(
            status_query(
                r#"summary ~ "an escaped \" ORDER BY quote""#,
                Some("Custom workflow status")
            ),
            r#"(summary ~ "an escaped \" ORDER BY quote") AND status = "Custom workflow status""#
        );
    }

    #[test]
    fn clearing_status_restores_the_original_query() {
        let query = "  project = DAT ORDER BY updated DESC  ";
        assert_eq!(status_query(query, None), query);
        assert_eq!(status_query(query, Some("  ")), query);
        assert_eq!(
            status_query("", Some(" In Progress ")),
            "status = \"In Progress\""
        );
    }

    #[test]
    fn status_options_keep_in_progress_and_deduplicate_observed_or_custom_names() {
        assert_eq!(
            status_options([
                "in progress",
                " In Review ",
                "",
                "Blocked",
                "In Review",
                "Awaiting approval"
            ]),
            ["Awaiting approval", "Blocked", "In Progress", "In Review"]
        );
    }

    #[test]
    fn current_sprint_query_preserves_assignee_and_default_project() {
        assert_eq!(
            current_sprint_query("DAT"),
            "assignee = currentUser() AND project = \"DAT\" AND sprint IN openSprints() ORDER BY updated DESC"
        );
        assert_eq!(
            current_sprint_query(""),
            "assignee = currentUser() AND sprint IN openSprints() ORDER BY updated DESC"
        );
        assert!(current_sprint_query("A\"B").contains("project = \"A\\\"B\""));
    }

    #[test]
    fn ticket_connection_rejects_site_and_executable_changes() {
        assert!(same_connection(
            "/tools/twg",
            "site-a",
            "/tools/twg",
            "site-a"
        ));
        assert!(!same_connection(
            "/tools/twg",
            "site-a",
            "/tools/twg",
            "site-b"
        ));
        assert!(!same_connection(
            "/tools/twg",
            "site-a",
            "/other/twg",
            "site-a"
        ));
    }

    #[test]
    fn assigned_query_applies_project_as_a_quoted_value() {
        assert_eq!(
            assigned_query("DAT"),
            "assignee = currentUser() AND project = \"DAT\" ORDER BY updated DESC"
        );
        assert!(assigned_query("A\"B").contains("\"A\\\"B\""));
    }

    #[test]
    fn zero_disables_refresh_and_large_intervals_do_not_overflow() {
        assert_eq!(refresh_interval(0), None);
        assert_eq!(
            refresh_interval(5),
            Some(std::time::Duration::from_secs(300))
        );
        assert!(refresh_interval(u64::MAX).is_some());
    }

    #[test]
    fn stale_requests_cannot_replace_newer_results() {
        let mut requests = Requests::default();
        let first = requests.next();
        let second = requests.next();
        assert!(!requests.is_current(first));
        assert!(requests.is_current(second));
    }

    #[test]
    fn navigation_requires_clean_draft_and_no_pending_write() {
        assert!(can_navigate(0, false));
        assert!(!can_navigate(1, false));
        assert!(!can_navigate(0, true));
    }

    #[test]
    fn posting_a_comment_does_not_clear_a_newer_draft() {
        assert!(submitted_comment_is_current("sent comment", "sent comment"));
        assert!(!submitted_comment_is_current("sent comment", "new draft"));
    }

    #[test]
    fn remote_changes_only_conflict_with_fields_being_saved() {
        assert!(field_conflicts(true, &"old", &"new"));
        assert!(!field_conflicts(false, &"old", &"new"));
        assert!(!field_conflicts(true, &"old", &"old"));
    }
}

pub(crate) fn assigned_query(project: &str) -> String {
    let project = project.trim();
    let constraint = if project.is_empty() {
        String::new()
    } else {
        format!(
            " AND project = \"{}\"",
            project.replace('\\', "\\\\").replace('"', "\\\"")
        )
    };
    format!("assignee = currentUser(){constraint} ORDER BY updated DESC")
}

pub(crate) fn current_sprint_query(project: &str) -> String {
    format!(
        "{} AND sprint IN openSprints() ORDER BY updated DESC",
        assigned_query(project).trim_end_matches(" ORDER BY updated DESC")
    )
}

pub(crate) fn status_query(query: &str, status: Option<&str>) -> String {
    let Some(status) = status.map(str::trim).filter(|status| !status.is_empty()) else {
        return query.to_owned();
    };
    let (predicate, ordering) = split_order_by(query);
    let status = status.replace('\\', "\\\\").replace('"', "\\\"");
    let mut filtered = if predicate.trim().is_empty() {
        format!("status = \"{status}\"")
    } else {
        format!("({}) AND status = \"{status}\"", predicate.trim())
    };
    if !ordering.is_empty() {
        filtered.push(' ');
        filtered.push_str(ordering.trim());
    }
    filtered
}

fn split_order_by(query: &str) -> (&str, &str) {
    let bytes = query.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0usize;
    for (index, &byte) in bytes.iter().enumerate() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b'o' | b'O'
                if depth == 0
                    && (index == 0
                        || (!bytes[index - 1].is_ascii_alphanumeric()
                            && bytes[index - 1] != b'_'))
                    && bytes
                        .get(index..index + 5)
                        .is_some_and(|word| word.eq_ignore_ascii_case(b"order")) =>
            {
                let mut next = index + 5;
                if !bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                    continue;
                }
                while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                    next += 1;
                }
                if bytes
                    .get(next..next + 2)
                    .is_some_and(|word| word.eq_ignore_ascii_case(b"by"))
                    && bytes.get(next + 2).is_none_or(|byte| {
                        byte.is_ascii_whitespace() || matches!(byte, b'\'' | b'"')
                    })
                {
                    return (&query[..index], &query[index..]);
                }
            }
            _ => {}
        }
    }
    (query, "")
}

pub(crate) fn status_options<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut statuses = vec!["In Progress".to_owned()];
    statuses.extend(
        names
            .into_iter()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
    );
    statuses.sort_by_key(|name| name.to_lowercase());
    statuses.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
    statuses
}

pub(crate) fn refresh_interval(minutes: u64) -> Option<std::time::Duration> {
    (minutes != 0).then(|| std::time::Duration::from_secs(minutes.min(1440) * 60))
}

#[derive(Default)]
pub(crate) struct Requests(u64);

impl Requests {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.0 == generation
    }
}

pub(crate) fn can_navigate(dirty_fields: usize, writing: bool) -> bool {
    dirty_fields == 0 && !writing
}

pub(crate) fn field_conflicts<T: PartialEq>(changed: bool, original: &T, remote: &T) -> bool {
    changed && original != remote
}

pub(crate) fn submitted_comment_is_current(submitted: &str, current: &str) -> bool {
    submitted == current
}

pub(crate) fn same_connection(
    loaded_executable: &str,
    loaded_site: &str,
    current_executable: &str,
    current_site: &str,
) -> bool {
    loaded_executable == current_executable && loaded_site == current_site
}
