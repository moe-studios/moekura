//! Translations, in [Fluent](https://projectfluent.org/).
//!
//! Messages live in `crates/web/locales/<language>/*.ftl`, embedded in
//! the binary. `paths.locales_override` holds more, laid out the same way:
//! a new language's directory adds the language, and files for a built-in
//! one replace the messages they define. Each page is shown in the
//! reader's chosen language, else the best match for their browser's
//! `Accept-Language`, else [`DEFAULT`]; messages a language lacks come
//! from [`DEFAULT`].
//!
//! Messages are HTML, as trusted as templates; the values put into them
//! are escaped (unless already safe).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use fluent_langneg::{NegotiationStrategy, negotiate_languages};
use rust_embed::RustEmbed;
use unic_langid::LanguageIdentifier;

#[derive(RustEmbed)]
#[folder = "locales/"]
struct Embedded;

/// The language every message exists in.
pub const DEFAULT: &str = "en-US";

#[derive(Debug, thiserror::Error)]
pub enum LocaleError {
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

struct Language {
    id: LanguageIdentifier,
    bundle: FluentBundle<FluentResource>,
    /// Its messages' keys (bundles don't list them).
    keys: Vec<String>,
}

pub struct Locales {
    /// [`DEFAULT`] first, then the rest by tag.
    languages: Vec<Language>,
}

impl Locales {
    /// Loads the built-in messages and those in `override_dir`; a file
    /// that doesn't parse, or defines a message twice among the built-in
    /// ones, fails startup.
    pub fn load(override_dir: Option<&Path>) -> Result<Self, LocaleError> {
        let mut sources: BTreeMap<String, Vec<(String, String, bool)>> = BTreeMap::new();
        for path in Embedded::iter() {
            let Some((tag, _)) = path.split_once('/').filter(|_| path.ends_with(".ftl")) else {
                continue;
            };
            let file = Embedded::get(&path).expect("listed files exist");
            let text = String::from_utf8_lossy(&file.data).into_owned();
            sources
                .entry(tag.to_owned())
                .or_default()
                .push((path.to_string(), text, false));
        }
        if let Some(dir) = override_dir {
            for (tag, path) in override_files(dir)? {
                let text = std::fs::read_to_string(&path).map_err(|source| LocaleError::Read {
                    path: path.clone(),
                    source,
                })?;
                sources
                    .entry(tag)
                    .or_default()
                    .push((path.display().to_string(), text, true));
            }
        }
        let mut languages = Vec::new();
        for (tag, files) in sources {
            let id: LanguageIdentifier = tag.parse().map_err(|_| LocaleError::Invalid {
                path: tag.clone(),
                message: "not a language tag".into(),
            })?;
            let mut bundle = FluentBundle::new_concurrent(vec![id.clone()]);
            // Bidi isolation marks would end up inside attributes and URLs.
            bundle.set_use_isolating(false);
            let mut keys = Vec::new();
            for (path, text, overriding) in files {
                keys.extend(message_keys(&text));
                let resource =
                    FluentResource::try_new(text).map_err(|(_, errors)| LocaleError::Invalid {
                        path: path.clone(),
                        message: format!("{errors:?}"),
                    })?;
                if overriding {
                    bundle.add_resource_overriding(resource);
                } else {
                    bundle
                        .add_resource(resource)
                        .map_err(|errors| LocaleError::Invalid {
                            path,
                            message: format!("{errors:?}"),
                        })?;
                }
            }
            languages.push(Language { id, bundle, keys });
        }
        languages.sort_by_key(|l| (l.id != DEFAULT, l.id.to_string()));
        if languages.first().is_none_or(|l| l.id != DEFAULT) {
            return Err(LocaleError::Invalid {
                path: DEFAULT.into(),
                message: "the default language is missing".into(),
            });
        }
        Ok(Self { languages })
    }

    /// Every language's tag and its name for itself (its `language-name`
    /// message).
    pub fn languages(&self) -> Vec<(String, String)> {
        self.languages
            .iter()
            .map(|l| {
                let tag = l.id.to_string();
                let name = self.format(&tag, "language-name", None);
                (tag, name)
            })
            .collect()
    }

    /// Whether the default language has message `key`.
    #[cfg(test)]
    pub fn has_message(&self, key: &str) -> bool {
        self.languages[0].bundle.has_message(key)
    }

