//! HTML templates (minijinja).
//!
//! Templates are embedded from `crates/web/templates`. A file with the same
//! name in `paths.templates_override` replaces the built-in one, so admins
//! can restyle pages without recompiling. Output is HTML-escaped by default.

use std::path::PathBuf;
use std::sync::Arc;

use minijinja::value::Kwargs;
use minijinja::{Environment, Error, ErrorKind, State, Value};
use rust_embed::RustEmbed;
use serde::Serialize;

use crate::assets::Assets;
use crate::i18n::{self, Locales};

#[derive(RustEmbed)]
#[folder = "templates/"]
struct Embedded;

pub struct Templates {
    env: Environment<'static>,
}

impl Templates {
    /// Builds the environment and compiles every template, so a syntax error
    /// (including in an override) fails startup rather than a page view.
    pub fn load(
        override_dir: Option<PathBuf>,
        assets: Arc<Assets>,
        locales: Arc<Locales>,
    ) -> Result<Self, Error> {
        let templates = Self {
            env: environment(override_dir, assets, locales),
        };
        for name in Embedded::iter() {
            templates.env.get_template(&name)?;
        }
        Ok(templates)
    }

    pub fn render(&self, name: &str, context: impl Serialize) -> Result<String, Error> {
        self.env.get_template(name)?.render(context)
    }
}

/// The template environment, with the functions templates use.
fn environment(
    override_dir: Option<PathBuf>,
    assets: Arc<Assets>,
    locales: Arc<Locales>,
) -> Environment<'static> {
    let mut env = Environment::new();
    // `t("key", name=value, …)`: message `key` in the page's language
    // (`lang`), its values escaped unless safe.
    env.add_function(
        "t",
        move |state: &State, key: &str, kwargs: Kwargs| -> Result<Value, Error> {
            let lang = i18n::rendering()
                .or_else(|| state.lookup("lang")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| i18n::DEFAULT.to_owned());
            let lang = lang.as_str();
            let mut args = fluent_bundle::FluentArgs::new();
            for name in kwargs.args() {
                let value: Value = kwargs.get(name)?;
                args.set(name.to_owned(), i18n::argument(&value));
            }
            Ok(Value::from_safe_string(locales.format(
                lang,
                key,
                Some(&args),
            )))
        },
    );
    env.add_global("build_version", env!("MOEKURA_BUILD_VERSION"));
    env.set_loader(move |name| load_source(override_dir.as_ref(), name));
    let icons = assets.clone();
    // Asset URLs are built by us from hex hashes and embedded paths, so
    // they are marked safe; escaping would turn `/` into `&#x2f;`.
    env.add_function("asset", move |path: &str| {
        assets
            .url(path)
            .map(|url| Value::from_safe_string(url.to_owned()))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    format!("unknown asset `{path}`"),
                )
            })
    });
    // The site a link is on, if it's one we know: its key, name and
    // icon's URL (its favicon, or its initials where that couldn't be
    // fetched; see scripts/fetch-site-icons.py).
    env.add_function("site_of", move |url: &str| {
        moekura_core::sites::site_of(url).map_or(Value::UNDEFINED, |site| {
            let icon = ["png", "svg"]
                .iter()
                .find_map(|ext| icons.url(&format!("site-icons/{}.{ext}", site.key)))
                .map(|url| Value::from_safe_string(url.to_owned()));
            minijinja::context! { key => site.key, name => site.name, icon }
        })
    });
    env.add_function("search_url", |query: &str| {
        Value::from_safe_string(search_url(query))
    });
    env
}

/// The search results page for `query`. Only URL-safe characters remain
/// after encoding, so the result needs no HTML escaping.
pub fn search_url(query: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    format!("/posts?tags={encoded}")
}

/// A URL ready for an HTML attribute or text. It may hold anything a
/// stored or looked-up link can (quotes, angle brackets), so every
/// character HTML cares about is escaped; only `/` is left as it is,
/// which autoescaping would turn into `&#x2f;` (valid, but some link
/// scrapers don't undo it).
pub fn url_value(url: &str) -> Value {
    Value::from_safe_string(crate::i18n::escape(url))
}

fn load_source(override_dir: Option<&PathBuf>, name: &str) -> Result<Option<String>, Error> {
    // Template names come from our own code, but keep overrides inside their directory.
    if name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Ok(None);
    }
    if let Some(path) = override_dir
        .map(|dir| dir.join(name))
        .filter(|p| p.is_file())
    {
        return std::fs::read_to_string(&path).map(Some).map_err(|e| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("reading {}: {e}", path.display()),
            )
        });
    }
    Ok(Embedded::get(name).map(|file| String::from_utf8_lossy(&file.data).into_owned()))
}

