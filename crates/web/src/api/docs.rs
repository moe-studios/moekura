//! The API reference page, `/api/docs`: the OpenAPI description laid out
//! for people, rendered here so the page needs no scripts and always
//! matches the running version.

use serde_json::Value as Json;

use minijinja::{Value, context};

/// HTML-escapes `text`.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// One paragraph's text with `code` spans, as HTML.
fn inline(text: &str) -> String {
    let mut out = String::new();
    for (i, part) in escape(text).split('`').enumerate() {
        if i % 2 == 1 {
            out.push_str("<code>");
            out.push_str(part);
            out.push_str("</code>");
        } else {
            out.push_str(part);
        }
    }
    out
}

/// Doc-comment text as HTML: paragraphs split by blank lines, with
/// `code` spans. Everything else is escaped.
fn prose(text: &str) -> Value {
    let html: String = text
        .split("\n\n")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .map(|p| format!("<p>{}</p>", inline(&p)))
        .collect();
    Value::from_safe_string(html)
}

/// The schema a `$ref` names.
fn ref_name(schema: &Json) -> Option<&str> {
    schema
        .get("$ref")?
        .as_str()?
        .strip_prefix("#/components/schemas/")
}

/// A short description of a schema's type, as HTML, linking to named
/// schemas.
fn type_html(schema: &Json) -> String {
    if let Some(name) = ref_name(schema) {
        let name = escape(name);
        return format!("<a href=\"#schema-{name}\">{name}</a>");
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(options) = schema.get(key).and_then(Json::as_array) {
            let (nulls, others): (Vec<&Json>, Vec<&Json>) = options
                .iter()
                .partition(|o| o.get("type") == Some(&"null".into()));
            let mut text: Vec<String> = others.into_iter().map(type_html).collect();
            if !nulls.is_empty() {
                text.push("null".to_owned());
            }
            return text.join(" or ");
        }
    }
    if let Some([only]) = schema
        .get("allOf")
        .and_then(Json::as_array)
        .map(Vec::as_slice)
    {
        return type_html(only);
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Json::String(t)) => vec![t.as_str()],
        Some(Json::Array(list)) => list.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    };
    let nullable = types.contains(&"null");
    let base = types.iter().find(|t| **t != "null").copied();
    let mut text = match base {
        Some("array") => format!(
            "array of {}",
            schema
                .get("items")
                .map_or_else(|| "anything".to_owned(), type_html)
        ),
        Some("string") => match schema.get("format").and_then(Json::as_str) {
            Some("date-time") => "date-time string".to_owned(),
            Some("binary") => "file".to_owned(),
            _ => "string".to_owned(),
        },
        Some(other) => other.to_owned(),
        None => "any".to_owned(),
    };
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        let values: Vec<String> = values
            .iter()
            .filter_map(Json::as_str)
            .map(|v| format!("<code>{}</code>", escape(v)))
            .collect();
        text = format!("one of {}", values.join(", "));
    }
    if nullable {
        text.push_str(" or null");
    }
    text
}

/// The description on a schema, or on its single `$ref` alternative.
fn description(schema: &Json) -> &str {
    let own = schema.get("description").and_then(Json::as_str);
    let nested = || {
        ["oneOf", "anyOf", "allOf"].iter().find_map(|key| {
            schema
                .get(*key)?
                .as_array()?
                .iter()
                .find_map(|s| s.get("description")?.as_str())
        })
    };
    own.or_else(nested).unwrap_or_default()
}

