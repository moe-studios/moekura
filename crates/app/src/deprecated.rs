//! Old names for configuration keys and command-line flags, kept working
//! after a rename until the next major release.
//!
//! Renaming a config key: change the field, then add a [`Rename`] to
//! [`CONFIG_KEYS`]. The old key (in the file, or as its `MOEKURA_*`
//! variable) is moved to the new one before the configuration is read, and
//! a warning naming the new key is logged at startup.
//!
//! Renaming a flag or subcommand: change it, then add a [`Rename`] to
//! [`CLI_NAMES`]. The old spelling is rewritten before clap sees it, so it
//! stays out of `--help`, and a warning is printed.
//!
//! See docs/src/stability.md for the policy.

use std::sync::{Arc, Mutex};

use figment::value::{Dict, Map, Value};
use figment::{Metadata, Profile, Provider};

pub struct Rename {
    /// For config keys, a dotted path (`server.request_timeout_secs`). For
    /// the CLI, the subcommands leading to it then the old spelling:
    /// `"admin seed --batch"`, or `"admin old-name"` for a subcommand.
    pub old: &'static str,
    /// The new key, or the new spelling (without the command path).
    pub new: &'static str,
    /// The release that renamed it.
    pub since: &'static str,
}

/// Config keys that were renamed, oldest first.
pub const CONFIG_KEYS: &[Rename] = &[];

/// Flags and subcommands that were renamed, oldest first.
pub const CLI_NAMES: &[Rename] = &[];

/// Warnings about old names, collected while loading.
pub type Notices = Arc<Mutex<Vec<String>>>;

/// Where a provider's keys come from, for naming them in warnings.
#[derive(Clone, Copy)]
pub enum Source {
    File,
    Env { prefix: &'static str },
}

impl Source {
    fn name(self, path: &str) -> String {
        match self {
            Source::File => format!("`{path}`"),
            Source::Env { prefix } => {
                format!("{prefix}{}", path.replace('.', "__").to_uppercase())
            }
        }
    }
}

/// A provider with renamed keys moved to their new place.
pub struct Renamed<P> {
    inner: P,
    renames: &'static [Rename],
    source: Source,
    notices: Notices,
}

impl<P> Renamed<P> {
    pub fn new(inner: P, renames: &'static [Rename], source: Source, notices: Notices) -> Self {
        Renamed {
            inner,
            renames,
            source,
            notices,
        }
    }
}

impl<P: Provider> Provider for Renamed<P> {
    fn metadata(&self) -> Metadata {
        self.inner.metadata()
    }

    fn data(&self) -> figment::Result<Map<Profile, Dict>> {
        let mut data = self.inner.data()?;
        for dict in data.values_mut() {
            for rename in self.renames {
                let Some(value) = take(dict, rename.old) else {
                    continue;
                };
                let (old, new) = (self.source.name(rename.old), self.source.name(rename.new));
                if find(dict, rename.new).is_some() {
                    return Err(
                        format!("both {new} and its old name {old} are set; remove {old}").into(),
                    );
                }
                insert(dict, rename.new, value);
                let notice = format!(
                    "{old} was renamed to {new} in {}; the old name stops working in the next major release",
                    rename.since
                );
                let mut notices = self.notices.lock().expect("not poisoned");
                if !notices.contains(&notice) {
                    notices.push(notice);
                }
            }
        }
        Ok(data)
    }
}

fn find<'a>(dict: &'a Dict, path: &str) -> Option<&'a Value> {
    let (head, rest) = split(path);
    match (dict.get(head)?, rest) {
        (value, None) => Some(value),
        (Value::Dict(_, inner), Some(rest)) => find(inner, rest),
        _ => None,
    }
}

/// Removes the value at `path`, and any tables left empty by that.
fn take(dict: &mut Dict, path: &str) -> Option<Value> {
    let (head, rest) = split(path);
    let Some(rest) = rest else {
        return dict.remove(head);
    };
    let Some(Value::Dict(_, inner)) = dict.get_mut(head) else {
        return None;
    };
    let value = take(inner, rest)?;
    if inner.is_empty() {
        dict.remove(head);
    }
    Some(value)
}

fn insert(dict: &mut Dict, path: &str, value: Value) {
    let (head, rest) = split(path);
    match rest {
        None => {
            dict.insert(head.to_owned(), value);
        }
        Some(rest) => {
            let entry = dict
                .entry(head.to_owned())
                .or_insert_with(|| Value::from(Dict::new()));
            if !matches!(entry, Value::Dict(..)) {
                *entry = Value::from(Dict::new());
            }
            if let Value::Dict(_, inner) = entry {
                insert(inner, rest, value);
            }
        }
    }
}

fn split(path: &str) -> (&str, Option<&str>) {
    match path.split_once('.') {
        Some((head, rest)) => (head, Some(rest)),
        None => (path, None),
    }
}