    pub fn has(&self, tag: &str) -> bool {
        self.languages.iter().any(|l| l.id == tag)
    }

    /// The language to show someone: `chosen` if the site has it, else the
    /// best match for `accept_language`, else [`DEFAULT`].
    pub fn negotiate(&self, chosen: Option<&str>, accept_language: Option<&str>) -> String {
        if let Some(tag) = chosen.filter(|tag| self.has(tag)) {
            return tag.to_owned();
        }
        let requested: Vec<LanguageIdentifier> = accept_language
            .map(parse_accept_language)
            .unwrap_or_default();
        let available: Vec<LanguageIdentifier> =
            self.languages.iter().map(|l| l.id.clone()).collect();
        let default = &self.languages[0].id;
        negotiate_languages(
            &requested,
            &available,
            Some(default),
            NegotiationStrategy::Lookup,
        )
        .first()
        .map_or_else(|| DEFAULT.to_owned(), ToString::to_string)
    }

    /// Message `key` in language `tag`, falling back to [`DEFAULT`], then
    /// to the key itself.
    pub fn format(&self, tag: &str, key: &str, args: Option<&FluentArgs>) -> String {
        let wanted = self.languages.iter().find(|l| l.id == tag);
        for language in wanted.into_iter().chain(self.languages.first()) {
            let Some(pattern) = language.bundle.get_message(key).and_then(|m| m.value()) else {
                continue;
            };
            let mut errors = Vec::new();
            let text = language.bundle.format_pattern(pattern, args, &mut errors);
            if !errors.is_empty() {
                tracing::debug!(key, tag, ?errors, "message formatted with errors");
            }
            return text.into_owned();
        }
        tracing::warn!(key, "no such message");
        key.to_owned()
    }

    /// Message `key` with text arguments, for code (rather than
    /// templates); the arguments aren't escaped.
    pub fn say(&self, tag: &str, key: &str, args: &[(&str, &str)]) -> String {
        let mut fluent = FluentArgs::new();
        for (name, value) in args {
            fluent.set(*name, FluentValue::from((*value).to_owned()));
        }
        self.format(tag, key, Some(&fluent))
    }

