//! Bulk update request scripts: several changes to tags, one per line.
//!
//! ```text
//! alias kitty -> cat
//! imply cat -> animal
//! unalias old -> new
//! unimply a -> b
//! update cat_ears solo -> animal_ears -cat_ears
//! category someone -> artist
//! # A comment.
//! ```
//!
//! `update` runs a mass tag edit: the search before the arrow, and after
//! it the tags to add, and to remove with `-`.

use crate::tags::TagName;

/// Most commands in one request.
pub const MAX_COMMANDS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Alias(TagName, TagName),
    Imply(TagName, TagName),
    Unalias(TagName, TagName),
    Unimply(TagName, TagName),
    /// A mass edit: search, tags to add, tags to remove.
    Update {
        query: String,
        add: Vec<TagName>,
        remove: Vec<TagName>,
    },
    /// A tag's new category, by name.
    Category(TagName, String),
}

impl Command {
    /// The command as a script line.
    pub fn line(&self) -> String {
        match self {
            Command::Alias(a, b) => format!("alias {} -> {}", a.as_str(), b.as_str()),
            Command::Imply(a, b) => format!("imply {} -> {}", a.as_str(), b.as_str()),
            Command::Unalias(a, b) => format!("unalias {} -> {}", a.as_str(), b.as_str()),
            Command::Unimply(a, b) => format!("unimply {} -> {}", a.as_str(), b.as_str()),
            Command::Update { query, add, remove } => {
                let changes: Vec<String> = add
                    .iter()
                    .map(|t| t.as_str().to_owned())
                    .chain(remove.iter().map(|t| format!("-{}", t.as_str())))
                    .collect();
                format!("update {query} -> {}", changes.join(" "))
            }
            Command::Category(tag, category) => format!("category {} -> {category}", tag.as_str()),
        }
    }
}

/// A script problem: the line (1-based) and what's wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ScriptError {
    pub line: usize,
    pub message: String,
}

pub fn parse(script: &str) -> Result<Vec<Command>, ScriptError> {
    let mut commands = Vec::new();
    for (i, raw) in script.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let error = |message: String| ScriptError {
            line: i + 1,
            message,
        };
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let (left, right) = rest
            .split_once("->")
            .map(|(l, r)| (l.trim(), r.trim()))
            .ok_or_else(|| error("expected `->` between the two sides".into()))?;
        let tag =
            |raw: &str| TagName::parse(raw).map_err(|e| error(format!("the tag `{raw}` {e}")));
        let pair = || -> Result<(TagName, TagName), ScriptError> {
            let (a, b) = (tag(left)?, tag(right)?);
            if a == b {
                return Err(error("the two tags are the same".into()));
            }
            Ok((a, b))
        };
        let command = match word.to_lowercase().as_str() {
            "alias" => pair().map(|(a, b)| Command::Alias(a, b))?,
            "imply" | "implicate" => pair().map(|(a, b)| Command::Imply(a, b))?,
            "unalias" => pair().map(|(a, b)| Command::Unalias(a, b))?,
            "unimply" => pair().map(|(a, b)| Command::Unimply(a, b))?,
            "update" | "mass" => {
                let parsed = crate::search::Query::parse(left)
                    .map_err(|e| error(format!("the search {e}")))?;
                if parsed.is_empty() {
                    return Err(error("an update needs a search".into()));
                }
                let (mut add, mut remove) = (Vec::new(), Vec::new());
                for word in right.split_whitespace() {
                    match word.strip_prefix('-') {
                        Some(name) => remove.push(tag(name)?),
                        None => add.push(tag(word)?),
                    }
                }
                if add.is_empty() && remove.is_empty() {
                    return Err(error("an update needs tags to add or remove".into()));
                }
                Command::Update {
                    query: parsed.to_string(),
                    add,
                    remove,
                }
            }
            "category" => {
                let category = right.to_lowercase();
                if category.is_empty() || category.contains(char::is_whitespace) {
                    return Err(error("expected a category name after `->`".into()));
                }
                Command::Category(tag(left)?, category)
            }
            other => {
                return Err(error(format!(
                    "unknown command `{other}`; use alias, imply, unalias, unimply, update or category"
                )));
            }
        };
        commands.push(command);
        if commands.len() > MAX_COMMANDS {
            return Err(error(format!(
                "a request can have at most {MAX_COMMANDS} commands"
            )));
        }
    }
    if commands.is_empty() {
        return Err(ScriptError {
            line: 1,
            message: "the script has no commands".into(),
        });
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> TagName {
        TagName::parse(name).unwrap()
    }

    #[test]
    fn parses_commands() {
        let script = "# Cats\nalias Kitty -> cat\n\nimply cat -> animal\nupdate cat_ears  solo -> animal_ears -cat_ears\ncategory Someone -> Artist\nunalias a -> b\nunimply c -> d";
        let commands = parse(script).unwrap();
        assert_eq!(
            commands,
            [
                Command::Alias(t("kitty"), t("cat")),
                Command::Imply(t("cat"), t("animal")),
                Command::Update {
                    query: "cat_ears solo".into(),
                    add: vec![t("animal_ears")],
                    remove: vec![t("cat_ears")],
                },
                Command::Category(t("someone"), "artist".into()),
                Command::Unalias(t("a"), t("b")),
                Command::Unimply(t("c"), t("d")),
            ]
        );
        assert_eq!(
            commands[2].line(),
            "update cat_ears solo -> animal_ears -cat_ears"
        );
    }

    #[test]
    fn explains_mistakes() {
        assert_eq!(parse("alias a b").unwrap_err().line, 1);
        assert!(
            parse("\nzap a -> b")
                .unwrap_err()
                .to_string()
                .starts_with("line 2: unknown command")
        );
        assert!(parse("alias a -> a").unwrap_err().message.contains("same"));
        assert!(
            parse("update -> x")
                .unwrap_err()
                .message
                .contains("needs a search")
        );
        assert!(
            parse("update cat -> ")
                .unwrap_err()
                .message
                .contains("tags to add")
        );
        assert!(parse("# nothing").is_err());
    }
}