/// Rewrites old spellings of flags and subcommands in `args` (the program
/// name first) to the current ones, returning a warning for each.
pub fn rewrite_args(
    command: &clap::Command,
    renames: &[Rename],
    args: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let mut out = Vec::with_capacity(args.len());
    let mut warnings = Vec::new();
    let mut args = args.into_iter();
    out.extend(args.next());
    // The subcommands seen so far, and the commands they lead to.
    let mut path: Vec<String> = Vec::new();
    let mut commands = vec![command.clone()];
    let mut positional_only = false;
    while let Some(arg) = args.next() {
        if positional_only || arg == "-" {
            out.push(arg);
            continue;
        }
        if arg == "--" {
            positional_only = true;
            out.push(arg);
            continue;
        }
        let current = commands.last().expect("starts with the root");
        if let Some(flag) = arg.strip_prefix("--") {
            let (name, value) = match flag.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (flag, None),
            };
            let mut name = name.to_owned();
            let old = format!("{} --{name}", path.join(" "));
            if let Some(rename) = renames.iter().find(|r| r.old.trim() == old.trim()) {
                warnings.push(notice(&old, &format!("{} {}", path.join(" "), rename.new)));
                name = rename.new.trim_start_matches('-').to_owned();
            }
            let takes_value = value.is_none()
                && commands.iter().rev().any(|command| {
                    command.get_arguments().any(|a| {
                        a.get_long() == Some(name.as_str())
                            && a.get_action().takes_values()
                            && a.get_num_args().is_none_or(|n| n.min_values() > 0)
                    })
                });
            out.push(match value {
                Some(value) => format!("--{name}={value}"),
                None => format!("--{name}"),
            });
            if takes_value {
                out.extend(args.next());
            }
            continue;
        }
        if arg.starts_with('-') {
            // Short flags: `-c path`.
            let takes_value = arg.len() == 2
                && commands.iter().rev().any(|command| {
                    command.get_arguments().any(|a| {
                        a.get_short().map(String::from).as_deref() == Some(&arg[1..])
                            && a.get_action().takes_values()
                    })
                });
            out.push(arg);
            if takes_value {
                out.extend(args.next());
            }
            continue;
        }
        let old = format!("{} {arg}", path.join(" "));
        let name = match renames.iter().find(|r| r.old.trim() == old.trim()) {
            Some(rename) => {
                warnings.push(notice(&old, &format!("{} {}", path.join(" "), rename.new)));
                rename.new.to_owned()
            }
            None => arg,
        };
        if let Some(sub) = current.find_subcommand(&name).cloned() {
            path.push(sub.get_name().to_owned());
            commands.push(sub);
        }
        out.push(name);
    }
    (out, warnings)
}

fn notice(old: &str, new: &str) -> String {
    format!(
        "`moekura {}` is deprecated, use `moekura {}`; the old name stops working in the next major release",
        old.trim(),
        new.trim()
    )
}

#[cfg(test)]
// `Jail` closures must return `figment::Result`, whose error type is large.
#[allow(clippy::result_large_err)]
mod tests {
    use clap::{Arg, ArgAction, Command};
    use figment::providers::{Env, Format, Serialized, Toml};
    use figment::{Figment, Jail};
    use serde::Deserialize;

    use super::*;

    const RENAMES: &[Rename] = &[
        Rename {
            old: "server.timeout",
            new: "server.request_timeout_secs",
            since: "1.1.0",
        },
        Rename {
            old: "uploads.max_bytes",
            new: "media.max_upload_bytes",
            since: "1.2.0",
        },
    ];

    #[derive(Deserialize, serde::Serialize, Default, Debug, PartialEq)]
    #[serde(deny_unknown_fields, default)]
    struct Config {
        server: Server,
        media: Media,
    }

    #[derive(Deserialize, serde::Serialize, Default, Debug, PartialEq)]
    #[serde(deny_unknown_fields, default)]
    struct Server {
        request_timeout_secs: u64,
        bind: String,
    }

    #[derive(Deserialize, serde::Serialize, Default, Debug, PartialEq)]
    #[serde(deny_unknown_fields, default)]
    struct Media {
        max_upload_bytes: u64,
    }

    fn load(notices: &Notices) -> figment::Result<Config> {
        Figment::from(Serialized::defaults(Config::default()))
            .merge(Renamed::new(
                Toml::file_exact("moekura.toml"),
                RENAMES,
                Source::File,
                notices.clone(),
            ))
            .merge(Renamed::new(
                Env::prefixed("MOEKURA_").split("__"),
                RENAMES,
                Source::Env { prefix: "MOEKURA_" },
                notices.clone(),
            ))
            .extract()
    }

