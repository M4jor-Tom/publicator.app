use publicator::{
    application::Application,
    error::Error,
    images::PreparedImage,
    metadata::{
        Endpoints, GenerationRequest, MetadataClient, ModelOption, Source, VISION_MODELS,
        consent_required, metadata_prompt, title_description_schema,
    },
    model::GENERATE_MARKER,
};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    net::TcpListener,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

mod common;
use common::http::{Reply, Server};
use common::{Fixture, id};

fn model_option() -> ModelOption {
    VISION_MODELS[0].clone()
}
/// A port nothing listens on: any request to it is refused.
const CLOSED: &str = "http://127.0.0.1:1";
fn local_client(
    openrouter: &str,
    ollama: &str,
    llmfit_script: &str,
) -> (MetadataClient, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("llmfit");
    common::executable::write_executable(&executable, llmfit_script);
    (
        MetadataClient::new(
            reqwest::Client::new(),
            Endpoints {
                openrouter: openrouter.into(),
                ollama: ollama.into(),
            },
            executable,
        ),
        dir,
    )
}
/// llmfit reports the first catalogue model as a good fit.
fn fitting_client(openrouter: &str, ollama: &str) -> (MetadataClient, tempfile::TempDir) {
    let report =
        json!({"models":[{"name":"google/gemma-4-26B-A4B-it","fit_level":"Good","notes":[]}]});
    local_client(
        openrouter,
        ollama,
        &format!("#!/bin/sh\nprintf '%s' '{report}'\n"),
    )
}
fn local_client_for_test(server: &Server) -> (MetadataClient, tempfile::TempDir) {
    fitting_client(CLOSED, server.url())
}
/// Listens but never accepts or answers: a request connects into the backlog and waits for
/// headers that never come. `connections` drains that backlog afterwards.
fn silent() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}
fn connections(listener: &TcpListener) -> usize {
    listener.set_nonblocking(true).unwrap();
    std::iter::from_fn(|| listener.accept().ok()).count()
}
fn reply(path: &str, status: u16, body: Value) -> Reply {
    Reply {
        path: path.into(),
        status,
        body: body.to_string(),
    }
}
fn chat(content: &str) -> Reply {
    reply("/api/chat", 200, json!({"message":{"content":content}}))
}
fn local_request(install_missing: bool, timeout: Duration) -> GenerationRequest {
    GenerationRequest {
        source: Source::Local,
        model: model_option(),
        timeout,
        install_missing,
    }
}
/// Bytes whose base64 (`+/8=`) uses both non-alphanumeric symbols.
fn prepared_image() -> (tempfile::TempDir, PreparedImage) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("art.png");
    std::fs::write(&path, [0xfb, 0xff]).unwrap();
    let mime = "image/png".into();
    (dir, PreparedImage { path, mime })
}
fn paths(requests: &[Value]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request["path"].as_str().unwrap())
        .collect()
}
fn assert_chat_request(request: &Value) {
    assert_eq!(request["path"], "/api/chat");
    assert_eq!(
        request["body"],
        json!({"model":"gemma4:26b-a4b","stream":false,"format":title_description_schema(),
            "messages":[{"role":"user","content":metadata_prompt(),"images":["+/8="]}]})
    );
}

