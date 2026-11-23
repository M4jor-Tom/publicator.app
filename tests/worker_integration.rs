//! Rust publisher and protocol decoder against the REAL Python worker.
//!
//! `tests/python/recording_worker.py` runs the real `--check-steps` CLI and the real
//! `serve(..., session_factory)` with an injected recording session, so framing, validation
//! and the event loop are production code; only the browser is replaced. See "Worker trust
//! boundary" in `docs/features/deviantart-publish.feature` for which side owns each guarantee.
mod common;
use common::{Fixture, id, row};
use publicator::{
    config::Config,
    error::{Error, Result},
    publish_journal::{PublishJournal, ReconcileOutcome},
    publish_protocol::read_frame,
    publishing::{PublishReport, PublishRequest, Publisher},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const REPO: &str = env!("CARGO_MANIFEST_DIR");
const ZONE: &str = "America/New_York";

/// One launcher per run, so each run has its own record; `behaviour` scripts the submit reply.
fn launcher(dir: &Path, name: &str, behaviour: &str) -> (PathBuf, PathBuf) {
    let (worker, record) = (
        dir.join(format!("{name}.sh")),
        dir.join(format!("{name}.jsonl")),
    );
    common::executable::write_executable(
        &worker,
        &format!(
            "#!/bin/sh\nexport PYTHONPATH='{REPO}/python'\nexec python '{REPO}/tests/python/recording_worker.py' '{}' {behaviour} \"$@\"\n",
            record.display()
        ),
    );
    (worker, record)
}

/// Recorded session calls: `["open", command, data_root, pid]`, `["submit", job, image_sha512]`,
/// `["close"]`.
fn records(record: &Path) -> Vec<String> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn recorded(record: &Path, kind: &str) -> Vec<Value> {
    let prefix = format!("[\"{kind}\"");
    records(record)
        .iter()
        .filter(|line| line.starts_with(&prefix))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn fixture() -> Fixture {
    let mut config = Config {
        tags: Some("tags.txt".into()),
        tiers: vec!["Gold".into(), "Silver".into()],
        galleries: vec!["Art".into(), "Artwork".into()],
        ..Config::default()
    };
    config
        .schedule
        .insert("timezone".into(), toml::Value::String(ZONE.into()));
    let f = Fixture::with_config(config);
    std::fs::write(f.path("tags.txt"), "art\n").unwrap();
    f
}

fn described(sha512sum: String) -> Value {
    let mut publication = row(1, "DESCRIBED");
    publication["files"] = json!([{"basename": "art.png", "sha512sum": sha512sum}]);
    publication["apparitionTimestampIfDifferentThanSubmission"] = json!(1_800_039_600);
    publication["apparitions"] = json!([{
        "platformName": "deviantart", "state": "unpublished", "tiers": [], "galleries": []
    }]);
    publication
}

fn sha(path: &Path) -> String {
    publicator::images::file_sha512(path).unwrap()
}

fn with_image(f: &Fixture) -> Value {
    described(sha(&f.image("art.png", 4, 4)))
}

async fn publish(f: &Fixture, worker: &Path) -> Result<PublishReport> {
    let skill = PathBuf::from(REPO).join(".harnesses/skills/publish-deviantart/SKILL.md");
    let app = common::offline_app(Arc::new(f.store.clone()))
        .with_publisher(Publisher::new(worker.to_owned(), skill));
    let request = PublishRequest {
        uuids: None,
        headless: true,
    };
    let publish = app.publish(request, Arc::new(AtomicBool::new(false)));
    tokio::time::timeout(Duration::from_secs(60), publish)
        .await
        .expect("publish run is bounded")
}

#[tokio::test]
async fn valid_publication_is_scheduled_by_the_real_worker_with_one_zone_and_a_staged_copy() {
    let tools = tempfile::tempdir().unwrap();
    let f = fixture();
    let mut publication = with_image(&f);
    let (tiers, galleries) = (json!(["Silver", "Gold"]), json!(["Artwork", "Art"]));
    publication["apparitions"][0]["tiers"] = tiers.clone();
    publication["apparitions"][0]["galleries"] = galleries.clone();
    let artwork_sha512 = publication["files"][0]["sha512sum"].clone();
    f.write(json!([publication]));
    let (worker, record) = launcher(tools.path(), "valid", "scheduled");
    assert_eq!(publish(&f, &worker).await.unwrap().published, 1);

    let [open] = recorded(&record, "open").try_into().unwrap();
    let data_dir = &f.store.paths().data_dir;
    assert_eq!(open[1]["timezone"], ZONE);
    let root = Path::new(open[2].as_str().unwrap());
    assert_eq!(root, data_dir.canonicalize().unwrap());
    let [submit] = recorded(&record, "submit").try_into().unwrap();
    let job = &submit[1];
    assert_eq!(job["uuid"], id(1));
    assert_eq!(job["schedule"]["timezone"], open[1]["timezone"]);
    assert_eq!((&job["tiers"], &job["galleries"]), (&tiers, &galleries));
    // The worker gets a verified per-attempt copy under `.publicator/`, never the original.
    let image = Path::new(job["image_path"].as_str().unwrap());
    assert!(image.starts_with(data_dir.join(".publicator")), "{image:?}");
    assert_ne!(image, data_dir.join("art.png"));
    assert_eq!(
        submit[2], artwork_sha512,
        "staged bytes differ from the artwork"
    );
    // `serve` closes the browser session on `close`, before the publisher signals the group.
    assert_eq!(
        recorded(&record, "close").len(),
        1,
        "{:?}",
        records(&record)
    );
}

#[tokio::test]
async fn a_failure_before_commit_from_the_real_worker_may_be_retried() {
    let tools = tempfile::tempdir().unwrap();
    let f = fixture();
    f.write(json!([with_image(&f)]));
    let (worker, record) = launcher(tools.path(), "failed", "failed");
    for sends in [1, 2] {
        let report = publish(&f, &worker).await.unwrap();
        assert_eq!((report.published, report.failed), (0, 1));
        assert_eq!(recorded(&record, "submit").len(), sends);
    }
    assert_worker_reaped(&record, "failed");
}

/// Builds a publication row in the data-dir fixture; the second fixture lies outside it.
type Publication = fn(&Fixture, &Fixture) -> Value;

fn empty_title(f: &Fixture, _: &Fixture) -> Value {
    let mut publication = with_image(f);
    publication["title"] = json!("");
    publication
}
fn negative_price(f: &Fixture, _: &Fixture) -> Value {
    let mut publication = with_image(f);
    publication["apparitions"][0]["priceIfNotFree"] = json!(-1);
    publication
}
fn missing_image(_: &Fixture, _: &Fixture) -> Value {
    described("0".repeat(128))
}
fn symlink_outside_data_dir(f: &Fixture, outside: &Fixture) -> Value {
    let target = outside.image("art.png", 4, 4);
    std::os::unix::fs::symlink(&target, f.path("art.png")).unwrap();
    described(sha(&target))
}
fn text_disguised_as_png(f: &Fixture, _: &Fixture) -> Value {
    std::fs::write(f.path("art.png"), "plain text, not an image").unwrap();
    described(sha(&f.path("art.png")))
}

#[tokio::test]
async fn rust_owned_publication_faults_never_reach_the_real_worker() {
    let tools = tempfile::tempdir().unwrap();
    let cases: [(&str, Publication); 5] = [
        ("empty_title", empty_title),
        ("negative_price", negative_price),
        ("missing_image", missing_image),
        ("symlink_outside_data_dir", symlink_outside_data_dir),
        ("text_disguised_as_png", text_disguised_as_png),
    ];
    for (name, publication) in cases {
        let (f, outside) = (fixture(), Fixture::new());
        f.write(json!([publication(&f, &outside)]));
        let (worker, record) = launcher(tools.path(), name, "scheduled");
        let result = publish(&f, &worker).await;
        assert!(!result.is_ok_and(|r| r.published > 0), "{name} published");
        assert!(records(&record).is_empty(), "{name} reached the worker");
    }
}

/// The recorded worker process is gone: the publisher reaped it.
fn assert_worker_reaped(record: &Path, behaviour: &str) {
    for open in recorded(record, "open") {
        let pid = open[3].as_u64().unwrap();
        let alive = Path::new(&format!("/proc/{pid}")).exists();
        assert!(!alive, "{behaviour}: worker {pid} outlived the run");
    }
}

#[tokio::test]
async fn channel_failures_after_submit_stay_unknown_and_block_resend_until_reconciled() {
    let tools = tempfile::tempdir().unwrap();
    // exit: dies after submit; partial: half a frame then EOF; mismatch: wrong attempt_id
    // first; fatal: session raises after submit; unknown: explicit unknown event.
    for behaviour in ["exit", "partial", "mismatch", "fatal", "unknown"] {
        let f = fixture();
        f.write(json!([with_image(&f)]));
        let (worker, record) = launcher(tools.path(), behaviour, behaviour);
        let report = publish(&f, &worker).await.unwrap();
        assert_eq!((report.published, report.unknown), (0, 1), "{behaviour}");
        let [submit] = recorded(&record, "submit").try_into().unwrap();
        let attempt = submit[1]["attempt_id"].as_str().unwrap();
        let data_dir = &f.store.paths().data_dir;
        let intent = data_dir.join(format!(".publicator/publish/{attempt}/receipt.json"));
        assert!(intent.is_file(), "{behaviour}: durable intent lost");
        assert_worker_reaped(&record, behaviour);

        let blocked = publish(&f, &worker).await;
        assert!(
            matches!(blocked, Err(Error::ReconciliationRequired(_))),
            "{behaviour}: second run was not blocked"
        );
        assert_eq!(recorded(&record, "submit").len(), 1, "{behaviour} resent");
        assert!(intent.is_file(), "{behaviour}: durable intent lost");

        PublishJournal::new(data_dir)
            .unwrap()
            .reconcile(&f.store, attempt, ReconcileOutcome::NotSubmitted)
            .unwrap();
        publish(&f, &worker).await.unwrap();
        assert_eq!(recorded(&record, "submit").len(), 2, "{behaviour}");
        assert_worker_reaped(&record, behaviour);
    }
}

const ATTEMPT: &str = "00000000-0000-4000-8000-000000000999";
const MAX_LINE: usize = 1024 * 1024;
/// Python worker outcomes: the session submitted, or a `fatal` reply with no session submit.
const SENT: &str = "scheduled";
const REFUSED: &str = "fatal";

fn job() -> Value {
    json!({
        "protocol": 1, "type": "submit", "attempt_id": ATTEMPT, "uuid": id(1),
        "image_path": "/data/.publicator/stage/upload.png", "title": "Title",
        "description": "Story", "tags": ["art"], "price": 0, "tiers": [], "galleries": [],
        "schedule": {"epoch": 1_800_039_600, "timezone": "Europe/Paris",
            "year": 2027, "month": 1, "day": 15, "hour": 20, "minute": 0}
    })
}

fn frame(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut job = job();
    edit(&mut job);
    let mut line = serde_json::to_vec(&job).unwrap();
    line.push(b'\n');
    line
}

/// The valid frame with the field at JSON `pointer` replaced.
fn set(pointer: &str, value: Value) -> Vec<u8> {
    frame(|job| *job.pointer_mut(pointer).unwrap() = value)
}

/// Non-JSON numeric tokens Python's json module accepts but Rust's serde_json rejects.
fn price_token(token: &str) -> Vec<u8> {
    let line = String::from_utf8(frame(|_| {})).unwrap();
    line.replace("\"price\":0", &format!("\"price\":{token}"))
        .into_bytes()
}

/// Valid epoch-consistent schedule parts for a Europe/Paris wall time at hour 02:00.
fn paris_2am(epoch: i64, month: u32, day: u32) -> Vec<u8> {
    frame(|job| {
        let schedule = &mut job["schedule"];
        schedule["epoch"] = json!(epoch);
        schedule["month"] = json!(month);
        schedule["day"] = json!(day);
        schedule["hour"] = json!(2);
    })
}

/// A valid frame padded with two-byte characters to exactly `total` bytes, newline included.
fn sized(total: usize) -> Vec<u8> {
    let pad = total - set("/title", json!("")).len();
    let line = set("/title", json!("é".repeat(pad / 2) + &"x".repeat(pad % 2)));
    assert_eq!(line.len(), total);
    line
}

/// (case, frame bytes, Rust decoder accepts, real Python worker outcome)
fn corpus() -> Vec<(&'static str, Vec<u8>, bool, &'static str)> {
    let valid = frame(|_| {});
    let mut invalid_utf8 = valid.clone();
    let title = valid.windows(5).position(|w| w == b"Title").unwrap();
    invalid_utf8[title + 1] = 0xff;
    let utc = frame(|job| {
        job["schedule"]["timezone"] = json!("UTC");
        job["schedule"]["epoch"] = json!(1_800_043_200);
    });
    vec![
        ("valid", valid.clone(), true, SENT),
        // Both sides refuse: protocol framing and job shape.
        (
            "unknown_field",
            frame(|j| j["surprise"] = json!(true)),
            false,
            REFUSED,
        ),
        ("protocol_2", set("/protocol", json!(2)), false, REFUSED),
        (
            "bad_attempt_id",
            set("/attempt_id", json!("x")),
            false,
            REFUSED,
        ),
        ("bad_uuid", set("/uuid", json!("x")), false, REFUSED),
        (
            "webp_image",
            set("/image_path", json!("/a.WebP")),
            false,
            REFUSED,
        ),
        (
            "nonzero_minute",
            set("/schedule/minute", json!(1)),
            false,
            REFUSED,
        ),
        (
            "dst_fold_earliest",
            paris_2am(1_824_940_800, 10, 31),
            true,
            SENT,
        ),
        (
            "dst_fold_later",
            paris_2am(1_824_944_400, 10, 31),
            false,
            REFUSED,
        ),
        (
            "malformed",
            b"{\"protocol\":1,\"type\":\"submit\",\n".to_vec(),
            false,
            REFUSED,
        ),
        (
            "truncated_at_eof",
            valid[..valid.len() / 2].to_vec(),
            false,
            REFUSED,
        ),
        (
            "oversized",
            set("/title", json!("x".repeat(MAX_LINE))),
            false,
            REFUSED,
        ),
        ("utf8_exactly_1_mib", sized(MAX_LINE), true, SENT),
        ("utf8_1_mib_plus_1", sized(MAX_LINE + 1), false, REFUSED),
        ("invalid_utf8", invalid_utf8, false, REFUSED),
        ("nan_price", price_token("NaN"), false, REFUSED),
        ("infinite_price", price_token("Infinity"), false, REFUSED),
        ("overflow_price", price_token("1e400"), false, REFUSED),
        // Python stricter than the decoder: Rust never sends these (Store price policy;
        // serde_json writes a non-finite f64 as null).
        ("negative_price", set("/price", json!(-1)), true, REFUSED),
        ("null_price", set("/price", Value::Null), true, REFUSED),
        // Rust-owned by Store validation and epoch-derived wall times in one config zone.
        ("empty_title", set("/title", json!("")), false, SENT),
        (
            "empty_description",
            set("/description", json!("")),
            false,
            SENT,
        ),
        ("dst_gap", paris_2am(1_806_195_600, 3, 28), false, SENT),
        ("timezone_mismatch", utc, true, SENT),
    ]
}

/// Like the Rust publisher: send `open`, wait for `ready`, then send the case frame and EOF.
/// Returns the worker's reply type after `ready`.
async fn python_outcome(tools: &Path, name: &str, frame: &[u8]) -> String {
    let data_root = tools.join(format!("{name}-data"));
    std::fs::create_dir(&data_root).unwrap();
    let open = json!({
        "protocol": 1, "type": "open", "headless": true, "timezone": "Europe/Paris",
        "login_profile": data_root.join(".deviantart-login"),
        "work_profile": data_root.join(".deviantart-session"),
    });
    let mut open = serde_json::to_vec(&open).unwrap();
    open.push(b'\n');
    let (worker, record) = launcher(tools, name, "scheduled");
    let mut child = tokio::process::Command::new(worker)
        .arg("--data-root")
        .arg(&data_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let frame = frame.to_vec();
    let run = async {
        stdin.write_all(&open).await.unwrap();
        let mut ready = String::new();
        stdout.read_line(&mut ready).await.unwrap();
        assert!(ready.contains("\"ready\""), "{name}: {ready}");
        // The worker may stop reading early (oversized line); EPIPE is expected then.
        let writer = tokio::spawn(async move { stdin.write_all(&frame).await.is_ok() });
        let mut replies = Vec::new();
        stdout.read_to_end(&mut replies).await.unwrap();
        writer.await.unwrap();
        child.wait().await.unwrap();
        replies
    };
    let replies = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("worker run is bounded");
    let first: Value = serde_json::from_slice(replies.split(|b| *b == b'\n').next().unwrap())
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let reply = first["type"].as_str().unwrap().to_owned();
    let submitted = !recorded(&record, "submit").is_empty();
    assert_eq!(submitted, reply == SENT, "{name}: {first}");
    reply
}

#[tokio::test]
async fn rust_decoder_and_real_python_worker_classify_the_shared_frame_corpus() {
    let tools = tempfile::tempdir().unwrap();
    let mut expected = Vec::new();
    let mut actual = Vec::new();
    for (name, frame, rust, python) in corpus() {
        let decoded = matches!(read_frame(&mut frame.as_slice()), Ok(Some(_)));
        let outcome = python_outcome(tools.path(), name, &frame).await;
        actual.push((name, decoded, outcome));
        expected.push((name, rust, python.to_owned()));
    }
    assert_eq!(actual, expected);
}
