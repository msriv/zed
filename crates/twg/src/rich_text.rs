use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub markdown: String,
    pub editable: bool,
}

pub fn project(value: &Value) -> Projection {
    if value.is_null() {
        return Projection {
            markdown: String::new(),
            editable: true,
        };
    }
    if let Some(text) = value.as_str() {
        return Projection {
            markdown: text.to_owned(),
            editable: true,
        };
    }
    let mut editable = true;
    let markdown = render(value, &mut editable, 0).trim_end().to_owned();
    Projection { markdown, editable }
}

fn escape(text: &str) -> String {
    let mut result = String::new();
    for character in text.chars() {
        if "\\`*_{}[]<>()#+-.!|~".contains(character) {
            result.push('\\');
        }
        result.push(character);
    }
    result
}

fn render(value: &Value, editable: &mut bool, depth: usize) -> String {
    if depth > 64 {
        *editable = false;
        return "[Unsupported: deeply nested content]".into();
    }
    let kind = value["type"].as_str().unwrap_or("unknown");
    if value["attrs"].as_object().is_some_and(|attributes| {
        attributes
            .keys()
            .any(|key| kind != "heading" || key != "level")
    }) {
        *editable = false;
    }
    let content = value["content"]
        .as_array()
        .map(|children| {
            children
                .iter()
                .map(|child| render(child, editable, depth + 1))
                .collect::<String>()
        })
        .unwrap_or_default();
    match kind {
        "doc" => content,
        "paragraph" => format!("{content}\n\n"),
        "heading" => {
            let level = value["attrs"]["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize;
            format!("{} {content}\n\n", "#".repeat(level))
        }
        "hardBreak" => "  \n".into(),
        "text" => {
            let mut text = escape(value["text"].as_str().unwrap_or_default());
            if let Some(marks) = value["marks"].as_array() {
                for mark in marks {
                    let kind = mark["type"].as_str().unwrap_or_default();
                    if mark["attrs"].as_object().is_some_and(|attributes| {
                        attributes.keys().any(|key| kind != "link" || key != "href")
                    }) {
                        *editable = false;
                    }
                    text = match kind {
                        "strong" => format!("**{text}**"),
                        "em" => format!("*{text}*"),
                        "strike" => format!("~~{text}~~"),
                        "link" => {
                            let href = mark["attrs"]["href"].as_str().unwrap_or_default();
                            if (href.starts_with("https://")
                                || href.starts_with("http://")
                                || href.starts_with("mailto:"))
                                && !href.chars().any(|character| {
                                    character.is_control() || character.is_whitespace()
                                })
                            {
                                let href = href
                                    .replace('(', "%28")
                                    .replace(')', "%29")
                                    .replace('\\', "%5C")
                                    .replace('<', "%3C")
                                    .replace('>', "%3E");
                                format!("[{text}]({href})")
                            } else {
                                *editable = false;
                                text
                            }
                        }
                        _ => {
                            *editable = false;
                            text
                        }
                    };
                }
            }
            text
        }
        _ => {
            *editable = false;
            format!("[Unsupported: {kind}]\n{content}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_paragraphs_and_safe_links() {
        let document = json!({"type":"doc","version":1,"content":[{"type":"paragraph","content":[
            {"type":"text","text":"Hello *world*","marks":[{"type":"strong"}]},
            {"type":"text","text":" docs","marks":[{"type":"link","attrs":{"href":"https://example.com"}}]}
        ]}]});
        assert_eq!(
            project(&document),
            Projection {
                markdown: "**Hello \\*world\\***[ docs](https://example.com)".into(),
                editable: true,
            }
        );
    }

    #[test]
    fn unsupported_content_is_visible_and_cannot_be_edited_as_markdown() {
        let document = json!({"type":"doc","content":[{"type":"table","content":[
            {"type":"paragraph","content":[{"type":"text","text":"Keep me"}]}
        ]}]});
        let projection = project(&document);
        assert!(!projection.editable);
        assert!(projection.markdown.contains("Keep me"));
        assert!(projection.markdown.contains("Unsupported: table"));
    }

    #[test]
    fn rejects_unsafe_links_and_unknown_marks() {
        for mark in [
            json!({"type":"link","attrs":{"href":"javascript:alert(1)"}}),
            json!({"type":"textColor"}),
        ] {
            let document = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"click","marks":[mark]}]}]});
            let projection = project(&document);
            assert!(!projection.editable);
            assert!(!projection.markdown.contains("javascript:"));
        }
    }

    #[test]
    fn unsupported_attributes_cannot_be_lost_by_markdown_editing() {
        for document in [
            json!({"type":"doc","version":1,"content":[{"type":"paragraph","attrs":{"textAlign":"center"},"content":[{"type":"text","text":"Centered"}]}]}),
            json!({"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":"Link","marks":[{"type":"link","attrs":{"href":"https://example.com","title":"Preserve title"}}]}]}]}),
        ] {
            assert!(!project(&document).editable);
        }
    }

    #[test]
    fn empty_content_is_editable() {
        assert!(project(&Value::Null).editable);
        assert!(project(&json!({"type":"doc","version":1,"content":[]})).editable);
    }
}