#[tokio::test]
async fn model_is_not_pulled_without_consent() {
    let server = Server::start(vec![reply("/api/tags", 200, json!({"models":[]}))]);
    let (client, _llmfit) = local_client_for_test(&server);
    let result = client
        .prepare_local(
            &model_option(),
            false,
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await;
    let requests = server.requests();
    server.finish();
    assert_eq!(
        result.unwrap_err().to_string(),
        "provider error: Local model gemma4:26b-a4b is not installed"
    );
    assert_eq!(paths(&requests), ["/api/tags"], "no pull without consent");
}

#[tokio::test]
async fn refused_or_silent_ollama_is_unavailable_and_never_pulled_or_chatted() {
    let (listener, silent_url) = silent();
    let refused = format!(
        "http://{}",
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    );
    for ollama in [refused, silent_url] {
        let (client, _llmfit) = fitting_client(CLOSED, &ollama);
        let (_dir, image) = prepared_image();
        let begun = Instant::now();
        let request = local_request(true, Duration::from_secs(1));
        let error = client.generate(&image, &request).await.unwrap_err();
        assert_eq!(error.to_string(), "provider error: Ollama is unavailable");
        assert!(begun.elapsed() < Duration::from_secs(3), "{ollama}");
    }
    assert_eq!(connections(&listener), 1, "only the tags probe connected");
}

const INHERITED_KEY: &str = "DUMMY-inherited-openrouter-key";

/// Child half of the next test; it runs only with the dummy key in its own environment.
#[tokio::test]
async fn inherited_key_child() {
    if std::env::var("OPENROUTER_KEY").as_deref() != Ok(INHERITED_KEY) {
        return;
    }
    let (openrouter, openrouter_url) = silent();
    let installed = json!({"models":[{"name":"gemma4:26b-a4b"}]});
    let valid = r#"{"title":"Local","description":"From Ollama."}"#;
    let server = Server::start(vec![reply("/api/tags", 200, installed), chat(valid)]);
    let (client, _llmfit) = fitting_client(&openrouter_url, server.url());
    let (_dir, image) = prepared_image();
    let request = local_request(false, Duration::from_secs(5));
    let generated = client.generate(&image, &request).await.unwrap();
    let requests = server.requests();
    server.finish();
    assert_eq!(generated.metadata.title, "Local");
    assert_eq!(paths(&requests), ["/api/tags", "/api/chat"]);
    assert_eq!(
        connections(&openrouter),
        0,
        "the inherited key reached OpenRouter"
    );
}

#[test]
fn an_inherited_openrouter_key_never_redirects_local_generation() {
    let cwd = tempfile::tempdir().unwrap();
    let key = [("OPENROUTER_KEY", OsStr::new(INHERITED_KEY))];
    common::run_child_test("inherited_key_child", cwd.path(), &key);
}

#[tokio::test]
async fn consented_pull_failure_never_advances_to_chat() {
    for (pull, expected) in [
        (
            reply("/api/pull", 500, json!({"error":"no space"})),
            "provider error: Ollama HTTP 500",
        ),
        (
            reply("/api/pull", 200, json!({"status":"failed"})),
            "provider error: Ollama failed to pull gemma4:26b-a4b",
        ),
    ] {
        // No chat reply is scripted: a chat attempt would end on the closed endpoint instead.
        let server = Server::start(vec![reply("/api/tags", 200, json!({"models":[]})), pull]);
        let (client, _llmfit) = local_client_for_test(&server);
        let (_dir, image) = prepared_image();
        let error = client
            .generate(&image, &local_request(true, Duration::from_secs(2)))
            .await
            .unwrap_err();
        let requests = server.requests();
        server.finish();
        assert_eq!(error.to_string(), expected);
        assert_eq!(paths(&requests), ["/api/tags", "/api/pull"]);
        assert_eq!(
            requests[1]["body"],
            json!({"model":"gemma4:26b-a4b","stream":false})
        );
    }
}

#[tokio::test]
async fn local_chat_sends_schema_and_image_and_parses_pair() {
    let server = Server::start(vec![
        reply("/api/tags", 200, json!({"models":[]})),
        reply("/api/pull", 200, json!({"status":"success"})),
        chat("{\"title\":\"A title\",\"description\":\"A scene unfolds.\"}"),
    ]);
    let (client, _llmfit) = local_client_for_test(&server);
    let (_dir, image) = prepared_image();
    let generated = client
        .generate(&image, &local_request(true, Duration::from_secs(3)))
        .await
        .unwrap();
    let req = server.requests();
    server.finish();
    assert_eq!(generated.metadata.title, "A title");
    assert_eq!(generated.model_used, "gemma4:26b-a4b");
    assert_eq!(
        req[1]["body"],
        json!({"model":"gemma4:26b-a4b","stream":false})
    );
    assert_chat_request(&req[2]);
}

#[tokio::test]
async fn local_chat_rejects_malformed_or_empty_metadata() {
    let marked = format!("{{\"title\":\"T\",\"description\":\"text {GENERATE_MARKER}\"}}");
    for (reply, expected) in [
        (chat("not json"), "provider error: no JSON object"),
        (
            chat("{\"title\": oops}"),
            "JSON error: expected value at line 1 column 11",
        ),
        (
            chat("{\"title\":\"\",\"description\":\"text\"}"),
            "validation error: complete unmarked metadata required",
        ),
        (
            chat(&marked),
            "validation error: complete unmarked metadata required",
        ),
        (
            reply("/api/chat", 200, json!({"message":{}})),
            "protocol error: Ollama response missing message content",
        ),
    ] {
        let server = Server::start(vec![reply]);
        let (client, _llmfit) = local_client_for_test(&server);
        let (_dir, image) = prepared_image();
        let error = client
            .local_generate(&image, &local_request(false, Duration::from_secs(2)))
            .await
            .unwrap_err();
        let requests = server.requests();
        server.finish();
        // The parse/validation verdict proves the scripted reply was delivered and read.
        assert_eq!(error.to_string(), expected);
        assert_eq!(requests.len(), 1);
        assert_chat_request(&requests[0]);
    }
}

/// The pre-fix oracle: tags scripted, chat sent, and only `is_err()` checked. The fixture's
/// wrong-path panic must still fail the test when the server is merely dropped.
#[tokio::test]
#[ignore = "child process of unexpected_local_request_fails_the_test_visibly"]
async fn unexpected_request_scenario() {
    let server = Server::start(vec![reply("/api/tags", 200, json!({"models":[]}))]);
    let (client, _llmfit) = local_client_for_test(&server);
    let (_dir, image) = prepared_image();
    let result = client
        .local_generate(&image, &local_request(false, Duration::from_secs(2)))
        .await;
    assert!(result.is_err());
}

#[test]
fn unexpected_local_request_fails_the_test_visibly() {
    let output = common::bounded_output(
        Command::new(std::env::current_exe().unwrap()).args([
            "--exact",
            "unexpected_request_scenario",
            "--ignored",
            "--nocapture",
        ]),
        Duration::from_secs(60),
    );
    let text = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(101), "{text}");
    for witness in [
        "unexpected test HTTP path",
        r#"left: "/api/chat""#,
        r#"right: "/api/tags""#,
        "unexpected_request_scenario ... FAILED",
    ] {
        assert!(text.contains(witness), "missing {witness:?} in {text}");
    }
}