    /// Every message whose key starts with `prefix`, in language `tag`
    /// (falling back like [`Locales::format`]), unformatted variables left
    /// as `{$name}` for the caller to fill in.
    pub fn with_prefix(&self, tag: &str, prefix: &str) -> BTreeMap<String, String> {
        let mut keys: Vec<&str> = self
            .languages
            .iter()
            .flat_map(|l| &l.keys)
            .map(String::as_str)
            .filter(|key| key.starts_with(prefix))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys.into_iter()
            .map(|key| {
                let text = self.format(tag, key, None);
                (key.to_owned(), text)
            })
            .collect()
    }
}

/// The keys of the messages (not terms) a file defines: identifiers
/// starting a line, before `=`.
fn message_keys(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines().filter_map(|line| {
        let (key, _) = line.split_once('=')?;
        let key = key.trim_end();
        (key.starts_with(|c: char| c.is_ascii_alphabetic())
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .then(|| key.to_owned())
    })
}

/// `<dir>/<tag>/*.ftl`, as `(tag, path)`.
fn override_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, LocaleError> {
    let read = |path: &Path| {
        std::fs::read_dir(path).map_err(|source| LocaleError::Read {
            path: path.to_owned(),
            source,
        })
    };
    let mut found = Vec::new();
    for entry in read(dir)? {
        let entry = entry.map_err(|source| LocaleError::Read {
            path: dir.to_owned(),
            source,
        })?;
        let path = entry.path();
        let Some(tag) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        let mut files: Vec<PathBuf> = read(&path)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ftl"))
            .collect();
        files.sort();
        found.extend(files.into_iter().map(|file| (tag.clone(), file)));
    }
    found.sort();
    Ok(found)
}

/// The languages in an `Accept-Language` header, most wanted first.
fn parse_accept_language(header: &str) -> Vec<LanguageIdentifier> {
    let mut weighted: Vec<(f32, usize, LanguageIdentifier)> = header
        .split(',')
        .enumerate()
        .filter_map(|(i, part)| {
            let mut pieces = part.trim().split(';');
            let tag = pieces.next()?.trim();
            let q = pieces
                .find_map(|p| p.trim().strip_prefix("q="))
                .map_or(Some(1.0), |q| q.trim().parse::<f32>().ok())?;
            let id = tag.parse().ok().filter(|_| tag != "*" && q > 0.0)?;
            Some((q, i, id))
        })
        .collect();
    weighted.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    weighted.into_iter().map(|(.., id)| id).collect()
}

thread_local! {
    /// The language of the page being rendered on this thread.
    static RENDERING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Runs `render` with pages' messages in language `tag` (rendering is
/// synchronous, so a thread-local reaches every template and macro).
pub(crate) fn rendering_in<T>(tag: &str, render: impl FnOnce() -> T) -> T {
    let previous = RENDERING.with(|lang| lang.replace(Some(tag.to_owned())));
    let result = render();
    RENDERING.with(|lang| *lang.borrow_mut() = previous);
    result
}

/// The language set by [`rendering_in`], if any.
pub(crate) fn rendering() -> Option<String> {
    RENDERING.with(|lang| lang.borrow().clone())
}

/// Escapes text for HTML, for values put into messages.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// A template value as a message argument: whole numbers stay numbers
/// (for plurals), anything else becomes escaped text.
pub(crate) fn argument(value: &minijinja::Value) -> FluentValue<'static> {
    use minijinja::value::ValueKind;
    match value.kind() {
        // Whole numbers stay numbers, for plurals; others keep the way
        // templates show them (`100.0`).
        ValueKind::Number if value.as_i64().is_some() && !value.to_string().contains('.') => {
            FluentValue::from(value.as_i64().unwrap_or_default())
        }
        ValueKind::Undefined | ValueKind::None => FluentValue::from(""),
        _ if value.is_safe() => FluentValue::from(value.to_string()),
        _ => FluentValue::from(escape(&value.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_messages_load() {
        let locales = Locales::load(None).unwrap();
        assert_eq!(locales.format(DEFAULT, "language-name", None), "English");
        assert_eq!(locales.format("xx", "language-name", None), "English");
        assert_eq!(
            locales.format(DEFAULT, "no-such-message", None),
            "no-such-message"
        );
    }

    #[test]
    fn overrides_add_languages_and_replace_messages() {
        let dir = std::env::temp_dir().join(format!("moekura-locales-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("de")).unwrap();
        std::fs::create_dir_all(dir.join("en-US")).unwrap();
        std::fs::write(dir.join("de/main.ftl"), "language-name = Deutsch\n").unwrap();
        std::fs::write(
            dir.join("en-US/mine.ftl"),
            "language-name = English (ours)\n",
        )
        .unwrap();
        let locales = Locales::load(Some(&dir)).unwrap();
        assert_eq!(
            locales.languages(),
            [
                ("en-US".to_owned(), "English (ours)".to_owned()),
                ("de".to_owned(), "Deutsch".to_owned())
            ]
        );
        // What German lacks comes from English.
        assert_eq!(locales.format("de", "common-save", None), "Save");
        std::fs::write(dir.join("de/broken.ftl"), "= nope\n").unwrap();
        assert!(Locales::load(Some(&dir)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn negotiates() {
        let dir = std::env::temp_dir().join(format!("moekura-negotiate-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("de")).unwrap();
        std::fs::write(dir.join("de/main.ftl"), "language-name = Deutsch\n").unwrap();
        let locales = Locales::load(Some(&dir)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(locales.negotiate(None, None), "en-US");
        assert_eq!(
            locales.negotiate(None, Some("fr;q=0.9, de-AT;q=0.8, en;q=0.1")),
            "de"
        );
        assert_eq!(locales.negotiate(None, Some("en-GB,de;q=0.5")), "en-US");
        assert_eq!(locales.negotiate(Some("de"), Some("en")), "de");
        assert_eq!(locales.negotiate(Some("xx"), Some("fr")), "en-US");
    }

    #[test]
    fn arguments_are_escaped_unless_safe() {
        let text = minijinja::Value::from("<b>");
        assert_eq!(argument(&text), FluentValue::from("&lt;b&gt;"));
        let safe = minijinja::Value::from_safe_string("<b>".into());
        assert_eq!(argument(&safe), FluentValue::from("<b>"));
        assert_eq!(argument(&minijinja::Value::from(3)), FluentValue::from(3));
        assert_eq!(
            argument(&minijinja::Value::from(99.5)),
            FluentValue::from("99.5")
        );
    }
}
