mod common;
use common::http::{Reply, Server};
use common::{Fixture, id, row};
use publicator::{
    application::Application,
    config::{Config, Paths},
    metadata::{Endpoints, GenerationRequest, MetadataClient, Source, VISION_MODELS},
    store::Store,
};
use serde_json::json;
use std::{
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

fn request(source: Source) -> GenerationRequest {
    GenerationRequest {
        source,
        model: VISION_MODELS[0].clone(),
        timeout: Duration::from_secs(2),
        install_missing: false,
    }
}
fn reply(path: &str, body: serde_json::Value) -> Reply {
    Reply {
        path: path.into(),
        status: 200,
        body: body.to_string(),
    }
}
/// Each record's OpenRouter failure: the selected model answers 500, then the rescue catalogue does.
fn provider_failures(records: usize) -> Vec<Reply> {
    let failure = |path: &str| Reply {
        path: path.into(),
        status: 500,
        body: "fixture failure".into(),
    };
    (0..records)
        .flat_map(|_| [failure("/chat/completions"), failure("/models")])
        .collect()
}
fn completion(title: &str) -> Reply {
    reply(
        "/chat/completions",
        json!({"choices":[{"message":{"content":content(title)}}]}),
    )
}
fn content(title: &str) -> String {
    json!({"title":title,"description":"A complete generated description."}).to_string()
}
fn local_client(endpoint: String, llmfit: &std::path::Path) -> MetadataClient {
    MetadataClient::new(
        reqwest::Client::new(),
        Endpoints {
            openrouter: "http://127.0.0.1:1".into(),
            ollama: endpoint,
        },
        llmfit.to_path_buf(),
    )
}
fn client(endpoint: String) -> MetadataClient {
    MetadataClient::new(
        reqwest::Client::new(),
        Endpoints {
            openrouter: endpoint.clone(),
            ollama: endpoint,
        },
        "llmfit".into(),
    )
    .with_openrouter_key("fixture")
}

#[tokio::test]
async fn marked_batch_uses_configuration_from_data_dir_for_external_json() {
    let f = Fixture::new();
    f.image("art.png", 4, 4);
    std::fs::write(
        f.path("publicator.toml"),
        "[deviantart]\ngalleries = ['Allowed']\n",
    )
    .unwrap();
    let config = Config::load(f.root.path()).unwrap();
    let external = tempfile::tempdir().unwrap();
    let json_path = external.path().join("rows.json");

    let make_row = |gallery: &str| {
        let mut publication = row(1, "SCHEDULED");
        publication["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        publication["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        publication["files"][0]["basename"] = json!("art.png");
        publication["files"][0]["sha512sum"] =
            json!(publicator::images::file_sha512(&f.path("art.png")).unwrap());
        publication["apparitions"] = json!([{
            "platformName":"deviantart", "state":"unpublished", "galleries":[gallery]
        }]);
        publication
    };

    let allowed_paths = Paths::resolve(Some(f.root.path()), Some(&json_path)).unwrap();
    let allowed_store = Store::new(allowed_paths, config.clone()).unwrap();
    std::fs::write(&json_path, json!([make_row("Allowed")]).to_string()).unwrap();
    let server = Server::start(vec![reply(
        "/chat/completions",
        json!({"choices":[{"message":{"content":content("Allowed title")}}]}),
    )]);
    let allowed_app = Application::new(Arc::new(allowed_store), client(server.url().to_owned()));
    let report = allowed_app
        .generate_marked(&request(Source::OpenRouter))
        .await
        .unwrap();
    assert_eq!((report.succeeded, report.failed), (1, 0));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&json_path).unwrap()).unwrap()
            [0]["title"],
        "Allowed title"
    );
    assert_eq!(server.requests().len(), 1);

    std::fs::write(&json_path, json!([make_row("Not allowed")]).to_string()).unwrap();
    let invalid_paths = Paths::resolve(Some(f.root.path()), Some(&json_path)).unwrap();
    let invalid_store = Store::new(invalid_paths, config).unwrap();
    let invalid_app =
        Application::new(Arc::new(invalid_store), client("http://127.0.0.1:1".into()));
    let before = std::fs::read(&json_path).unwrap();
    let error = invalid_app
        .generate_marked(&request(Source::OpenRouter))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Not allowed"));
    assert_eq!(std::fs::read(&json_path).unwrap(), before);
}