#[tokio::test]
async fn withheld_chat_body_ends_at_the_shared_local_deadline() {
    let installed = json!({"models":[{"name":"gemma4:26b-a4b"}]});
    let valid = "{\"title\":\"Late\",\"description\":\"Arrives after the deadline.\"}";
    let (server, captured, release) =
        Server::start_paused_at(vec![reply("/api/tags", 200, installed), chat(valid)], 1);
    let (client, _llmfit) = local_client_for_test(&server);
    let (_dir, image) = prepared_image();
    let timeout = Duration::from_secs(1);
    let begun = Instant::now();
    let error = client
        .generate(&image, &local_request(false, timeout))
        .await
        .unwrap_err();
    let elapsed = begun.elapsed();
    captured
        .try_recv()
        .expect("chat was captured and its headers sent before the deadline");
    release.send(()).unwrap();
    let requests = server.requests();
    server.finish();
    assert_eq!(
        error.to_string(),
        "provider error: local metadata deadline exceeded"
    );
    assert!(elapsed < timeout * 3, "ended after {elapsed:?}");
    assert_eq!(paths(&requests), ["/api/tags", "/api/chat"]);
    assert_chat_request(&requests[1]);
}

#[tokio::test]
async fn tags_without_models_is_a_protocol_error_that_leaves_publications_intact() {
    let f = Fixture::new();
    let mut publication = f.art_row(1, "SCHEDULED");
    publication["title"] = json!(GENERATE_MARKER);
    publication["description"] = json!(GENERATE_MARKER);
    f.write(json!([publication]));
    let before = std::fs::read(&f.store.paths().json).unwrap();
    for batch in [false, true] {
        let server = Server::start(vec![reply("/api/tags", 200, json!({"status":"ok"}))]);
        let (client, _llmfit) = local_client_for_test(&server);
        let app = Application::new(Arc::new(f.store.clone()), client);
        let request = local_request(false, Duration::from_secs(2));
        let error = if batch {
            app.generate_marked(&request).await.unwrap_err()
        } else {
            app.generate_one(&id(1), request).await.unwrap_err()
        };
        server.finish();
        assert!(
            matches!(error, Error::Protocol(_)),
            "batch {batch}: {error}"
        );
        assert_eq!(
            error.to_string(),
            "protocol error: Ollama tags missing models"
        );
        assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
    }
}