    #[test]
    fn old_config_keys_still_work_with_a_warning() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "moekura.toml",
                "[server]\ntimeout = 12\nbind = \"x\"\n[uploads]\nmax_bytes = 5\n",
            )?;
            let notices = Notices::default();
            let config = load(&notices)?;
            assert_eq!(config.server.request_timeout_secs, 12);
            assert_eq!(config.server.bind, "x");
            assert_eq!(config.media.max_upload_bytes, 5);
            let notices = notices.lock().unwrap();
            assert_eq!(notices.len(), 2, "{notices:?}");
            assert!(
                notices[0].starts_with(
                    "`server.timeout` was renamed to `server.request_timeout_secs` in 1.1.0"
                ),
                "{notices:?}"
            );
            Ok(())
        });
    }

    #[test]
    fn old_environment_variables_still_work() {
        Jail::expect_with(|jail| {
            jail.create_file("moekura.toml", "")?;
            jail.set_env("MOEKURA_SERVER__TIMEOUT", "7");
            let notices = Notices::default();
            assert_eq!(load(&notices)?.server.request_timeout_secs, 7);
            assert!(notices.lock().unwrap()[0].starts_with(
                "MOEKURA_SERVER__TIMEOUT was renamed to MOEKURA_SERVER__REQUEST_TIMEOUT_SECS"
            ),);
            Ok(())
        });
    }

    #[test]
    fn a_new_variable_beats_an_old_key_in_the_file() {
        Jail::expect_with(|jail| {
            jail.create_file("moekura.toml", "[server]\ntimeout = 12\n")?;
            jail.set_env("MOEKURA_SERVER__REQUEST_TIMEOUT_SECS", "9");
            assert_eq!(load(&Notices::default())?.server.request_timeout_secs, 9);
            Ok(())
        });
    }

    #[test]
    fn setting_both_names_is_an_error() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "moekura.toml",
                "[server]\ntimeout = 12\nrequest_timeout_secs = 3\n",
            )?;
            let error = load(&Notices::default()).unwrap_err().to_string();
            assert!(
                error.contains(
                    "both `server.request_timeout_secs` and its old name `server.timeout` are set"
                ),
                "{error}"
            );
            Ok(())
        });
    }

    #[test]
    fn new_names_alone_give_no_warning() {
        Jail::expect_with(|jail| {
            jail.create_file("moekura.toml", "[server]\nrequest_timeout_secs = 3\n")?;
            let notices = Notices::default();
            assert_eq!(load(&notices)?.server.request_timeout_secs, 3);
            assert!(notices.lock().unwrap().is_empty());
            Ok(())
        });
    }

    fn cli() -> Command {
        Command::new("moekura")
            .arg(Arg::new("config").short('c').long("config").global(true))
            .subcommand(
                Command::new("admin").subcommand(
                    Command::new("seed")
                        .arg(Arg::new("batch-size").long("batch-size"))
                        .arg(Arg::new("force").long("force").action(ArgAction::SetTrue)),
                ),
            )
    }

    const CLI_RENAMES: &[Rename] = &[
        Rename {
            old: "admin seed --batch",
            new: "--batch-size",
            since: "1.1.0",
        },
        Rename {
            old: "admin fill",
            new: "seed",
            since: "1.1.0",
        },
    ];

    fn rewrite(line: &str) -> (String, Vec<String>) {
        let args = line.split(' ').map(String::from).collect();
        let (args, warnings) = rewrite_args(&cli(), CLI_RENAMES, args);
        (args.join(" "), warnings)
    }

    #[test]
    fn old_flags_and_subcommands_are_rewritten() {
        let (args, warnings) = rewrite("moekura -c batch admin fill --batch 5 --force");
        assert_eq!(args, "moekura -c batch admin seed --batch-size 5 --force");
        assert_eq!(
            warnings,
            [
                "`moekura admin fill` is deprecated, use `moekura admin seed`; the old name stops working in the next major release",
                "`moekura admin seed --batch` is deprecated, use `moekura admin seed --batch-size`; the old name stops working in the next major release",
            ]
        );
        assert_eq!(
            rewrite("moekura admin seed --batch=5").0,
            "moekura admin seed --batch-size=5"
        );
    }

    #[test]
    fn current_names_and_values_are_left_alone() {
        for line in [
            "moekura admin seed --batch-size 5",
            // A value that happens to look like an old name.
            "moekura --config fill admin seed",
            "moekura admin seed -- --batch",
            // Only under its own subcommand.
            "moekura --batch admin",
        ] {
            let (args, warnings) = rewrite(line);
            assert_eq!(args, line);
            assert!(warnings.is_empty(), "{line}: {warnings:?}");
        }
    }
}

#[cfg(test)]
mod current {
    use figment::Figment;
    use figment::providers::Serialized;
    use moekura_core::config::Config;

    /// Each old key leads to a key that exists, and no longer exists
    /// itself.
    #[test]
    fn renamed_config_keys_point_at_current_ones() {
        let defaults = Figment::from(Serialized::defaults(Config::default()));
        for rename in super::CONFIG_KEYS {
            assert!(defaults.contains(rename.new), "{} is missing", rename.new);
            assert!(
                !defaults.contains(rename.old),
                "{} still exists",
                rename.old
            );
        }
    }
}
