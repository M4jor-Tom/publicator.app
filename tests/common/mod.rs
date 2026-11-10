#![allow(dead_code)]
pub mod git;
pub mod http;
pub mod image;
pub mod process;
pub use self::image::*;
pub use process::*;

use publicator::{
    config::{Config, Paths},
    images::{Candidate, Search, find_candidates},
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
        solid_image(&path, "white", width, height);
        path
    }
    /// A tiny solid-color PNG (parents created); distinct colors give distinct content identities.
    pub fn colored(&self, name: &str, color: &str) -> std::path::PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        solid_image(&path, color, 2, 2);
        path
    }
    /// Runs `magick <args> <name>`; WebP output is lossless so pixels can be compared exactly.
    pub fn generated(&self, name: &str, args: &[&str]) -> std::path::PathBuf {
        let path = self.path(name);
        let made = std::process::Command::new("magick")
            .args(args)
            .args(["-define", "webp:lossless=true"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success(), "magick failed for {name}");
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
    /// A SCHEDULED `art_row` marked for generation, as AI generation requires.
    pub fn marked_row(&self, n: u64) -> Value {
        with_markers(self.art_row(n, "SCHEDULED"))
    }
}

/// Cached thumbnail file names in `f`'s `.thumbs/`, sorted.
pub fn thumbs(f: &Fixture) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(f.path(".thumbs"))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| !name.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A fixture whose only configuration is the publicable `roots`.
pub fn configured(roots: &[&str]) -> Fixture {
    Fixture::with_config(Config {
        publicable: roots.iter().map(|root| (*root).into()).collect(),
        ..Config::default()
    })
}

/// Discovery over the configured `roots`, at most 100 candidates.
pub fn search(store: &Store, roots: &[&str]) -> publicator::error::Result<Vec<Candidate>> {
    let search = Search {
        limit: 100,
        paths: roots.iter().map(|root| (*root).into()).collect(),
    };
    find_candidates(store, &search)
}

/// The sorted paths discovery offers over `roots`.
pub fn found(f: &Fixture, roots: &[&str]) -> publicator::error::Result<Vec<std::path::PathBuf>> {
    let mut paths = search(&f.store, roots)?
        .into_iter()
        .map(|candidate| candidate.path)
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

/// `row` with both fields carrying the exact generation marker.
pub fn with_markers(mut row: Value) -> Value {
    row["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    row["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    row
}

/// An Application over `store` whose AI providers are unreachable loopback endpoints.
pub fn offline_app(store: std::sync::Arc<Store>) -> publicator::application::Application {
    use publicator::metadata::{Endpoints, MetadataClient};
    let endpoints = Endpoints {
        openrouter: "http://127.0.0.1:1".into(),
        ollama: "http://127.0.0.1:1".into(),
    };
    let metadata = MetadataClient::new(reqwest::Client::new(), endpoints, "llmfit".into());
    publicator::application::Application::new(store, metadata)
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
    let receipt = root
        .join(".publicator/publish")
        .join(only_attempt(root))
        .join("receipt.json");
    let receipt: Value = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
    assert!(receipt["terminal"].is_null(), "{receipt}");
    assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 1);
}

/// The ID of the one publish attempt recorded under `root`.
pub fn only_attempt(root: &std::path::Path) -> String {
    let mut attempts = std::fs::read_dir(root.join(".publicator/publish"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    attempts.pop().unwrap()
}

/// Drives Iced tasks the way the runtime would, keeping lifecycle effects observable:
/// a test that waits for a close must see zero exits, not merely zero messages.
#[cfg(feature = "desktop")]
pub mod ui {
    use iced_runtime::{Action, futures::futures::StreamExt};
    use publicator::ui::{App, Message};

    /// What tasks asked the runtime to do.
    #[derive(Debug, Default)]
    pub struct Effects {
        pub messages: Vec<Message>,
        pub exits: usize,
        pub widgets: usize,
    }

    impl Effects {
        /// Whether any produced message satisfies `f`.
        pub fn any(&self, f: impl FnMut(&Message) -> bool) -> bool {
            self.messages.iter().any(f)
        }

        fn record(&mut self, action: Action<Message>) {
            match action {
                Action::Output(message) => self.messages.push(message),
                Action::Exit => self.exits += 1,
                Action::Widget(_) => self.widgets += 1,
                other => panic!("unclassified runtime action: {other:?}"),
            }
        }
    }

    /// Runs one task to completion without dispatching its messages.
    pub async fn run(task: iced::Task<Message>) -> Effects {
        let mut effects = Effects::default();
        if let Some(mut actions) = iced_runtime::task::into_stream(task) {
            while let Some(action) = actions.next().await {
                effects.record(action);
            }
        }
        effects
    }

    /// Runs `task` and every task its messages chain through `App::update`.
    pub async fn settle(app: &mut App, task: iced::Task<Message>) -> Effects {
        let mut effects = run(task).await;
        let mut next = 0;
        while let Some(message) = effects.messages.get(next).cloned() {
            next += 1;
            let chained = run(app.update(message)).await;
            effects.messages.extend(chained.messages);
            effects.exits += chained.exits;
            effects.widgets += chained.widgets;
        }
        effects
    }

    /// Delivers `message` and settles everything it chains.
    pub async fn send(app: &mut App, message: Message) -> Effects {
        let task = app.update(message);
        settle(app, task).await
    }

    /// The next message of a held task stream; an exit or the end of the stream fails the test.
    pub async fn next_output<S>(actions: &mut S) -> Message
    where
        S: iced::futures::Stream<Item = Action<Message>> + Unpin,
    {
        loop {
            match actions.next().await {
                Some(Action::Output(message)) => return message,
                Some(Action::Exit) => panic!("task exited before producing a message"),
                Some(_) => {}
                None => panic!("task ended without producing a message"),
            }
        }
    }

    /// The first message `task` produces.
    pub async fn first_output(task: iced::Task<Message>) -> Message {
        let mut actions = iced_runtime::task::into_stream(task).expect("dispatched task stream");
        next_output(&mut actions).await
    }

    /// Runs a workflow operation's task, requires its success and applies the completion.
    pub async fn finish_workflow(app: &mut App, task: iced::Task<Message>) {
        let completion = first_output(task).await;
        assert!(
            matches!(
                &completion,
                Message::WorkflowFinished { result: Ok(()), .. }
            ),
            "{completion:?}"
        );
        let _ = app.update(completion);
    }
}

/// Listens but never accepts or answers: a request connects into the backlog and waits for
/// headers that never come. `connections` drains that backlog afterwards.
pub fn silent() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}

/// The inherited `PATH` with `dir` searched first.
pub fn path_with(dir: &std::path::Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(dir.to_owned()).chain(std::env::split_paths(&inherited)))
        .unwrap()
}

/// Drains the backlog of a listener that never accepts: the connections made to it.
pub fn connections(listener: &std::net::TcpListener) -> usize {
    listener.set_nonblocking(true).unwrap();
    std::iter::from_fn(|| listener.accept().ok()).count()
}
