//! Running the external media programs (vips, ffmpeg).
//!
//! Untrusted files are decoded in these separate processes rather than in
//! the server, so a decoder crash or hang costs one killed subprocess, and
//! every run has a timeout. On Linux, runs can also be held to [`Limits`]
//! on memory and CPU time, set by a shell (`ulimit`) before the program
//! starts.

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
    /// Over [`Limits::memory_mb`]: the file needs more memory to process
    /// than the site allows.
    #[error("{program} needed more than the {limit_mb} MB of memory it may use")]
    OutOfMemory { program: String, limit_mb: u64 },
    /// Over [`Limits::cpu_secs`].
    #[error("{program} needed more than the {limit_secs} seconds of CPU time it may use")]
    OutOfCpu { program: String, limit_secs: u64 },
}

impl ToolError {
    /// Whether the run hit [`Limits`]: the file's demands, not a fault.
    pub fn is_over_limit(&self) -> bool {
        matches!(self, Self::OutOfMemory { .. } | Self::OutOfCpu { .. })
    }
}

/// What one run may use. Zero means no limit. Only enforced on Linux.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// Address space (virtual memory), in MB, which also covers the
    /// program's libraries and thread stacks.
    pub memory_mb: u64,
    /// CPU time, in seconds, counting every thread.
    pub cpu_secs: u64,
}

impl Limits {
    pub const NONE: Self = Self {
        memory_mb: 0,
        cpu_secs: 0,
    };

    fn is_none(self) -> bool {
        self == Self::NONE
    }

    /// A shell script setting the limits, then running its arguments.
    fn script(self) -> String {
        let mut script = String::new();
        if self.cpu_secs > 0 {
            // SIGXCPU at the soft limit; SIGKILL a little later if it's
            // ignored.
            script.push_str(&format!(
                "ulimit -S -t {} && ulimit -H -t {} && ",
                self.cpu_secs,
                self.cpu_secs + 5
            ));
        }
        if self.memory_mb > 0 {
            script.push_str(&format!("ulimit -v {} && ", self.memory_mb * 1024));
        }
        script.push_str("exec \"$@\"");
        script
    }
}

/// What programs print when an allocation fails (`ENOMEM`, threads
/// whose stacks couldn't be mapped, libraries that couldn't be loaded).
const OUT_OF_MEMORY: &[&str] = &[
    "Cannot allocate memory",
    "out of memory",
    "Out of memory",
    "pthread_create() failed",
    "failed to map segment",
    "bad_alloc",
];

/// Whether a failed run was stopped for using too much CPU time: killed
/// by SIGXCPU, or (ffmpeg) quitting on it.
fn out_of_cpu(status: std::process::ExitStatus, stderr: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        const SIGXCPU: i32 = 24;
        if status.signal() == Some(SIGXCPU) {
            return true;
        }
    }
    #[cfg(not(unix))]
    let _ = status;
    stderr.contains("received signal 24")
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
    run_limited(program, args, timeout, Loaders::Trusted, Limits::NONE).await
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
    run_limited(program, args, timeout, loaders, Limits::NONE).await
}

/// [`run_with`], held to `limits` (on Linux; elsewhere they're ignored).
pub async fn run_limited<I, S>(
    program: &Path,
    args: I,
    timeout: Duration,
    loaders: Loaders,
    limits: Limits,
) -> Result<Vec<u8>, ToolError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let name = program.display().to_string();
    let limited = cfg!(target_os = "linux") && !limits.is_none();
    let mut command = if limited {
        let mut shell = Command::new("/bin/sh");
        shell.arg("-c").arg(limits.script()).arg("sh").arg(program);
        shell
    } else {
        Command::new(program)
    };
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
    // ffmpeg can report a failed allocation and still exit 0, leaving its
    // output missing; it only prints errors, so look whatever the status.
    if limited && limits.memory_mb > 0 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if OUT_OF_MEMORY.iter().any(|s| stderr.contains(s)) {
            return Err(ToolError::OutOfMemory {
                program: name,
                limit_mb: limits.memory_mb,
            });
        }
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if limited {
            // The shell couldn't find the program.
            if output.status.code() == Some(127) && stderr.contains("not found") {
                return Err(ToolError::Missing(name));
            }
            if limits.cpu_secs > 0 && out_of_cpu(output.status, stderr) {
                return Err(ToolError::OutOfCpu {
                    program: name,
                    limit_secs: limits.cpu_secs,
                });
            }
        }
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
    let text = String::from_utf8_lossy(&out);
    let first_line = text.lines().next().unwrap_or_default();
    // ffmpeg appends its copyright notice to the version line.
    Ok(first_line
        .split(" Copyright")
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
            Path::new("moekura-definitely-not-installed"),
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn holds_programs_to_their_limits() {
        let cpu = Limits {
            cpu_secs: 1,
            ..Limits::NONE
        };
        let started = std::time::Instant::now();
        let err = run_limited(
            Path::new("sh"),
            ["-c", "while :; do :; done"],
            Duration::from_secs(30),
            Loaders::Trusted,
            cpu,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ToolError::OutOfCpu { limit_secs: 1, .. }),
            "{err}"
        );
        assert!(err.is_over_limit());
        assert!(started.elapsed() < Duration::from_secs(10));

        // A frame far larger than the memory allowed.
        let memory = Limits {
            memory_mb: 300,
            ..Limits::NONE
        };
        let err = run_limited(
            Path::new("ffmpeg"),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=8192x8192",
                "-frames:v",
                "1",
                "-vf",
                "scale=16000:16000",
                "-f",
                "null",
                "-",
            ],
            Duration::from_secs(30),
            Loaders::Trusted,
            memory,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ToolError::OutOfMemory { limit_mb: 300, .. }),
            "{err}"
        );

        // Within the limits, runs are as before; missing programs are
        // still reported as missing.
        let out = run_limited(
            Path::new("sh"),
            ["-c", "echo hi"],
            Duration::from_secs(5),
            Loaders::Trusted,
            Limits {
                memory_mb: 512,
                cpu_secs: 5,
            },
        )
        .await
        .unwrap();
        assert_eq!(out, b"hi\n");
        let err = run_limited(
            Path::new("moekura-definitely-not-installed"),
            ["x"],
            Duration::from_secs(5),
            Loaders::Trusted,
            memory,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Missing(_)), "{err}");
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