#[tokio::test]
async fn empty_local_marked_batch_is_noop_without_model_or_service_setup() {
    let f = Fixture::new();
    f.write(json!([row(1, "SCHEDULED")]));
    let marker = f.path("llmfit-called");
    let tool = f.path("llmfit");
    common::executable::write_executable(
        &tool,
        &format!(
            "#!/bin/sh\ntouch '{}'\nprintf '%s' '{{\"models\":[]}}'\n",
            marker.display()
        ),
    );
    let app = Application::new(
        Arc::new(f.store.clone()),
        local_client("http://127.0.0.1:1".into(), &tool),
    );
    let report = app.generate_marked(&request(Source::Local)).await.unwrap();
    assert_eq!(
        (report.succeeded, report.failed, report.results.len()),
        (0, 0, 0)
    );
    assert!(!marker.exists());
    assert_eq!(f.read()[0], row(1, "SCHEDULED"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marked_batch_admission_uses_eligibility_after_a_waiting_edit() {
    let f = Fixture::new();
    f.image("1.png", 4, 4);
    let mut publication = row(1, "SCHEDULED");
    publication["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    publication["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    f.write(json!([publication]));
    let app = Arc::new(Application::new(
        Arc::new(f.store.clone()),
        client("http://127.0.0.1:1".into()),
    ));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mutation_app = Arc::clone(&app);
    let mutation = tokio::task::spawn_blocking(move || {
        mutation_app.mutate(Some(&id(1)), |store| {
            store.mutate(|rows| {
                rows[0]["title"] = json!("Manual title");
                rows[0]["description"] = json!("Manual description");
                Ok(())
            })?;
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let batch_app = Arc::clone(&app);
    let (started_tx, started_rx) = mpsc::channel();
    let batch = tokio::spawn(async move {
        started_tx.send(()).unwrap();
        batch_app
            .generate_marked(&request(Source::OpenRouter))
            .await
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    release_tx.send(()).unwrap();
    mutation.await.unwrap().unwrap();

    let report = batch.await.unwrap().unwrap();
    assert_eq!(
        (report.succeeded, report.failed, report.results.len()),
        (0, 0, 0)
    );
    assert_eq!(f.read()[0]["title"], "Manual title");
    assert_eq!(f.read()[0]["description"], "Manual description");
}

#[tokio::test]
async fn generation_lease_excludes_duplicate_and_same_uuid_edits_but_not_other_work() {
    let f = Fixture::new();
    f.write(json!([f.art_row(1, "SCHEDULED"), row(2, "SCHEDULED")]));
    let (server, app, task, release_tx) = paused_generation(&f, common::http::Reply {
        path: "/chat/completions".into(),
        status: 200,
        body: json!({"choices":[{"message":{"content":json!({"title":"Generated title","description":"A generated description."}).to_string()}}]}).to_string(),
    })
    .await;
    assert!(app.mutate(Some(&id(1)), |_| Ok(())).is_err());
    assert!(
        app.generate_one(&id(1), request(Source::OpenRouter))
            .await
            .is_err()
    );
    assert!(app.begin_publishing().is_err());
    app.mutate(Some(&id(2)), |_| Ok(())).unwrap();
    release_tx.send(()).unwrap();
    assert!(task.await.unwrap().is_ok());
    server.finish();
    assert_eq!(f.read()[0]["title"], "Generated title");
}

#[tokio::test]
async fn openrouter_batch_continues_after_failure_and_ignores_inexact_marker() {
    let f = Fixture::new();
    let mut rows = Vec::new();
    for n in 1..=3 {
        let mut publication = f.art_row(n, "SCHEDULED");
        publication["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        publication["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        rows.push(publication);
    }
    rows[2]["title"] = json!("[[PUBLICATOR:TODO_GENERATE]] ");
    rows[2]["description"] = json!("[[PUBLICATOR:TODO_GENERATE]] ");
    f.write(json!(rows));
    let server = Server::start(vec![
        Reply {
            path: "/chat/completions".into(),
            status: 500,
            body: json!({"error":{"message":"fixture failure"}}).to_string(),
        },
        reply("/models", json!({"data":[]})),
        reply(
            "/chat/completions",
            json!({"choices":[{"message":{"content":content("Generated")}}]}),
        ),
    ]);
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));

    let report = app
        .generate_marked(&request(Source::OpenRouter))
        .await
        .unwrap();
    assert_eq!((report.succeeded, report.failed), (1, 1));
    assert_eq!(
        report
            .results
            .iter()
            .map(|result| result.uuid.as_str())
            .collect::<Vec<_>>(),
        [id(2).as_str(), id(1).as_str()]
    );
    let saved = f.read();
    assert_eq!(saved[0]["title"], "Generated");
    assert_eq!(saved[0]["description"], "A complete generated description.");
    assert_eq!(saved[1]["title"], "[[PUBLICATOR:TODO_GENERATE]]");
    assert_eq!(saved[1]["description"], "[[PUBLICATOR:TODO_GENERATE]]");
    assert_eq!(saved[2]["title"], "[[PUBLICATOR:TODO_GENERATE]] ");
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["path"] == "/chat/completions")
            .count(),
        2
    );
}

#[tokio::test]
async fn batch_persists_each_success_before_continuing_after_failure() {
    let f = Fixture::new();
    let mut rows = Vec::new();
    for n in 1..=3 {
        let mut publication = f.art_row(n, "SCHEDULED");
        publication["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        publication["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        rows.push(publication);
    }
    f.write(json!(rows));
    // The second (failing) record's request is held until the first success is on disk.
    let mut replies = vec![completion("title-3")];
    replies.extend(provider_failures(1));
    replies.push(completion("title-1"));
    let (server, started, release) = Server::start_paused_at(replies, 1);
    let app = Arc::new(Application::new(
        Arc::new(f.store.clone()),
        client(server.url().to_owned()),
    ));
    let batch = {
        let app = Arc::clone(&app);
        tokio::spawn(async move { app.generate_marked(&request(Source::OpenRouter)).await })
    };
    tokio::task::spawn_blocking(move || started.recv_timeout(Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    let persisted = f.read();
    release.send(()).unwrap();
    let report = batch.await.unwrap().unwrap();
    server.finish();
    assert_eq!(persisted[2]["title"], "title-3", "saved before continuing");
    assert_eq!((report.succeeded, report.failed), (2, 1));
    assert_eq!(
        report
            .results
            .iter()
            .map(|result| result.uuid.as_str())
            .collect::<Vec<_>>(),
        [id(3).as_str(), id(2).as_str(), id(1).as_str()]
    );
    let saved = f.read();
    assert_eq!(saved[0]["title"], "title-1");
    assert_eq!(saved[0]["description"], "A complete generated description.");
    assert_eq!(saved[1]["title"], "[[PUBLICATOR:TODO_GENERATE]]");
    assert_eq!(saved[1]["description"], "[[PUBLICATOR:TODO_GENERATE]]");
    assert_eq!(saved[2]["title"], "title-3");
    assert_eq!(saved[2]["description"], "A complete generated description.");
}

#[tokio::test]
async fn successful_batches_save_in_provider_order() {
    for (source, expected) in [
        (Source::OpenRouter, vec![3, 2, 1]),
        (Source::Local, vec![1, 2, 3]),
    ] {
        let f = Fixture::new();
        let mut rows = Vec::new();
        for n in 1..=3 {
            let mut p = f.art_row(n, "SCHEDULED");
            p["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
            p["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
            rows.push(p);
        }
        f.write(json!(rows));
        let replies = if source == Source::OpenRouter {
            expected
                .iter()
                .map(|n| {
                    reply(
                        "/chat/completions",
                        json!({"choices":[{"message":{"content":content(&format!("title-{n}"))}}]}),
                    )
                })
                .collect()
        } else {
            let mut replies = vec![reply(
                "/api/tags",
                json!({"models":[{"name":VISION_MODELS[0].ollama_name}]}),
            )];
            for n in &expected {
                replies.push(reply(
                    "/api/tags",
                    json!({"models":[{"name":VISION_MODELS[0].ollama_name}]}),
                ));
                replies.push(reply(
                    "/api/chat",
                    json!({"message":{"content":content(&format!("title-{n}"))}}),
                ));
            }
            replies
        };
        let server = Server::start(replies);
        let metadata = if source == Source::OpenRouter {
            client(server.url().to_owned())
        } else {
            let tool = f.path("llmfit");
            common::executable::write_executable(
                &tool,
                &format!(
                    "#!/bin/sh\nprintf '%s' '{{\"models\":[{{\"name\":\"{}\",\"fit_level\":\"Good\"}}]}}'\n",
                    VISION_MODELS[0].llmfit_name
                ),
            );
            local_client(server.url().to_owned(), &tool)
        };
        let app = Application::new(Arc::new(f.store.clone()), metadata);
        let report = app.generate_marked(&request(source)).await.unwrap();
        assert_eq!((report.succeeded, report.failed), (3, 0));
        assert_eq!(
            report
                .results
                .iter()
                .map(|r| r.uuid.clone())
                .collect::<Vec<_>>(),
            expected.iter().map(|n| id(*n)).collect::<Vec<_>>()
        );
        for n in 1..=3 {
            assert_eq!(f.read()[n - 1]["title"], format!("title-{n}"));
            assert_eq!(f.read()[n - 1]["state"], "DESCRIBED");
        }
    }
}

#[tokio::test]
async fn publishing_lease_blocks_edits_until_dropped() {
    let f = Fixture::new();
    f.write(json!([row(1, "SCHEDULED")]));
    let app = Application::new(
        Arc::new(f.store.clone()),
        client("http://127.0.0.1:1".into()),
    );
    let lease = app.begin_publishing().unwrap();
    assert!(app.mutate(Some(&id(1)), |_| Ok(())).is_err());
    drop(lease);
    assert!(app.mutate(Some(&id(1)), |_| Ok(())).is_ok());
}

#[tokio::test]
async fn invalid_phase_model_and_database_do_not_write_or_call_provider() {
    use std::io::ErrorKind;

    let f = Fixture::new();
    f.image("1.png", 4, 4);
    f.write(json!([row(1, "PICKED")]));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (thread_calls, thread_stop) = (Arc::clone(&calls), Arc::clone(&stop));
    let server = thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((_, _)) => {
                    thread_calls.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("counter listener failed: {error}"),
            }
        }
    });
    let app = Application::new(Arc::new(f.store.clone()), client(endpoint));
    let before_phase = std::fs::read(&f.store.paths().json).unwrap();
    assert!(
        app.generate_one(&id(1), request(Source::OpenRouter))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before_phase);

    f.store
        .mutate(|rows| {
            rows[0]["state"] = json!("SCHEDULED");
            Ok(())
        })
        .unwrap();
    let before_model = std::fs::read(&f.store.paths().json).unwrap();
    let mut invalid = request(Source::OpenRouter);
    invalid.model.ollama_name = "unapproved".into();
    assert!(app.generate_one(&id(1), invalid).await.is_err());
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before_model);

    for invalid_field in ["uuid", "submissionTimestamp"] {
        let mut invalid = row(1, "SCHEDULED");
        invalid["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        invalid["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        if invalid_field == "uuid" {
            invalid["uuid"] = json!("short");
        } else {
            invalid.as_object_mut().unwrap().remove(invalid_field);
        }
        f.write(json!([invalid]));
        let before_invalid = std::fs::read(&f.store.paths().json).unwrap();
        assert!(
            app.generate_marked(&request(Source::OpenRouter))
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(&f.store.paths().json).unwrap(),
            before_invalid,
            "invalid {invalid_field} database was rewritten"
        );
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn accepted_provider_failure_saves_both_markers() {
    let f = Fixture::new();
    f.write(json!([f.art_row(1, "SCHEDULED")]));
    let server = Server::start(provider_failures(1));
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));
    let result = app.generate_one(&id(1), request(Source::OpenRouter)).await;
    server.finish();
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OpenRouter HTTP 500"), "{error}");
    assert_eq!(f.read()[0]["title"], "[[PUBLICATOR:TODO_GENERATE]]");
    assert_eq!(f.read()[0]["description"], "[[PUBLICATOR:TODO_GENERATE]]");
}

#[tokio::test]
async fn openrouter_batch_reverses_persisted_order_and_keeps_marker_on_failure() {
    let f = Fixture::new();
    let mut rows = vec![];
    for n in 1..=3 {
        let mut p = f.art_row(n, "SCHEDULED");
        p["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        rows.push(p);
    }
    f.write(json!(rows));
    let server = Server::start(provider_failures(3));
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));
    let report = app
        .generate_marked(&request(Source::OpenRouter))
        .await
        .unwrap();
    server.finish();
    assert_eq!(
        report
            .results
            .iter()
            .map(|r| r.uuid.as_str())
            .collect::<Vec<_>>(),
        vec![id(3).as_str(), id(2).as_str(), id(1).as_str()]
    );
    assert_eq!((report.succeeded, report.failed), (0, 3));
    assert!(
        f.read()
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["description"] == "[[PUBLICATOR:TODO_GENERATE]]")
    );
}

#[tokio::test]
async fn batch_keeps_first_successful_safe_model_as_preference() {
    let f = Fixture::new();
    let mut rows = Vec::new();
    for n in 1..=3 {
        let mut p = f.art_row(n, "SCHEDULED");
        p["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        p["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
        rows.push(p);
    }
    f.write(json!(rows));
    let catalog = json!({"data":[
        {"id":"vendor/model-b:free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"}},
        {"id":"google/model-c:free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"}}
    ]});
    let success = |title: &str| {
        reply(
            "/chat/completions",
            json!({"choices":[{"message":{"content":content(title)}}]}),
        )
    };
    let failed = || Reply {
        path: "/chat/completions".into(),
        status: 500,
        body: json!({"error":{"message":"temporary"}}).to_string(),
    };
    let server = Server::start(vec![
        failed(),
        reply("/models", catalog.clone()),
        success("one"),
        failed(),
        success("two"),
        success("three"),
    ]);
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));
    let report = app
        .generate_marked(&request(Source::OpenRouter))
        .await
        .unwrap();
    assert_eq!((report.succeeded, report.failed), (3, 0));
    let models = server
        .requests()
        .iter()
        .map(|r| r["body"]["model"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(
        models,
        vec![
            Some("google/gemma-4-26b-a4b-it:free".into()),
            None,
            Some("vendor/model-b:free".into()),
            Some("vendor/model-b:free".into()),
            Some("google/model-c:free".into()),
            Some("vendor/model-b:free".into()),
        ]
    );
}

fn generated_reply() -> Reply {
    reply(
        "/chat/completions",
        json!({"choices":[{"message":{"content":content("Generated")}}]}),
    )
}

#[tokio::test]
async fn changed_artwork_bytes_make_no_provider_call_and_save_nothing() {
    let f = Fixture::new();
    let mut publication = f.art_row(1, "SCHEDULED");
    publication["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    publication["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    f.write(json!([publication]));
    f.colored("1.png", "blue");
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let server = Server::start(vec![generated_reply()]);
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));

    let error = app
        .generate_one(&id(1), request(Source::OpenRouter))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        publicator::error::Error::ReconciliationRequired(_)
    ));
    let report = app
        .generate_marked(&request(Source::OpenRouter))
        .await
        .unwrap();
    assert_eq!((report.succeeded, report.failed), (0, 1));
    assert!(server.requests().is_empty());
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
    assert!(!f.path(".thumbs").exists());
}

#[tokio::test]
async fn duplicate_basenames_resolve_to_the_recorded_hash_in_any_directory_order() {
    for recorded in ["a", "b"] {
        let f = Fixture::new();
        // a/ is both named and created first, so a name-, creation- or reverse-ordered walk meets
        // the unrecorded duplicate first in one of the two cases.
        let color = |dir| if dir == recorded { "red" } else { "blue" };
        let a = f.colored("a/1.png", color("a"));
        let b = f.colored("b/1.png", color("b"));
        let (red, blue) = if recorded == "a" { (a, b) } else { (b, a) };
        let (red_hash, blue_hash) = (
            publicator::images::file_sha512(&red).unwrap(),
            publicator::images::file_sha512(&blue).unwrap(),
        );
        let mut publication = row(1, "SCHEDULED");
        publication["files"][0]["sha512sum"] = json!(red_hash);
        f.write(json!([publication]));
        let server = Server::start(vec![generated_reply()]);
        let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));

        app.generate_one(&id(1), request(Source::OpenRouter))
            .await
            .unwrap();
        let thumbs = f.path(".thumbs");
        let thumb = |path: &std::path::Path, hash: &str| {
            thumbs.join(publicator::images::thumb_name(path, hash).unwrap())
        };
        assert!(!thumb(&blue, &blue_hash).exists());
        let [request] = server.requests().try_into().unwrap();
        let red_thumb = std::fs::read(thumb(&red, &red_hash)).unwrap();
        let sent = openssl::base64::encode_block(&red_thumb);
        assert!(
            request.to_string().contains(&sent),
            "not the recorded image"
        );
    }
}

type Generation =
    tokio::task::JoinHandle<publicator::error::Result<publicator::metadata::Generated>>;

/// Starts `generate_one(id(1))` and returns once the server, paused before `reply`, captured it.
async fn paused_generation(
    f: &Fixture,
    reply: Reply,
) -> (Server, Arc<Application>, Generation, mpsc::Sender<()>) {
    let (server, started_rx, release_tx) = Server::start_paused(reply);
    let app = Arc::new(Application::new(
        Arc::new(f.store.clone()),
        client(server.url().to_owned()),
    ));
    let task = {
        let app = Arc::clone(&app);
        tokio::spawn(async move { app.generate_one(&id(1), request(Source::OpenRouter)).await })
    };
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    (server, app, task, release_tx)
}

/// Pauses a single generation at its captured provider request, saves a newer draft through
/// another Store writer, then releases `reply`. Returns the saved row and the generation outcome.
async fn complete_after_newer_edit(reply: Reply) -> (serde_json::Value, bool) {
    let f = Fixture::new();
    f.write(json!([f.art_row(1, "SCHEDULED")]));
    let (server, app, task, release_tx) = paused_generation(&f, reply).await;
    let newer = publicator::workflow::MetadataPatch {
        title: Some("Newer title".into()),
        description: Some("Newer description".into()),
    };
    publicator::workflow::save_metadata(&f.store, &id(1), &newer).unwrap();
    release_tx.send(()).unwrap();
    let outcome = task.await.unwrap();
    server.finish();
    app.mutate(Some(&id(1)), |_| Ok(()))
        .expect("generation lease is released");
    (f.read()[0].clone(), outcome.is_ok())
}

#[tokio::test]
async fn single_generation_completion_keeps_fields_saved_by_another_writer() {
    let malformed = reply(
        "/chat/completions",
        json!({"choices":[{"message":{"content":"not a metadata object"}}]}),
    );
    for (reply, succeeded) in [(generated_reply(), true), (malformed, false)] {
        let (saved, outcome) = complete_after_newer_edit(reply).await;
        assert_eq!(outcome, succeeded);
        assert_eq!(saved["title"], "Newer title");
        assert_eq!(saved["description"], "Newer description");
        assert_eq!(
            saved["state"], "SCHEDULED",
            "a newer edit is never approved"
        );
    }
}

const STALLED_TOOL_DIR: &str = "PUBLICATOR_TEST_STALLED_TOOL_DIR";

/// True once `condition` holds, polling until `limit` elapses.
fn eventually(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while !condition() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

/// Whether `pid` is a live process, neither reaped nor a zombie.
fn running(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        let state = stat.rsplit(')').next().unwrap_or("").trim_start();
        !state.starts_with(['Z', 'X'])
    })
}

