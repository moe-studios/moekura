//! HTML templates (minijinja).
//!
//! Templates are embedded from `crates/web/templates`. A file with the same
//! name in `paths.templates_override` replaces the built-in one, so admins
//! can restyle pages without recompiling. Output is HTML-escaped by default.

use std::path::PathBuf;
use std::sync::Arc;

use minijinja::{Environment, Error, ErrorKind, Value};
use rust_embed::RustEmbed;
use serde::Serialize;

use crate::assets::Assets;

#[derive(RustEmbed)]
#[folder = "templates/"]
struct Embedded;

pub struct Templates {
    env: Environment<'static>,
}

impl Templates {
    /// Builds the environment and compiles every template, so a syntax error
    /// (including in an override) fails startup rather than a page view.
    pub fn load(override_dir: Option<PathBuf>, assets: Arc<Assets>) -> Result<Self, Error> {
        let mut env = Environment::new();
        env.set_loader(move |name| load_source(override_dir.as_ref(), name));
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
        env.add_function("search_url", |query: &str| {
            Value::from_safe_string(search_url(query))
        });
        let templates = Self { env };
        for name in Embedded::iter() {
            templates.env.get_template(&name)?;
        }
        Ok(templates)
    }

    pub fn render(&self, name: &str, context: impl Serialize) -> Result<String, Error> {
        self.env.get_template(name)?.render(context)
    }
}

/// The search results page for `query`. Only URL-safe characters remain
/// after encoding, so the result needs no HTML escaping.
pub fn search_url(query: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    format!("/posts?tags={encoded}")
}

/// A local URL whose query string was built with `form_urlencoded`, ready
/// for an HTML attribute. Encoding leaves `&` as the only character HTML
/// cares about, so escaping it is enough.
pub fn url_value(url: &str) -> Value {
    Value::from_safe_string(url.replace('&', "&amp;"))
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

    #[test]
    fn built_in_templates_compile() {
        Templates::load(None, assets()).unwrap();
    }

    #[test]
    fn escapes_html_by_default() {
        let templates = Templates::load(None, assets()).unwrap();
        let html = templates
            .render(
                "error.html",
                context! { status => 400, message => "<script>alert(1)</script>", site => context! { name => "x" } },
            )
            .unwrap();
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn overrides_replace_templates_and_are_checked_at_load() {
        let dir = std::env::temp_dir().join(format!("uwuu-templates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("error.html"), "custom {{ status }}").unwrap();
        let templates = Templates::load(Some(dir.clone()), assets()).unwrap();
        assert_eq!(
            templates
                .render("error.html", context! { status => 404 })
                .unwrap(),
            "custom 404"
        );

        std::fs::write(dir.join("error.html"), "{% if %}").unwrap();
        assert!(Templates::load(Some(dir.clone()), assets()).is_err());
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