/// Polls the runtime (whose reaper collects killed children) until `pid` no longer exists.
async fn gone_within(pid: libc::pid_t, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    while unsafe { libc::kill(pid, 0) } == 0 {
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

#[tokio::test]
async fn blocked_llmfit_is_killed_and_reaped_at_the_shared_deadline() {
    let entered = tempfile::tempdir().unwrap();
    let pid_file = entered.path().join("llmfit.pid");
    let (client, _llmfit) = local_client(
        CLOSED,
        CLOSED,
        &format!(
            "#!/bin/sh\necho $$ > '{0}.tmp'\nmv '{0}.tmp' '{0}'\nexec sleep 30\n",
            pid_file.display()
        ),
    );
    let (_dir, image) = prepared_image();
    let timeout = Duration::from_secs(2);
    let begun = Instant::now();
    let error = client
        .generate(&image, &local_request(false, timeout))
        .await
        .unwrap_err();
    let elapsed = begun.elapsed();
    let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
        .expect("llmfit signalled entry before the deadline")
        .trim()
        .parse()
        .unwrap();
    let gone = gone_within(pid, Duration::from_secs(5)).await;
    if !gone {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(gone, "llmfit {pid} outlived the generation deadline");
    assert_eq!(
        error.to_string(),
        "provider error: local metadata deadline exceeded"
    );
    // llmfit's own 10 s bound would end later: the shared generation deadline ended it.
    assert!(elapsed < timeout * 2, "ended after {elapsed:?}");
}

#[tokio::test]
async fn llmfit_good_too_tight_and_unassessable_verdicts_are_distinct() {
    for (stdout, exit, expected, level) in [
        (
            r#"{"models":[{"name":"google/gemma-4-26B-A4B-it","fit_level":"Good"}]}"#,
            true,
            true,
            "Good",
        ),
        (
            r#"{"models":[{"name":"google/gemma-4-26B-A4B-it","fit_level":"Too Tight"}]}"#,
            true,
            false,
            "Too Tight",
        ),
        ("not json", true, false, "Unavailable"),
        ("", false, false, "Unavailable"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("llmfit");
        common::executable::write_executable(
            &exe,
            &format!(
                "#!/bin/sh\nprintf '%s' '{}'\n{}\n",
                stdout,
                if exit { "exit 0" } else { "exit 1" }
            ),
        );
        let client = MetadataClient::new(reqwest::Client::new(), Endpoints::default(), exe);
        let fit = client.local_fit(&model_option()).await.unwrap();
        assert_eq!(fit.fit, expected);
        assert_eq!(fit.level, level);
    }
}

#[test]
fn consent_is_only_required_for_a_fitting_absent_model() {
    let missing = publicator::metadata::LocalStatus {
        fit: publicator::metadata::Fit {
            fit: true,
            level: "Good".into(),
            reason: String::new(),
        },
        installed: false,
        available: true,
    };
    assert!(consent_required(&missing));
    let installed = publicator::metadata::LocalStatus {
        installed: true,
        ..missing.clone()
    };
    assert!(!consent_required(&installed));
    let unavailable = publicator::metadata::LocalStatus {
        fit: publicator::metadata::Fit {
            fit: false,
            level: "Too Tight".into(),
            reason: "ram".into(),
        },
        installed: false,
        available: false,
    };
    assert!(!consent_required(&unavailable));
}