/// The parent is a watchdog: only its child test process gets a PATH whose `identify` records
/// its PID and a descendant's, then blocks. Whatever outlives the child is killed and fails.
#[tokio::test]
async fn stalled_image_preparation_fails_within_the_generation_deadline() {
    if let Some(dir) = std::env::var_os(STALLED_TOOL_DIR) {
        return stalled_preparation_child(std::path::Path::new(&dir)).await;
    }
    let bin = tempfile::tempdir().unwrap();
    let script = format!(
        "#!/bin/sh\nsleep 1000 &\necho \"$$ $!\" > '{}'\nwait\n",
        bin.path().join("pids").display()
    );
    common::executable::write_executable(&bin.path().join("identify"), &script);
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin.path().to_owned()).chain(std::env::split_paths(&inherited)),
    )
    .unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "stalled_image_preparation_fails_within_the_generation_deadline",
            "--nocapture",
        ])
        .env("PATH", path)
        .env(STALLED_TOOL_DIR, bin.path())
        .spawn()
        .unwrap();
    let exited = eventually(Duration::from_secs(60), || {
        child.try_wait().unwrap().is_some()
    });
    if !exited {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    let pids = std::fs::read_to_string(bin.path().join("pids")).unwrap_or_default();
    let lingering: Vec<&str> = pids.split_whitespace().filter(|pid| running(pid)).collect();
    for pid in &lingering {
        unsafe { libc::kill(pid.parse().unwrap(), libc::SIGKILL) };
    }
    assert!(
        exited,
        "generation outlived the watchdog: preparation is unbounded"
    );
    assert!(status.success(), "stalled-preparation child failed");
    assert!(
        lingering.is_empty(),
        "image tools outlived generation: {lingering:?}"
    );
}

