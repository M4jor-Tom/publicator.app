use publicator::{
    application::Application,
    images::{PreparedImage, file_sha512},
    metadata::{
        CatalogModel, Endpoints, GenerationRequest, MetadataClient, ModelOption, Source,
        VISION_MODELS,
    },
    model::GENERATE_MARKER,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

mod common;
use common::{
    Fixture,
    http::{Reply, Server},
};

fn client(base: &str) -> MetadataClient {
    MetadataClient::new(
        reqwest::Client::new(),
        Endpoints {
            openrouter: base.into(),
            ollama: String::new(),
        },
        PathBuf::new(),
    )
    .with_openrouter_key("DUMMY-test-credential")
}
/// Opaque bytes: the client forwards them base64-encoded; decoding is tested in tests/images*.rs.
fn image() -> (tempfile::TempDir, PreparedImage) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("art.png");
    std::fs::write(&path, b"png bytes").unwrap();
    (
        dir,
        PreparedImage {
            path,
            mime: "image/png".into(),
        },
    )
}
fn completion(model: &str) -> String {
    json!({"choices":[{"message":{"content":format!("{{\"title\":\"T\",\"description\":\"D\"}}")}}], "model": model}).to_string()
}
fn request() -> GenerationRequest {
    GenerationRequest {
        source: Source::OpenRouter,
        model: ModelOption {
            ollama_name: String::new(),
            openrouter_name: "openrouter/vendor/selected:free".into(),
            llmfit_name: String::new(),
        },
        timeout: Duration::from_secs(4),
        install_missing: false,
    }
}

#[tokio::test]
async fn silent_headers_end_generation_at_its_deadline_with_no_later_request() {
    // Listens but never accepts or answers: the request waits for headers that never come.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = client(&format!("http://{}", listener.local_addr().unwrap()));
    let (_dir, image) = image();
    let timeout = Duration::from_secs(1);
    let begun = Instant::now();
    let request = GenerationRequest {
        timeout,
        ..request()
    };
    let error = client.generate(&image, &request).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "provider error: OpenRouter deadline exceeded"
    );
    assert!(
        begun.elapsed() < timeout * 3,
        "ended after {:?}",
        begun.elapsed()
    );
    listener.set_nonblocking(true).unwrap();
    let connections = std::iter::from_fn(|| listener.accept().ok()).count();
    assert_eq!(
        connections, 1,
        "the catalogue fallback ran past the shared deadline"
    );
}

#[test]
fn catalogue_requires_vision_and_zero_pricing() {
    let v = json!({"id":"vendor/free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"},"supported_parameters":[]});
    let m = CatalogModel::from_api(&v).unwrap();
    assert!(m.image_input && m.free);
    for v in [
        json!({"id":"paid-image","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0","image":"0.0001"}}),
        json!({"id":"paid-request","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0","request":"0.01"}}),
        json!({"id":"text-only","architecture":{"input_modalities":["text"]},"pricing":{"prompt":"0","completion":"0"}}),
    ] {
        let m = CatalogModel::from_api(&v).unwrap();
        if v["id"] == "text-only" {
            assert!(!m.image_input);
        } else {
            assert!(!m.free, "paid model advertised as free: {}", m.id);
        }
    }
}

#[tokio::test]
async fn http_429_walks_other_vendor_before_sibling_and_reports_answerer() {
    let (_dir, img) = image();
    let catalog=json!({"data":[
        {"id":"vendor/selected:free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"}},
        {"id":"other/model:free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"},"supported_parameters":[]},
        {"id":"vendor/sibling:free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"}}
    ]}).to_string();
    let server = common::http::Server::start(vec![
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 429,
            body: "busy".into(),
        },
        common::http::Reply {
            path: "/models".into(),
            status: 200,
            body: catalog,
        },
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 200,
            body: completion("other/model:free"),
        },
    ]);
    let got = client(server.url())
        .openrouter_generate(&img, &request(), None)
        .await
        .unwrap();
    assert_eq!(got.model_used, "other/model:free");
    assert_eq!(got.metadata.title, "T");
    assert_eq!(got.metadata.description, "D");
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["body"]["model"], "vendor/selected:free");
    assert_eq!(requests[2]["body"]["model"], "other/model:free");
    assert!(requests[2]["body"].get("response_format").is_none());
    assert!(
        requests[0]["body"]
            .to_string()
            .contains("data:image/png;base64,")
    );
}

