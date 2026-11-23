//! Runs a helper program (the system `curl` or `wget`) with hard bounds, whatever the program
//! or the server it talks to does: a wall-clock deadline after which it is killed, a cap on
//! what it may write to standard output (it is killed past it, so a hostile or broken server
//! cannot fill the memory through it) and on what is kept of its standard error. No shell is
//! involved and standard input is closed.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessLimits {
    /// The program is killed when it is still running this long after it started.
    pub timeout: Duration,
    /// The program is killed when it writes more than this to standard output.
    pub max_stdout: u64,
    /// Standard error beyond this is read and dropped, so the program never blocks on it.
    pub max_stderr: usize,
}

#[derive(Debug)]
pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    /// At most [`ProcessLimits::max_stderr`] bytes.
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    /// The program could not be started (missing, not executable, out of resources).
    Spawn(String),
    /// Still running at the deadline; it was killed.
    Timeout,
    /// It wrote more than the standard-output cap; it was killed.
    OutputTooLarge { limit: u64 },
    /// Waiting for it or reading its output failed.
    Io(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(reason) => write!(f, "cannot start: {reason}"),
            Self::Timeout => write!(f, "timed out and was stopped"),
            Self::OutputTooLarge { limit } => write!(f, "wrote more than {limit} bytes and was stopped"),
            Self::Io(reason) => write!(f, "I/O error: {reason}"),
        }
    }
}

/// Reads `source` into memory up to `keep` bytes. With `overflow`, reading stops past `keep` and
/// the flag is raised (the caller kills the program); without, the excess is drained and dropped.
fn collect(mut source: impl Read + Send + 'static, keep: u64, overflow: Option<Arc<AtomicBool>>) -> std::io::Result<JoinHandle<std::io::Result<Vec<u8>>>> {
    std::thread::Builder::new().name("terra-subprocess".into()).spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 16 << 10];
        loop {
            let read = match source.read(&mut buffer) {
                Ok(0) => return Ok(kept),
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            let room = usize::try_from(keep.saturating_sub(kept.len() as u64)).unwrap_or(usize::MAX);
            kept.extend_from_slice(&buffer[..read.min(room)]);
            if read > room
                && let Some(flag) = &overflow
            {
                flag.store(true, Ordering::Release);
                return Ok(kept);
            }
        }
    })
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs `command` within `limits`. The command's own standard streams settings are replaced.
pub fn run_bounded(command: &mut Command, limits: &ProcessLimits) -> Result<ProcessOutput, ProcessError> {
    let started = Instant::now();
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| ProcessError::Spawn(error.to_string()))?;
    let overflow = Arc::new(AtomicBool::new(false));
    let readers = match (child.stdout.take(), child.stderr.take()) {
        (Some(stdout), Some(stderr)) => collect(stdout, limits.max_stdout, Some(Arc::clone(&overflow))).and_then(|out| Ok((out, collect(stderr, limits.max_stderr as u64, None)?))),
        _ => Err(std::io::Error::other("standard streams were not captured")),
    };
    let (stdout, stderr) = match readers {
        Ok(readers) => readers,
        Err(error) => {
            stop(&mut child);
            return Err(ProcessError::Io(error.to_string()));
        }
    };
    let mut pause = Duration::from_millis(1);
    let outcome = loop {
        if overflow.load(Ordering::Acquire) {
            stop(&mut child);
            break Err(ProcessError::OutputTooLarge { limit: limits.max_stdout });
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() >= limits.timeout => {
                stop(&mut child);
                break Err(ProcessError::Timeout);
            }
            Ok(None) => {
                std::thread::sleep(pause.min(limits.timeout.saturating_sub(started.elapsed())));
                pause = (pause * 2).min(Duration::from_millis(20));
            }
            Err(error) => {
                stop(&mut child);
                break Err(ProcessError::Io(error.to_string()));
            }
        }
    };
    // The program is gone, so its pipes close and both readers end (a grandchild still holding
    // them is not something curl or wget leave behind).
    let stdout = stdout.join().map_err(|_| ProcessError::Io("output reader panicked".into()))?;
    let stderr = stderr.join().map_err(|_| ProcessError::Io("error reader panicked".into()))?;
    let status = outcome?;
    if overflow.load(Ordering::Acquire) {
        return Err(ProcessError::OutputTooLarge { limit: limits.max_stdout });
    }
    Ok(ProcessOutput { status, stdout: stdout.map_err(|error| ProcessError::Io(error.to_string()))?, stderr: stderr.unwrap_or_default() })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    const LIMITS: ProcessLimits = ProcessLimits { timeout: Duration::from_secs(10), max_stdout: 1 << 20, max_stderr: 64 };

    #[test]
    fn collects_output_and_status() {
        let output = run_bounded(&mut shell("printf hello; printf oops >&2; exit 3"), &LIMITS).unwrap();
        assert_eq!((output.stdout.as_slice(), output.stderr.as_slice(), output.status.code()), (&b"hello"[..], &b"oops"[..], Some(3)));
    }

    #[test]
    fn a_program_that_never_ends_is_killed_at_the_deadline() {
        let started = Instant::now();
        let limits = ProcessLimits { timeout: Duration::from_millis(300), ..LIMITS };
        assert_eq!(run_bounded(&mut shell("exec sleep 30"), &limits).unwrap_err(), ProcessError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn endless_output_is_cut_and_the_program_killed() {
        let started = Instant::now();
        let limits = ProcessLimits { max_stdout: 100_000, ..LIMITS };
        assert_eq!(run_bounded(&mut shell("exec cat /dev/zero"), &limits).unwrap_err(), ProcessError::OutputTooLarge { limit: 100_000 });
        assert!(started.elapsed() < Duration::from_secs(5));
        // Exactly at the cap is fine.
        assert_eq!(run_bounded(&mut shell("head -c 100000 /dev/zero"), &limits).unwrap().stdout.len(), 100_000);
    }

    #[test]
    fn endless_errors_are_drained_without_blocking_or_growing() {
        let output = run_bounded(&mut shell("head -c 3000000 /dev/zero >&2; printf done"), &LIMITS).unwrap();
        assert_eq!((output.stdout.as_slice(), output.stderr.len()), (&b"done"[..], 64));
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        assert!(matches!(run_bounded(&mut Command::new("/nonexistent/terra-helper"), &LIMITS), Err(ProcessError::Spawn(_))));
    }
}