#[cfg(test)]
mod tests {
    use minijinja::context;

    use super::*;

    fn assets() -> Arc<Assets> {
        Arc::new(Assets::load(None).unwrap())
    }

    fn locales() -> Arc<Locales> {
        Arc::new(Locales::load(None).unwrap())
    }

    /// Every site links are recognised on has an icon.
    #[test]
    fn sites_have_icons() {
        let assets = assets();
        let missing: Vec<_> = moekura_core::sites::ALL
            .iter()
            .filter(|site| {
                ["png", "svg"].iter().all(|ext| {
                    assets
                        .url(&format!("site-icons/{}.{ext}", site.key))
                        .is_none()
                })
            })
            .map(|site| site.key)
            .collect();
        assert!(
            missing.is_empty(),
            "no icon in static/site-icons for {missing:?}"
        );
    }

    /// Every `t("key")` with a literal key names a message.
    #[test]
    fn templates_use_existing_messages() {
        let locales = locales();
        let mut missing = Vec::new();
        for name in Embedded::iter() {
            let file = Embedded::get(&name).unwrap();
            let text = String::from_utf8_lossy(&file.data);
            for (i, _) in text.match_indices("t(") {
                // Only calls: `t(` not ending another name.
                if text[..i].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                    continue;
                }
                let rest = &text[i + 2..];
                let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                    continue;
                };
                let Some(end) = rest[1..].find(quote) else {
                    continue;
                };
                let key = &rest[1..=end];
                // Keys built at render time (`"status-" ~ …`) can't be checked.
                if rest[end + 2..].trim_start().starts_with('~') {
                    continue;
                }
                if !locales.has_message(key) {
                    missing.push(format!("{name}: {key}"));
                }
            }
        }
        assert!(missing.is_empty(), "missing messages: {missing:#?}");
    }

    #[test]
    fn built_in_templates_compile() {
        Templates::load(None, assets(), locales()).unwrap();
    }

    #[test]
    fn escapes_html_by_default() {
        let templates = Templates::load(None, assets(), locales()).unwrap();
        let html = templates
            .render(
                "error.html",
                context! { status => 400, message => "<script>alert(1)</script>", site => context! { name => "x" } },
            )
            .unwrap();
        assert!(
            html.contains(&format!("Moekura</a> {}", env!("MOEKURA_BUILD_VERSION"))),
            "{html}"
        );
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn urls_are_escaped_in_attributes() {
        assert_eq!(
            url_value("/posts?tags=cat&page=2").to_string(),
            "/posts?tags=cat&amp;page=2"
        );
        // What a looked-up page or a stored link may hold.
        let evil = "https://example.com/a\"><img src=x>'b' c";
        assert_eq!(
            url_value(evil).to_string(),
            "https://example.com/a&quot;&gt;&lt;img src=x&gt;&#x27;b&#x27; c"
        );
        let templates = Templates::load(None, assets(), locales()).unwrap();
        let html = templates
            .render(
                "upload_source.html",
                context! {
                    source => context! {
                        site => "Example",
                        page_url => url_value(evil),
                        artist_name => "someone",
                        profiles => vec![url_value(evil)],
                    },
                },
            )
            .unwrap();
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("/a\""), "{html}");
        assert!(
            html.contains(
                "href=\"https://example.com/a&quot;&gt;&lt;img src=x&gt;&#x27;b&#x27; c\""
            ),
            "{html}"
        );
    }

    #[test]
    fn overrides_replace_templates_and_are_checked_at_load() {
        let dir = std::env::temp_dir().join(format!("moekura-templates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("error.html"), "custom {{ status }}").unwrap();
        let templates = Templates::load(Some(dir.clone()), assets(), locales()).unwrap();
        assert_eq!(
            templates
                .render("error.html", context! { status => 404 })
                .unwrap(),
            "custom 404"
        );

        std::fs::write(dir.join("error.html"), "{% if %}").unwrap();
        assert!(Templates::load(Some(dir.clone()), assets(), locales()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The CSP forbids inline styles and scripts; browsers would silently
    /// ignore them, so keep them out of the templates altogether.
    #[test]
    fn templates_have_no_inline_styles_or_scripts() {
        for name in Embedded::iter() {
            let source = load_source(None, &name).unwrap().unwrap();
            for forbidden in [
                " style=",
                "<style",
                "<script>",
                " onclick=",
                " onload=",
                "javascript:",
            ] {
                assert!(!source.contains(forbidden), "{name} contains `{forbidden}`");
            }
        }
    }

    #[test]
    fn refuses_path_traversal() {
        assert_eq!(load_source(None, "../Cargo.toml").unwrap(), None);
        assert_eq!(load_source(None, "/etc/passwd").unwrap(), None);
    }
}
