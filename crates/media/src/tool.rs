//! Running the external media programs (vips, ffmpeg).
//!
//! Untrusted files are decoded in these separate processes rather than in
//! the server, so a decoder crash or hang costs one killed subprocess, and
//! every run has a timeout.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0} is not installed (or not on PATH); see the README for media prerequisites")]
    Missing(String),
    #[error("{program} took longer than {timeout:?} and was stopped")]
    Timeout { program: String, timeout: Duration },
    #[error("{program} failed: {stderr}")]
    Failed { program: String, stderr: String },
    #[error("could not run {program}: {source}")]
    Io {
        program: String,
        source: std::io::Error,
    },
}

/// Which libvips loaders a run may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loaders {
    /// Only loaders libvips considers safe for hostile input. ImageMagick,
    /// PDF, OpenSlide and similar are refused.
    Trusted,
    /// Also loaders libvips marks untrusted. Only for files already
    /// identified as a type whose (untrusted) loader the admin opted into.
    IncludingUntrusted,
}

/// Runs `program` and returns its standard output.
pub async fn run<I, S>(program: &Path, args: I, timeout: Duration) -> Result<Vec<u8>, ToolError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_with(program, args, timeout, Loaders::Trusted).await
}

pub async fn run_with<I, S>(
    program: &Path,
    args: I,
    timeout: Duration,
    loaders: Loaders,
) -> Result<Vec<u8>, ToolError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let name = program.display().to_string();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // Keep vips from spawning a thread per core per process.
        .env("VIPS_CONCURRENCY", "1");
    if loaders == Loaders::Trusted {
        command.env("VIPS_BLOCK_UNTRUSTED", "1");
    }
    let child = command.spawn().map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => ToolError::Missing(name.clone()),
        _ => ToolError::Io {
            program: name.clone(),
            source,
        },
    })?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => result.map_err(|source| ToolError::Io {
            program: name.clone(),
            source,
        })?,
        // Dropping the future kills the child (kill_on_drop).
        Err(_) => {
            return Err(ToolError::Timeout {
                program: name,
                timeout,
            });
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let stderr: String = stderr.chars().take(500).collect();
        return Err(ToolError::Failed {
            program: name,
            stderr: if stderr.is_empty() {
                format!("exited with {}", output.status)
            } else {
                stderr
            },
        });
    }
    Ok(output.stdout)
}

/// The first line a program prints for `--version`, to confirm it is
/// installed and to log which version is in use.
pub async fn version(program: &Path, flag: &str) -> Result<String, ToolError> {
    let out = run(program, [flag], Duration::from_secs(10)).await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_missing_programs() {
        let err = run(
            Path::new("uwuu-definitely-not-installed"),
            ["x"],
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Missing(_)), "{err}");
    }

    #[tokio::test]
    async fn captures_failures_and_output() {
        let out = run(Path::new("sh"), ["-c", "echo hi"], Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out, b"hi\n");
        let err = run(
            Path::new("sh"),
            ["-c", "echo broken >&2; exit 3"],
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed { stderr, .. } if stderr == "broken"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn kills_programs_that_run_too_long() {
        let started = std::time::Instant::now();
        let err = run(Path::new("sleep"), ["10"], Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Timeout { .. }), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
