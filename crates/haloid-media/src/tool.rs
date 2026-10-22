//! Running the external media tools: ffprobe, ffmpeg and vips.
//!
//! Every one of them parses untrusted input, so every run gets the same
//! treatment: no stdin, a wall-clock timeout after which the process is
//! killed, and for libvips the untrusted-loader block, which refuses the
//! loaders (ImageMagick, MATLAB, FITS and friends) that were never meant to
//! see hostile files. On Unix each child also runs under resource limits —
//! address space, CPU time, file size, no core dumps — so a decoder bug a
//! hostile file trips cannot take the machine's memory or disk with it. The tools' own error text is kept for the logs;
//! `MediaError` is never shown to a client.

use haloid_core::MediaError;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

pub(crate) struct Tool<'a> {
    pub binary: &'a Path,
    /// For `MediaError::ToolUnavailable`.
    pub name: &'static str,
    pub timeout: Duration,
}

/// Generous next to anything a legitimate file needs; a backstop against a
/// runaway decoder, not a quota.
const MAX_ADDRESS_SPACE: u64 = 8 << 30;
const MAX_FILE_SIZE: u64 = 16 << 30;

#[cfg(unix)]
fn limit_resources(command: &mut tokio::process::Command, timeout: Duration) {
    // CPU time counts every thread, so it is the wall-clock limit times the
    // cores the tool could use, plus slack. The wall clock is the real
    // limit; this catches a child that outlives its parent's kill.
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    let cpu = timeout.as_secs().max(1) * cores + 10;
    let limits = [
        (libc::RLIMIT_AS, MAX_ADDRESS_SPACE),
        (libc::RLIMIT_FSIZE, MAX_FILE_SIZE),
        (libc::RLIMIT_CPU, cpu),
        (libc::RLIMIT_CORE, 0),
    ];
    // SAFETY: the closure runs in the child between fork and exec, where
    // only async-signal-safe calls are allowed. It makes setrlimit calls
    // on values computed before the fork, allocates nothing and takes no
    // locks. A limit that cannot be set (one already lower, say) is left
    // as it is rather than failing the run.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(move || {
            for (resource, value) in limits {
                let limit = libc::rlimit {
                    rlim_cur: value as libc::rlim_t,
                    rlim_max: value as libc::rlim_t,
                };
                libc::setrlimit(resource, &limit);
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn limit_resources(_: &mut tokio::process::Command, _: Duration) {}

impl Tool<'_> {
    /// Run to completion and return stdout. A non-zero exit becomes
    /// `failure(stderr)`.
    pub(crate) async fn run<I, S>(
        &self,
        args: I,
        failure: fn(String) -> MediaError,
    ) -> Result<Vec<u8>, MediaError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = tokio::process::Command::new(self.binary);
        limit_resources(&mut command, self.timeout);
        let child = command
            .args(args)
            // Refuse libvips loaders not meant for untrusted input, and keep
            // its warnings (an absent optional module, say) out of stderr.
            .env("VIPS_BLOCK_UNTRUSTED", "1")
            .env("VIPS_WARNING", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                    MediaError::ToolUnavailable(self.name)
                }
                _ => failure(format!("could not start {}: {e}", self.name)),
            })?;

        // Dropping the future on timeout drops the child, which kills it.
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| MediaError::Timeout {
                secs: self.timeout.as_secs().max(1),
            })?
            .map_err(|e| failure(format!("{} did not finish: {e}", self.name)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(failure(format!(
                "{} failed: {}",
                self.name,
                stderr.trim().chars().take(500).collect::<String>()
            )));
        }
        Ok(output.stdout)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_child_runs_under_the_resource_limits() {
        let shell = Tool {
            binary: Path::new("sh"),
            name: "sh",
            timeout: Duration::from_secs(5),
        };
        let out = shell
            .run(
                ["-c", "ulimit -c; ulimit -v; ulimit -f"],
                MediaError::ProbeFailed,
            )
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "0", "no core dumps: {text}");
        assert_eq!(
            lines[1],
            (MAX_ADDRESS_SPACE / 1024).to_string(),
            "address space in KiB: {text}"
        );
        // `ulimit -f` counts 512-byte blocks in POSIX sh.
        let blocks: u64 = lines[2].parse().unwrap();
        assert!(
            blocks == MAX_FILE_SIZE / 512 || blocks == MAX_FILE_SIZE / 1024,
            "file size: {text}"
        );
    }
}