#[tokio::test]
async fn schema_rejection_retries_once_prompt_shaped_then_memoizes() {
    let (_dir, img) = image();
    let server = common::http::Server::start(vec![
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 400,
            body: r#"{"error":{"message":"response_format is not supported"}}"#.into(),
        },
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 200,
            body: completion("vendor/model"),
        },
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 200,
            body: completion("vendor/model"),
        },
    ]);
    let c = client(server.url());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    c.openrouter_attempt(&img, "vendor/model", true, deadline)
        .await
        .unwrap();
    c.openrouter_attempt(&img, "vendor/model", true, deadline)
        .await
        .unwrap();
    let req = server.requests();
    assert_eq!(req.len(), 3);
    assert!(req[0]["body"].get("response_format").is_some());
    assert!(req[1]["body"].get("response_format").is_none());
    assert!(req[1]["body"].to_string().contains("title"));
    assert!(req[2]["body"].get("response_format").is_none());
}

#[tokio::test]
async fn response_model_alias_never_becomes_paid_preference() {
    let (_dir, img) = image();
    let server = common::http::Server::start(vec![
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 200,
            body: completion("vendor/canonical-without-free"),
        },
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 200,
            body: completion("vendor/canonical-without-free"),
        },
    ]);
    let c = client(server.url());
    let first = c.openrouter_generate(&img, &request(), None).await.unwrap();
    assert_eq!(first.model_used, "vendor/selected:free");
    c.openrouter_generate(&img, &request(), Some(&first.model_used))
        .await
        .unwrap();
    assert!(
        server
            .requests()
            .iter()
            .all(|r| r["body"]["model"] == "vendor/selected:free")
    );
}

