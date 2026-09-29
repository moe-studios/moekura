use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=MOEKURA_BUILD_VERSION");

    let version = std::env::var("MOEKURA_BUILD_VERSION")
        .ok()
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| {
            // Watch both directories: a worktree has its own HEAD but shares
            // refs and tags. This also catches packed refs and newly added tags.
            for arg in ["--git-dir", "--git-common-dir"] {
                if let Some(path) = git(&["rev-parse", "--path-format=absolute", arg]) {
                    println!("cargo::rerun-if-changed={path}");
                }
            }
            let release = format!("v{}", env!("CARGO_PKG_VERSION"));
            if git(&["tag", "--points-at", "HEAD"])
                .is_some_and(|tags| tags.lines().any(|tag| tag == release))
            {
                return env!("CARGO_PKG_VERSION").to_owned();
            }
            match git(&["rev-parse", "HEAD"]) {
                Some(hash) => format!("git-{}", &hash[..7]),
                None => {
                    println!("cargo::warning=Git metadata unavailable; set MOEKURA_BUILD_VERSION to embed the source version");
                    "git-unknown".to_owned()
                }
            }
        });
    println!("cargo::rustc-env=MOEKURA_BUILD_VERSION={version}");
}