async fn stalled_preparation_child(bin: &std::path::Path) {
    let f = Fixture::new();
    f.write(json!([f.art_row(1, "SCHEDULED")]));
    let server = Server::start(vec![generated_reply()]);
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));
    let mut bounded = request(Source::OpenRouter);
    bounded.timeout = Duration::from_millis(500);
    let started = std::time::Instant::now();
    let result = app.generate_one(&id(1), bounded).await;
    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(server.requests().is_empty(), "no provider call");
    app.mutate(Some(&id(1)), |_| Ok(()))
        .expect("generation lease is released");
    drop(app.begin_publishing().expect("no reservation remains"));
    let pids = std::fs::read_to_string(bin.join("pids")).expect("identify was entered");
    let (tool, descendant) = pids.trim().split_once(' ').unwrap();
    // Only this process can reap its child, so a vanished /proc entry proves the app reaped it.
    assert!(
        eventually(Duration::from_secs(5), || {
            !std::path::Path::new(&format!("/proc/{tool}")).exists()
        }),
        "stalled image tool was not reaped"
    );
    assert!(
        eventually(Duration::from_secs(5), || !running(descendant)),
        "image tool descendant outlived the deadline"
    );
}

/// `f.art_row(n)` with both fields carrying the exact generation marker.
fn marked(f: &Fixture, n: u64) -> serde_json::Value {
    let mut publication = f.art_row(n, "SCHEDULED");
    publication["title"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    publication["description"] = json!("[[PUBLICATOR:TODO_GENERATE]]");
    publication
}

/// A local app whose llmfit touches `entered`, then reports a fit once `release` exists.
/// Ollama is unreachable, so a released preflight fails as unavailable.
fn gated_local_app(f: &Fixture) -> (Arc<Application>, std::path::PathBuf, std::path::PathBuf) {
    let (entered, release, tool) = (f.path("entered"), f.path("release"), f.path("llmfit"));
    let script = format!(
        "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '%s' '{{\"models\":[{{\"name\":\"{}\",\"fit_level\":\"Good\"}}]}}'\n",
        entered.display(),
        release.display(),
        VISION_MODELS[0].llmfit_name
    );
    common::executable::write_executable(&tool, &script);
    let metadata = local_client("http://127.0.0.1:1".into(), &tool);
    let app = Arc::new(Application::new(Arc::new(f.store.clone()), metadata));
    (app, entered, release)
}

/// A local request whose deadline outlasts any gated pause in these tests.
fn patient_local() -> GenerationRequest {
    GenerationRequest {
        timeout: Duration::from_secs(30),
        ..request(Source::Local)
    }
}

async fn wait_for(path: std::path::PathBuf) {
    let entered =
        tokio::task::spawn_blocking(move || eventually(Duration::from_secs(5), || path.exists()));
    assert!(entered.await.unwrap(), "fake llmfit was not entered");
}

#[tokio::test]
async fn reordered_expected_batch_is_rejected_before_any_local_work() {
    let f = Fixture::new();
    f.write(json!([marked(&f, 1), marked(&f, 2)]));
    let (app, entered, release) = gated_local_app(&f);
    std::fs::write(release, "").unwrap();
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let reordered = [id(2), id(1)];
    let error = app
        .generate_marked_expected(&patient_local(), &reordered)
        .await
        .unwrap_err();
    assert!(matches!(error, publicator::error::Error::InvalidState(_)));
    assert!(!entered.exists(), "llmfit ran for a changed batch");
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
    drop(
        app.begin_publishing()
            .expect("a rejected batch reserves nothing"),
    );
}

#[tokio::test]
async fn paused_batch_preflight_reserves_exactly_the_admitted_rows() {
    let f = Fixture::new();
    f.write(json!([
        marked(&f, 1),
        marked(&f, 2),
        f.art_row(3, "SCHEDULED")
    ]));
    let (app, entered, release) = gated_local_app(&f);
    let batch = {
        let app = Arc::clone(&app);
        tokio::spawn(async move { app.generate_marked(&patient_local()).await })
    };
    wait_for(entered).await;
    for reserved in [id(1), id(2)] {
        assert!(app.mutate(Some(&reserved), |_| Ok(())).is_err());
    }
    app.mutate(Some(&id(3)), |_| Ok(()))
        .expect("an unrelated row stays editable");
    std::fs::write(release, "").unwrap();
    assert!(batch.await.unwrap().is_err(), "Ollama is unreachable");
    app.mutate(Some(&id(1)), |_| Ok(()))
        .expect("the failed preflight released the batch");
}

#[tokio::test]
async fn aborted_generation_releases_its_reservation() {
    let f = Fixture::new();
    f.write(json!([marked(&f, 1)]));
    let (app, entered, release) = gated_local_app(&f);
    let task = {
        let app = Arc::clone(&app);
        tokio::spawn(async move { app.generate_one(&id(1), patient_local()).await })
    };
    wait_for(entered).await;
    assert!(app.mutate(Some(&id(1)), |_| Ok(())).is_err());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    std::fs::write(release, "").unwrap();
    app.mutate(Some(&id(1)), |_| Ok(()))
        .expect("the aborted task released its UUID");
    drop(app.begin_publishing().expect("no reservation remains"));
}

#[tokio::test]
async fn invalid_single_generation_source_runs_no_local_preflight() {
    let f = Fixture::new();
    let (app, entered, release) = gated_local_app(&f);
    std::fs::write(release, "").unwrap();
    let mut unscheduled = f.art_row(1, "PICKED");
    unscheduled["title"] = json!("Kept");
    let mut incomplete = marked(&f, 1);
    incomplete
        .as_object_mut()
        .unwrap()
        .remove("submissionTimestamp");
    for source in [unscheduled, row(1, "SCHEDULED"), incomplete] {
        f.write(json!([source]));
        let before = std::fs::read(&f.store.paths().json).unwrap();
        assert!(app.generate_one(&id(1), patient_local()).await.is_err());
        assert!(!entered.exists(), "llmfit ran for invalid source data");
        assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
    }
}
