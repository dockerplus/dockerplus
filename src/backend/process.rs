//! Running a command line tool that may never answer.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(20);
const SIGKILL: i32 = 9;

unsafe extern "C" {
    fn killpg(group: i32, signal: i32) -> i32;
}

#[derive(Debug)]
pub enum Failure {
    /// It could not be started or watched.
    Io(io::Error),
    /// It gave no answer in time, and has been killed.
    TimedOut,
}

/// Runs `command` for up to `limit`. It gets a process group of its own, so that
/// giving up also ends what it started: `colima ssh` leaves `limactl` and `ssh`
/// behind otherwise, still holding the pipes open.
pub fn run_limited(command: &mut Command, limit: Duration) -> Result<Output, Failure> {
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Failure::Io)?;
    // Drained on their own threads so a chatty command cannot fill a pipe and stall.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let end = Instant::now() + limit;
    let mut status = None;
    loop {
        if status.is_none() {
            status = child.try_wait().map_err(Failure::Io)?;
        }
        if let (Some(status), true, true) = (status, stdout.is_finished(), stderr.is_finished()) {
            let stdout = stdout.join().unwrap_or_default();
            let stderr = stderr.join().unwrap_or_default();
            return Ok(Output { status, stdout, stderr });
        }
        if Instant::now() >= end {
            // SAFETY: killpg takes plain integers and touches no memory.
            unsafe { killpg(child.id() as i32, SIGKILL) };
            let _ = child.wait();
            return Err(Failure::TimedOut);
        }
        thread::sleep(POLL);
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn a_quick_command_gives_its_output_and_status() {
        let output = run_limited(&mut shell("echo out; echo err >&2; exit 3"), Duration::from_secs(10)).unwrap();
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
        assert_eq!(output.status.code(), Some(3));
    }

    #[test]
    fn more_output_than_a_pipe_holds_does_not_stall_it() {
        let output = run_limited(&mut shell("head -c 500000 /dev/zero"), Duration::from_secs(10)).unwrap();
        assert_eq!(output.stdout.len(), 500_000);
    }

    #[test]
    fn a_stuck_command_goes_with_what_it_started() {
        let dir = std::env::temp_dir().join(format!("dp-process-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mark = dir.join("survived");
        // The child outlives the shell's own `wait` only if nothing kills the group.
        let script = format!("(sleep 2; touch '{}') & wait", mark.display());

        let started = Instant::now();
        let result = run_limited(&mut shell(&script), Duration::from_millis(200));

        assert!(matches!(result, Err(Failure::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(1));
        thread::sleep(Duration::from_millis(2_200));
        assert!(!mark.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_daemon_holding_the_pipes_cannot_outlast_the_limit() {
        // The shell exits at once; what it left behind keeps stdout open.
        let result = run_limited(&mut shell("sleep 5 & exit 0"), Duration::from_millis(200));
        assert!(matches!(result, Err(Failure::TimedOut)));
    }
}
