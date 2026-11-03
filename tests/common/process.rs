//! Child-process fixtures: /proc probes, bounded waits, reaping guards and child-test runners.
//! Crate-independent: also mounted in-crate.
use std::{
    ffi::OsStr,
    path::Path,
    process::{Child, Command, Output},
    time::{Duration, Instant},
};

/// `/proc/PID/stat` state, parent PID and process group.
pub fn proc_stat(pid: i32) -> Option<(char, i32, i32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    let mut fields = fields.split_whitespace();
    Some((
        fields.next()?.chars().next()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    ))
}

/// Whether `pid` is a live process, neither reaped nor a zombie.
pub fn running(pid: i32) -> bool {
    proc_stat(pid).is_some_and(|(state, ..)| !matches!(state, 'Z' | 'X'))
}

pub fn live_process_group_members(pgid: i32) -> Vec<(i32, char)> {
    let mut members = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<i32>().ok())
        .filter_map(|pid| proc_stat(pid).map(|(state, _, group)| (pid, state, group)))
        .filter(|(_, state, group)| *group == pgid && !matches!(state, 'Z' | 'X'))
        .map(|(pid, state, _)| (pid, state))
        .collect::<Vec<_>>();
    members.sort_unstable();
    members
}

/// Poll `ready` every 10 ms until it holds or `timeout` passes.
pub fn eventually(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !ready() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

pub fn gate_pids(gate: &Path) -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(gate).ok()?;
    let mut pids = text.split_whitespace().map(str::parse);
    Some((pids.next()?.ok()?, pids.next()?.ok()?))
}

/// SIGKILLs the live members of the process group recorded in `gate` on drop, so a failed
/// assertion never strands fixture processes.
pub struct GroupWatchdog<'a>(pub &'a Path);

impl Drop for GroupWatchdog<'_> {
    fn drop(&mut self) {
        if let Some((group, _)) = gate_pids(self.0) {
            for (pid, _) in live_process_group_members(group) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

/// Kills and reaps a still-running child on drop, even when an assertion fails first. A
/// process-group leader takes its whole group (grandchildren included) with it; any other child
/// leads no group, so only the direct kill reaches anything.
pub struct Reaped(pub Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            // An unreaped child keeps its pid, so its group id cannot be reused yet.
            unsafe { libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL) };
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Runs `command` to completion within `limit`. On timeout or panic the still-running
/// process group (including grandchildren such as a worker) is killed and reaped.
pub fn bounded_output(command: &mut Command, limit: Duration) -> Output {
    use std::{
        io::{Read, Seek},
        os::unix::process::CommandExt,
        process::Stdio,
    };
    // Files, not pipes: a chatty child cannot block on a full pipe while we poll.
    let files = [(); 2].map(|_| tempfile::tempfile().unwrap());
    let mut child = Reaped(
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(files[0].try_clone().unwrap())
            .stderr(files[1].try_clone().unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "{command:?} did not exit within {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let [stdout, stderr] = files.map(|mut file| {
        let mut bytes = Vec::new();
        file.rewind().unwrap();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    });
    Output {
        status,
        stdout,
        stderr,
    }
}

/// Runs one test of the current test binary in a child process, so it can use another working
/// directory or `PATH` without touching this process; `timeout` kills and reaps a hung child.
pub fn run_child_test(test: &str, cwd: &Path, envs: &[(&str, &OsStr)]) {
    let output = Command::new("timeout")
        .args(["-k", "5", "120"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .current_dir(cwd)
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child test {test} failed:\n{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs one ignored test of the current test binary in a child that must fail as a test
/// (exit 101), not hang or abort; returns its stdout and stderr.
pub fn failing_child_test(test: &str, envs: &[(&str, &OsStr)]) -> String {
    let output = bounded_output(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--nocapture"])
            .envs(envs.iter().copied()),
        Duration::from_secs(60),
    );
    let text = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(101), "{text}");
    text.into_owned()
}