#[tokio::test]
async fn upstream_error_diagnostics_redact_credentials_and_image_data_and_are_bounded() {
    let (_dir, img) = image();
    let server = common::http::Server::start(vec![common::http::Reply {
        path: "/chat/completions".into(),
        status: 400,
        body: json!({"error":{"message":format!("response_format unsupported; echoed DUMMY-test-credential and data:image/png;base64,cG5nIGJ5dGVz {}", "x".repeat(2000))}}).to_string(),
    }]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let error = client(server.url())
        .openrouter_attempt(&img, "vendor/model", false, deadline)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP 400"));
    assert!(error.contains("response_format unsupported"));
    assert!(!error.contains("DUMMY-test-credential"));
    assert!(!error.contains("data:image/png;base64"));
    assert!(
        error.len() < 600,
        "diagnostic exceeded bound: {}",
        error.len()
    );
}

#[tokio::test]
async fn clipped_data_uri_in_upstream_error_is_redacted_without_losing_classification() {
    let (_dir, img) = image();
    let server = common::http::Server::start(vec![common::http::Reply {
        path: "/chat/completions".into(), status: 400,
        body: json!({"error":{"message":"DUMMY-test-credential echoed data:image/png;base64,cG5nIGJ5 response_format unsupported"}}).to_string(),
    }]);
    let error = client(server.url())
        .openrouter_attempt(
            &img,
            "vendor/model",
            false,
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP 400"));
    assert!(error.contains("response_format unsupported"));
    assert!(!error.contains("DUMMY-test-credential"));
    assert!(!error.contains("data:image/"));
    assert!(!error.contains("cG5nIGJ5"));
    assert!(error.len() < 600);
}

#[tokio::test]
async fn unrelated_bad_request_does_not_trigger_schema_retry() {
    let (_dir, img) = image();
    let server = common::http::Server::start(vec![common::http::Reply {
        path: "/chat/completions".into(),
        status: 400,
        body: r#"{"error":{"message":"invalid prompt"}}"#.into(),
    }]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    assert!(
        client(server.url())
            .openrouter_attempt(&img, "vendor/model", true, deadline)
            .await
            .is_err()
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn successful_selected_model_does_not_fetch_catalogue() {
    let (_dir, img) = image();
    let server = common::http::Server::start(vec![common::http::Reply {
        path: "/chat/completions".into(),
        status: 200,
        body: completion("vendor/model"),
    }]);
    client(server.url())
        .openrouter_generate(&img, &request(), None)
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn malformed_rescue_catalogue_preserves_original_model_error() {
    let (_dir, img) = image();
    let server = common::http::Server::start(vec![
        common::http::Reply {
            path: "/chat/completions".into(),
            status: 429,
            body: "original busy".into(),
        },
        common::http::Reply {
            path: "/models".into(),
            status: 200,
            body: "{bad".into(),
        },
    ]);
    let err = client(server.url())
        .openrouter_generate(&img, &request(), None)
        .await
        .unwrap_err();
    let requests = server.requests();
    server.finish();
    assert!(err.to_string().contains("429"), "{err}");
    let paths: Vec<_> = requests.iter().map(|r| r["path"].clone()).collect();
    assert_eq!(paths, ["/chat/completions", "/models"]);
}

const SELECTED: &str = "google/gemma-4-26b-a4b-it:free";

fn reply(path: &str, status: u16, body: &str) -> Reply {
    Reply {
        path: path.into(),
        status,
        body: body.into(),
    }
}
fn vision_request(timeout: Duration) -> GenerationRequest {
    GenerationRequest {
        source: Source::OpenRouter,
        model: VISION_MODELS[0].clone(),
        timeout,
        install_missing: false,
    }
}
fn vision(id: &str, pricing: Value) -> Value {
    json!({"id": id, "architecture": {"input_modalities": ["text", "image"]}, "pricing": pricing})
}
/// Paid, text-only, duplicate and selected rows around the eligible candidates, whose order is
/// other vendors as listed (`other/fails`, `third/answers`), then the selected vendor's sibling.
fn mixed_catalogue() -> String {
    let free = json!({"prompt": "0", "completion": "0"});
    json!({"data": [
        vision(SELECTED, free.clone()),
        vision("paid/vision", json!({"prompt": "0.000001", "completion": "0"})),
        vision("paid/images:free", json!({"prompt": "0", "completion": "0", "image": "0.01"})),
        {"id": "free/text-only:free", "architecture": {"input_modalities": ["text"]}, "pricing": free},
        vision("other/fails:free", free.clone()),
        vision("other/fails:free", free.clone()),
        vision("google/sibling:free", free.clone()),
        vision("third/answers:free", free),
    ]})
    .to_string()
}
/// The selected model fails, the rescue catalogue arrives, its first candidate fails.
fn failing_rescue() -> Vec<Reply> {
    vec![
        reply("/chat/completions", 429, "busy"),
        reply("/models", 200, &mixed_catalogue()),
        reply(
            "/chat/completions",
            500,
            r#"{"error":{"message":"upstream failed"}}"#,
        ),
    ]
}
/// A valid pair answered under an alias, never to become the recorded model.
fn answered() -> Reply {
    reply("/chat/completions", 200, &completion("third/answers"))
}
/// Each request's chat model, or its path when it carries none.
fn attempts(requests: &[Value]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| {
            let model = request["body"]["model"].as_str();
            model.or(request["path"].as_str()).unwrap()
        })
        .collect()
}
/// Our own request, queued behind any request the client left in flight: such a request would
/// reach the server first and fail its path check.
fn probe(server: &Server) {
    use std::io::{Read, Write};
    let address = server.url().trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(stream, "GET /probe HTTP/1.1\r\nHost: probe\r\n\r\n").unwrap();
    let _ = stream.read_to_end(&mut Vec::new());
}

#[tokio::test]
async fn rescue_shares_one_budget_and_sends_nothing_after_it_expires() {
    let expired_selected = (vec![answered()], 0, vec![SELECTED]);
    let mut rescue = failing_rescue();
    rescue.push(answered());
    let expired_rescue = (
        rescue,
        3,
        vec![
            SELECTED,
            "/models",
            "other/fails:free",
            "third/answers:free",
        ],
    );
    for (mut replies, index, mut expected) in [expired_selected, expired_rescue] {
        replies.push(reply("/probe", 200, "{}"));
        expected.push("/probe");
        let (_dir, img) = image();
        let (server, captured, release) = Server::start_paused_at(replies, index);
        let timeout = Duration::from_millis(1500);
        let begun = Instant::now();
        let error = client(server.url())
            .openrouter_generate(&img, &vision_request(timeout), None)
            .await
            .unwrap_err();
        let elapsed = begun.elapsed();
        captured
            .try_recv()
            .expect("the held request was captured before the deadline");
        release.send(()).unwrap();
        probe(&server);
        let requests = server.requests();
        server.finish();
        assert_eq!(
            error.to_string(),
            "provider error: OpenRouter deadline exceeded"
        );
        assert!(elapsed < timeout * 3 / 2, "ended after {elapsed:?}");
        assert_eq!(attempts(&requests), expected);
    }
}

/// A marked SCHEDULED row whose `{n}.{extension}` artwork is 16x8: `left` half, `right` half.
fn two_tone_row(f: &Fixture, n: u64, extension: &str, [left, right]: [&str; 2]) -> Value {
    let name = format!("{n}.{extension}");
    let path = f.path(&name);
    let status = Command::new("magick")
        .args(["-size", "8x8", &format!("xc:{left}")])
        .args(["-size", "8x8", &format!("xc:{right}"), "+append"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let mut publication = common::row(n, "SCHEDULED");
    publication["title"] = json!(GENERATE_MARKER);
    publication["description"] = json!(GENERATE_MARKER);
    publication["files"][0] = json!({"basename": name, "sha512sum": file_sha512(&path).unwrap()});
    publication
}
/// Decodes the image a completion request carried: "MIME FORMAT WIDTH HEIGHT" and RGB bytes.
fn sent_image(request: &Value) -> (String, Vec<u8>) {
    let url = request["body"]["messages"][0]["content"][1]["image_url"]["url"]
        .as_str()
        .unwrap();
    let (mime, data) = url
        .strip_prefix("data:")
        .and_then(|url| url.split_once(";base64,"))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sent");
    std::fs::write(&path, openssl::base64::decode_block(data).unwrap()).unwrap();
    let magick = |args: &[&str]| {
        let output = Command::new("magick")
            .arg(&path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        output.stdout
    };
    let geometry = String::from_utf8(magick(&["-format", "%m %w %h", "info:"])).unwrap();
    (
        format!("{mime} {geometry}"),
        magick(&["-depth", "8", "RGB:-"]),
    )
}
fn assert_artwork(request: &Value, expected: &str, halves: [[u8; 3]; 2]) {
    let (description, rgb) = sent_image(request);
    assert_eq!(description, expected);
    // Representative pixels away from the seam, which lossy WebP may blur.
    for (x, colour) in [(2, halves[0]), (13, halves[1])] {
        let at = (4 * 16 + x) * 3;
        let pixel = &rgb[at..at + 3];
        let close = pixel
            .iter()
            .zip(colour)
            .all(|(&got, want)| got.abs_diff(want) < 48);
        assert!(
            close,
            "{expected}: pixel ({x}, 4) {pixel:?}, expected {colour:?}"
        );
    }
}

#[tokio::test]
async fn batch_sends_the_recorded_artwork_and_keeps_the_attempted_candidate() {
    const RED: [u8; 3] = [255, 0, 0];
    const BLUE: [u8; 3] = [0, 0, 255];
    let f = Fixture::new();
    f.write(json!([
        two_tone_row(&f, 1, "png", ["red", "blue"]),
        // AI input accepts WebP; only DeviantArt uploads must not be WebP.
        two_tone_row(&f, 2, "webp", ["blue", "red"]),
    ]));
    let mut replies = failing_rescue();
    // The next record must start from the attempted ID, not the answer's alias.
    replies.extend([answered(), answered()]);
    let server = Server::start(replies);
    let app = Application::new(Arc::new(f.store.clone()), client(server.url()));
    let report = app
        .generate_marked(&vision_request(Duration::from_secs(10)))
        .await
        .unwrap();
    let requests = server.requests();
    server.finish();
    assert_eq!((report.succeeded, report.failed), (2, 0));
    // OpenRouter batches run newest first: row 2 is rescued, row 1 starts on the kept candidate.
    assert_eq!(
        attempts(&requests),
        [
            SELECTED,
            "/models",
            "other/fails:free",
            "third/answers:free",
            "third/answers:free"
        ]
    );
    for at in [0, 2, 3] {
        assert_artwork(&requests[at], "image/webp WEBP 16 8", [BLUE, RED]);
    }
    assert_artwork(&requests[4], "image/png PNG 16 8", [RED, BLUE]);
    for saved in f.read().as_array().unwrap() {
        assert_eq!(
            (&saved["title"], &saved["state"]),
            (&json!("T"), &json!("DESCRIBED"))
        );
    }
}

#[allow(dead_code)]
fn _api_type(_: Value) {}