/// The fields of an object schema.
fn fields(schema: &Json) -> Vec<Value> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Json::as_array)
        .map(|r| r.iter().filter_map(Json::as_str).collect())
        .unwrap_or_default();
    schema
        .get("properties")
        .and_then(Json::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, field)| {
                    context! {
                        name => name,
                        required => required.contains(&name.as_str()),
                        type_html => Value::from_safe_string(type_html(field)),
                        description => prose(description(field)),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The body of an operation's request or of one response: its content
/// type and schema.
fn content(body: &Json) -> Option<(String, String)> {
    let (content_type, media) = body.get("content")?.as_object()?.iter().next()?;
    let schema = media.get("schema")?;
    Some((content_type.clone(), type_html(schema)))
}

const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

fn operation(path: &str, method: &str, op: &Json) -> Value {
    let text = |key: &str| op.get(key).and_then(Json::as_str).unwrap_or_default();
    let params: Vec<Value> = op
        .get("parameters")
        .and_then(Json::as_array)
        .map(|list| {
            list.iter()
                .map(|p| {
                    let get = |key: &str| p.get(key).and_then(Json::as_str).unwrap_or_default();
                    context! {
                        name => get("name"),
                        location => get("in"),
                        required => p.get("required").and_then(Json::as_bool).unwrap_or(false),
                        type_html => Value::from_safe_string(
                            p.get("schema").map(type_html).unwrap_or_default(),
                        ),
                        description => prose(get("description")),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let body = op.get("requestBody").and_then(content).map(|(ct, ty)| {
        context! { content_type => ct, type_html => Value::from_safe_string(ty) }
    });
    let responses: Vec<Value> = op
        .get("responses")
        .and_then(Json::as_object)
        .map(|map| {
            map.iter()
                .map(|(status, response)| {
                    let body = content(response).map(|(_, ty)| Value::from_safe_string(ty));
                    context! {
                        status => status,
                        description => prose(
                            response.get("description").and_then(Json::as_str).unwrap_or_default(),
                        ),
                        type_html => body,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    context! {
        id => text("operationId"),
        method => method.to_uppercase(),
        path => path,
        summary => text("summary").trim_end_matches('.'),
        description => prose(text("description")),
        params => params,
        body => body,
        responses => responses,
    }
}

/// The page's context for `spec` (the OpenAPI description as JSON).
pub(crate) fn reference(spec: &Json) -> Value {
    let paths = spec.get("paths").and_then(Json::as_object);
    let groups: Vec<Value> = spec
        .get("tags")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .map(|tag| {
            let name = tag.get("name").and_then(Json::as_str).unwrap_or_default();
            let operations: Vec<Value> = paths
                .into_iter()
                .flatten()
                .flat_map(|(path, item)| {
                    METHODS.iter().filter_map(move |method| {
                        let op = item.get(*method)?;
                        let tagged = op
                            .get("tags")
                            .and_then(Json::as_array)
                            .is_some_and(|t| t.iter().any(|t| t.as_str() == Some(name)));
                        tagged.then(|| operation(path, method, op))
                    })
                })
                .collect();
            let description = tag.get("description").and_then(Json::as_str);
            context! {
                name => name,
                description => prose(description.unwrap_or_default()),
                operations => operations,
            }
        })
        .collect();
    let schemas: Vec<Value> = spec
        .pointer("/components/schemas")
        .and_then(Json::as_object)
        .into_iter()
        .flatten()
        .map(|(name, schema)| {
            let is_object = schema.get("properties").is_some();
            context! {
                name => name,
                description => prose(description(schema)),
                fields => fields(schema),
                type_html => (!is_object).then(|| Value::from_safe_string(type_html(schema))),
            }
        })
        .collect();
    let info = |key: &str| {
        spec.pointer(&format!("/info/{key}"))
            .and_then(Json::as_str)
            .unwrap_or_default()
    };
    context! {
        title => info("title"),
        version => info("version"),
        intro => prose(info("description")),
        groups => groups,
        schemas => schemas,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn describes_types() {
        assert_eq!(
            type_html(&json!({"$ref": "#/components/schemas/ApiPost"})),
            "<a href=\"#schema-ApiPost\">ApiPost</a>"
        );
        assert_eq!(
            type_html(&json!({"type": "array", "items": {"type": "string"}})),
            "array of string"
        );
        assert_eq!(
            type_html(&json!({"type": ["integer", "null"], "format": "int64"})),
            "integer or null"
        );
        assert_eq!(
            type_html(&json!({"oneOf": [{"type": "null"}, {"$ref": "#/components/schemas/A"}]})),
            "<a href=\"#schema-A\">A</a> or null"
        );
        assert_eq!(
            type_html(&json!({"type": "string", "enum": ["alias", "implication"]})),
            "one of <code>alias</code>, <code>implication</code>"
        );
        assert_eq!(
            type_html(&json!({"type": "string", "format": "date-time"})),
            "date-time string"
        );
    }

    #[test]
    fn renders_prose_safely() {
        assert_eq!(
            prose("Needs `view_posts`.\nSecond\nline.\n\n<b>Two</b>").to_string(),
            "<p>Needs <code>view_posts</code>. Second line.</p><p>&lt;b&gt;Two&lt;/b&gt;</p>"
        );
    }
}
