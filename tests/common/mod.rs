#![allow(dead_code)]
pub mod http;

use publicator::{
    config::{Config, Paths},
    store::Store,
};
use serde_json::{Value, json};
use tempfile::TempDir;

pub fn id(n: u64) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
/// Placeholder digest: for state tests; identity/upload tests use `Fixture::art_row`.
pub fn row(n: u64, state: &str) -> Value {
    json!({"uuid": id(n), "state": state, "title": "Title", "description": "Description",
        "submissionTimestamp": 1_700_000_000, "files": [{"basename": format!("{n}.png"), "sha512sum": "0".repeat(128)}], "apparitions": []})
}
pub mod executable;

pub fn epoch(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp()
}
pub struct Fixture {
    pub root: TempDir,
    pub store: Store,
}
impl Fixture {
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }
    pub fn with_config(config: Config) -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::resolve(Some(root.path()), None).unwrap();
        let store = Store::new(paths, config).unwrap();
        Self { root, store }
    }
    pub fn path(&self, name: &str) -> std::path::PathBuf {
        self.root.path().join(name)
    }
    pub fn write(&self, value: Value) {
        std::fs::write(
            &self.store.paths().json,
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    pub fn read(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.store.paths().json).unwrap()).unwrap()
    }
    pub fn image(&self, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = self.path(name);
        std::process::Command::new("magick")
            .arg("-size")
            .arg(format!("{width}x{height}"))
            .arg("xc:white")
            .arg(&path)
            .status()
            .unwrap()
            .success()
            .then_some(())
            .unwrap();
        path
    }
}
impl Fixture {
    /// A tiny solid-color PNG (parents created); distinct colors give distinct content identities.
    pub fn colored(&self, name: &str, color: &str) -> std::path::PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let status = std::process::Command::new("magick")
            .args(["-size", "2x2", &format!("xc:{color}")])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }
    /// `row(n, state)` whose `{n}.png` artwork exists and carries its real SHA-512, as Pick records it.
    pub fn art_row(&self, n: u64, state: &str) -> Value {
        let image = self.image(&format!("{n}.png"), 4, 4);
        let mut publication = row(n, state);
        publication["files"][0]["sha512sum"] =
            json!(publicator::images::file_sha512(&image).unwrap());
        publication
    }
}

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
pub fn eventually(timeout: std::time::Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while !ready() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    true
}

/// Worker that logs each submit to `calls`, then hangs with a same-group child after
/// atomically writing "<worker pid = pgid> <child pid>" to `gate`.
pub fn hanging_submit_worker(calls: &std::path::Path, gate: &std::path::Path) -> String {
    format!(
        r#"#!/bin/sh
[ "$1" = "--check-steps" ] && exit 0
while IFS= read -r line; do
case "$line" in
*'"type":"open"'*) echo '{{"protocol":1,"type":"ready"}}' ;;
*'"type":"submit"'*) echo sent >> '{calls}'; sleep 1000 & echo "$$ $!" > '{gate}.tmp'; mv '{gate}.tmp' '{gate}'; wait ;;
esac
done"#,
        calls = calls.display(),
        gate = gate.display()
    )
}

pub fn gate_pids(gate: &std::path::Path) -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(gate).ok()?;
    let mut pids = text.split_whitespace().map(str::parse);
    Some((pids.next()?.ok()?, pids.next()?.ok()?))
}

/// SIGKILLs the live members of the process group recorded in `gate` on drop, so a failed
/// assertion never strands fixture processes.
pub struct GroupWatchdog<'a>(pub &'a std::path::Path);

impl Drop for GroupWatchdog<'_> {
    fn drop(&mut self) {
        if let Some((group, _)) = gate_pids(self.0) {
            for (pid, _) in live_process_group_members(group) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

/// After an abrupt stop mid-submit: the worker group is gone, the publisher lock is free and
/// the one sent attempt keeps its terminal-less intent receipt.
pub fn assert_abandoned_attempt(
    root: &std::path::Path,
    gate: &std::path::Path,
    calls: &std::path::Path,
) {
    let (group, _) = gate_pids(gate).expect("worker gate");
    assert!(
        eventually(std::time::Duration::from_secs(5), || {
            live_process_group_members(group).is_empty()
        }),
        "worker group survived: {:?}",
        live_process_group_members(group)
    );
    drop(publicator::publish_journal::publisher_lock(root).expect("publisher lock is free"));
    let attempts = std::fs::read_dir(root.join(".publicator/publish"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join("receipt.json"))
        .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    let receipt: Value = serde_json::from_slice(&std::fs::read(&attempts[0]).unwrap()).unwrap();
    assert!(receipt["terminal"].is_null(), "{receipt}");
    assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 1);
}

/// Writes a decodable solid-colour image; the format follows the path suffix.
pub fn solid_image(path: &std::path::Path, color: &str, width: u32, height: u32) {
    let status = std::process::Command::new("magick")
        .arg("-size")
        .arg(format!("{width}x{height}"))
        .arg(format!("xc:{color}"))
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "magick failed for {}", path.display());
}

/// Runs `command` to completion within `limit`. On timeout or panic the still-running
/// process group (including grandchildren such as a worker) is killed and reaped.
pub fn bounded_output(
    command: &mut std::process::Command,
    limit: std::time::Duration,
) -> std::process::Output {
    use std::{
        io::{Read, Seek},
        os::unix::process::CommandExt,
        process::Stdio,
        time::Instant,
    };
    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            if let Ok(None) = self.0.try_wait() {
                // An unreaped leader keeps its pid, so the group id cannot be reused yet.
                unsafe { libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL) };
                let _ = self.0.wait();
            }
        }
    }
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
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let [stdout, stderr] = files.map(|mut file| {
        let mut bytes = Vec::new();
        file.rewind().unwrap();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    });
    std::process::Output {
        status,
        stdout,
        stderr,
    }
}

/// Decodes `path` with ImageMagick's `identify -format`, failing the test if it cannot.
pub fn identify(path: &std::path::Path, format: &str) -> String {
    let output = std::process::Command::new("identify")
        .args(["-format", format])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "identify {}", path.display());
    String::from_utf8(output.stdout).unwrap()
}

/// Runs one test of the current test binary in a child process, so it can use another working
/// directory or `PATH` without touching this process; `timeout` kills and reaps a hung child.
pub fn run_child_test(test: &str, cwd: &std::path::Path, envs: &[(&str, &std::ffi::OsStr)]) {
    let output = std::process::Command::new("timeout")
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
