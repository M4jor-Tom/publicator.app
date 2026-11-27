mod common;
use common::http::{Reply, Server, completion, content};
use common::{Fixture, eventually, id, row, run_child_test, running, with_blank_metadata};
use publicator::{
    application::Application,
    config::{Config, Paths},
    metadata::{Endpoints, GenerationRequest, MetadataClient, Source, VISION_MODELS},
    store::Store,
};
use serde_json::json;
use std::{
    sync::{Arc, mpsc},
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
/// Each record's OpenRouter failure: the selected model answers 500, then the rescue catalogue does.
fn provider_failures(records: usize) -> Vec<Reply> {
    let failure = |path: &str| Reply::new(path, 500, "fixture failure");
    (0..records)
        .flat_map(|_| [failure("/chat/completions"), failure("/models")])
        .collect()
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
        let mut publication = with_blank_metadata(row(1, "SCHEDULED"));
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
    let server = Server::start(vec![completion("Allowed title")]);
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
    f.write(json!([with_blank_metadata(row(1, "SCHEDULED"))]));
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
    let (server, app, task, release_tx) =
        paused_generation(&f, completion("Generated title")).await;
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
async fn openrouter_batch_continues_after_failure_and_skips_a_complete_draft() {
    let f = Fixture::new();
    let mut rows: Vec<_> = (1..=3).map(|n| f.marked_row(n)).collect();
    rows[0]["title"] = json!("Manual title");
    rows[2]["title"] = json!("Complete title");
    rows[2]["description"] = json!("Complete description");
    f.write(json!(rows));
    let server = Server::start(vec![
        Reply::new(
            "/chat/completions",
            500,
            json!({"error":{"message":"fixture failure"}}),
        ),
        Reply::new("/models", 200, json!({"data":[]})),
        completion("Generated"),
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
    // The partial draft gets only its blank description and validates.
    assert_eq!(saved[0]["title"], "Manual title");
    assert_eq!(saved[0]["description"], "A complete generated description.");
    assert_eq!(saved[0]["state"], "DESCRIBED");
    // The failed record is left exactly as it was.
    assert_eq!(saved[1], rows[1]);
    // The complete draft was never admitted.
    assert_eq!(saved[2], rows[2]);
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
    let rows: Vec<_> = (1..=3).map(|n| f.marked_row(n)).collect();
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
    assert_eq!(saved[1], rows[1], "a failed record is left unchanged");
    assert_eq!(saved[2]["title"], "title-3");
    assert_eq!(saved[2]["description"], "A complete generated description.");
}

#[tokio::test]
async fn batch_reports_progress_and_releases_each_finished_record() {
    use common::http::{manual_clock, received};
    use publicator::application::{BatchEvent, RecordResult};
    let f = Fixture::new();
    let mut rows: Vec<_> = (1..=3).map(|n| f.marked_row(n)).collect();
    rows[2]["title"] = json!("Manual title");
    f.write(json!(rows));
    let (limited, observed) = Reply::new("/chat/completions", 429, "busy").confirm_skipped_body();
    // The second record's request is held while the first record's uuid is edited.
    let replies = vec![
        limited,
        completion("title-3"),
        completion("title-2"),
        completion("title-1"),
    ];
    let (server, started, release) = Server::start_paused_at(replies, 2);
    let app = Arc::new(Application::new(
        Arc::new(f.store.clone()),
        client(server.url().to_owned()),
    ));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _clock = manual_clock();
    let patient = GenerationRequest {
        timeout: Duration::from_secs(15),
        ..request(Source::OpenRouter)
    };
    let batch = {
        let (app, events) = (Arc::clone(&app), Arc::clone(&events));
        tokio::spawn(async move {
            app.generate_marked_with_events(&patient, None, |event| {
                events.lock().unwrap().push(event)
            })
            .await
        })
    };
    received(&observed).await;
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            BatchEvent::Admitted(vec![id(3), id(2), id(1)]),
            BatchEvent::Started(id(3))
        ]
    );
    tokio::time::advance(Duration::from_millis(1001)).await;
    received(&started).await;
    let persisted = f.read();
    assert_eq!(persisted[2]["title"], "Manual title");
    assert_eq!(
        persisted[2]["description"],
        "A complete generated description."
    );
    assert_eq!(persisted[2]["state"], "DESCRIBED");
    app.mutate(Some(&id(3)), |_| Ok(()))
        .expect("a finished record accepts edits");
    assert!(app.mutate(Some(&id(2)), |_| Ok(())).is_err());
    assert!(app.mutate(Some(&id(1)), |_| Ok(())).is_err());
    assert!(app.begin_publishing().is_err());
    release.send(()).unwrap();
    let report = batch.await.unwrap().unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0], requests[1], "same image and model");
    server.finish();
    let finished = |n| {
        BatchEvent::Finished(RecordResult {
            uuid: id(n),
            saved: true,
            error: None,
        })
    };
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            BatchEvent::Admitted(vec![id(3), id(2), id(1)]),
            BatchEvent::Started(id(3)),
            finished(3),
            BatchEvent::Started(id(2)),
            finished(2),
            BatchEvent::Started(id(1)),
            finished(1),
        ]
    );
    assert_eq!((report.succeeded, report.failed), (3, 0));
    app.begin_publishing().unwrap();
}

#[tokio::test]
async fn successful_batches_save_in_provider_order() {
    for (source, expected) in [
        (Source::OpenRouter, vec![3, 2, 1]),
        (Source::Local, vec![1, 2, 3]),
    ] {
        let f = Fixture::new();
        let rows: Vec<_> = (1..=3).map(|n| f.marked_row(n)).collect();
        f.write(json!(rows));
        let replies = if source == Source::OpenRouter {
            expected
                .iter()
                .map(|n| completion(&format!("title-{n}")))
                .collect()
        } else {
            let mut replies = vec![Reply::new(
                "/api/tags",
                200,
                json!({"models":[{"name":VISION_MODELS[0].ollama_name}]}),
            )];
            for n in &expected {
                replies.push(Reply::new(
                    "/api/tags",
                    200,
                    json!({"models":[{"name":VISION_MODELS[0].ollama_name}]}),
                ));
                replies.push(Reply::new(
                    "/api/chat",
                    200,
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
    let f = Fixture::new();
    f.image("1.png", 4, 4);
    f.write(json!([row(1, "PICKED")]));
    let (listener, endpoint) = common::silent();
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
        let mut invalid = with_blank_metadata(row(1, "SCHEDULED"));
        if invalid_field == "uuid" {
            invalid["uuid"] = json!("short");
        } else {
            invalid.as_object_mut().unwrap().shift_remove(invalid_field);
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
    assert_eq!(common::connections(&listener), 0);
}

#[tokio::test]
async fn accepted_provider_failure_leaves_the_row_unchanged() {
    let f = Fixture::new();
    f.write(json!([f.art_row(1, "SCHEDULED")]));
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let server = Server::start(provider_failures(1));
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().to_owned()));
    let result = app.generate_one(&id(1), request(Source::OpenRouter)).await;
    server.finish();
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OpenRouter HTTP 500"), "{error}");
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
    assert_eq!(f.read()[0]["title"], "Title");
    assert_eq!(f.read()[0]["description"], "Description");
    assert_eq!(f.read()[0]["state"], "SCHEDULED");
}

#[tokio::test]
async fn openrouter_batch_reverses_persisted_order_and_keeps_blank_field_on_failure() {
    let f = Fixture::new();
    let mut rows = vec![];
    for n in 1..=3 {
        let mut p = f.art_row(n, "SCHEDULED");
        p["description"] = json!("");
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
            .all(|p| p["description"] == "" && p["state"] == "SCHEDULED")
    );
}

#[tokio::test]
async fn batch_keeps_first_successful_safe_model_as_preference() {
    let f = Fixture::new();
    let rows: Vec<_> = (1..=3).map(|n| f.marked_row(n)).collect();
    f.write(json!(rows));
    let catalog = json!({"data":[
        {"id":"vendor/model-b:free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"}},
        {"id":"google/model-c:free","architecture":{"input_modalities":["text","image"]},"pricing":{"prompt":"0","completion":"0"}}
    ]});
    let failed = || {
        Reply::new(
            "/chat/completions",
            500,
            json!({"error":{"message":"temporary"}}),
        )
    };
    let server = Server::start(vec![
        failed(),
        Reply::new("/models", 200, catalog.clone()),
        completion("one"),
        failed(),
        completion("two"),
        completion("three"),
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

#[tokio::test]
async fn changed_artwork_bytes_make_no_provider_call_and_save_nothing() {
    let f = Fixture::new();
    f.write(json!([f.marked_row(1)]));
    f.colored("1.png", "blue");
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let server = Server::start(vec![completion("Generated")]);
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
        let server = Server::start(vec![completion("Generated")]);
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
    let malformed = Reply::new(
        "/chat/completions",
        200,
        json!({"choices":[{"message":{"content":"not a metadata object"}}]}),
    );
    for (reply, succeeded) in [(completion("Generated"), true), (malformed, false)] {
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
    let path = common::path_with(bin.path());
    let child = std::panic::catch_unwind(|| {
        run_child_test(
            "stalled_image_preparation_fails_within_the_generation_deadline",
            bin.path(),
            &[
                ("PATH", path.as_os_str()),
                (STALLED_TOOL_DIR, bin.path().as_os_str()),
            ],
        )
    });
    let pids = std::fs::read_to_string(bin.path().join("pids")).unwrap_or_default();
    let lingering: Vec<i32> = pids
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .filter(|&pid| running(pid))
        .collect();
    for &pid in &lingering {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(
        child.is_ok(),
        "stalled-preparation child failed or outlived its watchdog: preparation is unbounded"
    );
    assert!(
        lingering.is_empty(),
        "image tools outlived generation: {lingering:?}"
    );
}

async fn stalled_preparation_child(bin: &std::path::Path) {
    let f = Fixture::new();
    f.write(json!([f.art_row(1, "SCHEDULED")]));
    let server = Server::start(vec![completion("Generated")]);
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
        eventually(Duration::from_secs(5), || !running(
            descendant.parse().unwrap()
        )),
        "image tool descendant outlived the deadline"
    );
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
    f.write(json!([f.marked_row(1), f.marked_row(2)]));
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
        f.marked_row(1),
        f.marked_row(2),
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
    f.write(json!([f.marked_row(1)]));
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
    let mut incomplete = f.marked_row(1);
    incomplete
        .as_object_mut()
        .unwrap()
        .shift_remove("submissionTimestamp");
    for source in [unscheduled, row(1, "SCHEDULED"), incomplete] {
        f.write(json!([source]));
        let before = std::fs::read(&f.store.paths().json).unwrap();
        assert!(app.generate_one(&id(1), patient_local()).await.is_err());
        assert!(!entered.exists(), "llmfit ran for invalid source data");
        assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
    }
}

#[tokio::test]
async fn cooldown_expiry_uses_the_active_deadline_and_the_next_record_gets_a_new_one() {
    use common::http::{manual_clock, received, wait_for};
    use publicator::application::BatchEvent;
    use tokio::time::{Instant, advance};
    let f = Fixture::new();
    let (mut rows, _, _) = f.namesake_rows("SCHEDULED");
    rows = rows.into_iter().map(with_blank_metadata).collect();
    f.write(json!(rows));
    let (limited, release) = Reply::new("/chat/completions", 429, "busy")
        .with_header("Retry-After", "6")
        .hold_headers();
    let (limited, observed) = limited.confirm_skipped_body();
    let server = Server::start(vec![limited, completion("Queued record")]);
    let app = Application::new(Arc::new(f.store.clone()), client(server.url().into()));
    let mut request = request(Source::OpenRouter);
    request.timeout = Duration::from_secs(6);
    let (events, mut progress) = tokio::sync::mpsc::unbounded_channel();
    let _clock = manual_clock();
    let begun = Instant::now();
    let batch =
        app.generate_marked_with_events(&request, None, move |event| events.send(event).unwrap());
    let check = async {
        wait_for(|| server.requests().len() == 1).await;
        assert_eq!(
            progress.try_recv().unwrap(),
            BatchEvent::Admitted(vec![id(2), id(1)])
        );
        assert_eq!(progress.try_recv().unwrap(), BatchEvent::Started(id(2)));
        advance(Duration::from_secs(3)).await;
        release.send(()).unwrap();
        received(&observed).await;
        advance(Duration::from_millis(3001)).await;
        let Some(BatchEvent::Finished(active)) = progress.recv().await else {
            panic!("active record did not finish")
        };
        assert_eq!(active.uuid, id(2));
        assert!(!active.saved);
        assert_eq!(
            active.error.as_deref(),
            Some("provider error: OpenRouter deadline exceeded")
        );
        assert_eq!(Instant::now() - begun, Duration::from_millis(6001));
        assert_eq!(progress.recv().await, Some(BatchEvent::Started(id(1))));
        assert_eq!(server.requests().len(), 1);
        assert_eq!(f.read(), json!(rows));
        advance(Duration::from_secs(3)).await;
    };
    let (report, ()) = tokio::join!(batch, check);
    let report = report.unwrap();
    assert_eq!(
        (report.succeeded, report.failed, report.results.len()),
        (1, 1, 2)
    );
    assert_eq!(f.read()[0]["title"], "Queued record");
    assert_eq!(f.read()[1], rows[1]);
    let calls = server.requests();
    assert_eq!(calls.len(), 2);
    assert_ne!(
        calls[0]["body"], calls[1]["body"],
        "the second call is not a retry of the first image"
    );
    server.finish();
}

#[tokio::test]
async fn abort_during_cooldown_releases_batch_but_keeps_other_callers_gated() {
    use common::http::{manual_clock, received};
    let f = Fixture::new();
    f.write(json!([
        f.marked_row(1),
        f.marked_row(2),
        f.art_row(3, "SCHEDULED")
    ]));
    let original = f.read();
    let (limited, observed) = Reply::new("/chat/completions", 429, "busy")
        .with_header("Retry-After", "5")
        .confirm_skipped_body();
    let server = Server::start(vec![limited, completion("Unrelated record")]);
    let app = Arc::new(Application::new(
        Arc::new(f.store.clone()),
        client(server.url().into()),
    ));
    let _clock = manual_clock();
    let patient = GenerationRequest {
        timeout: Duration::from_secs(15),
        ..request(Source::OpenRouter)
    };
    let batch = {
        let app = Arc::clone(&app);
        tokio::spawn(async move { app.generate_marked(&patient).await })
    };
    received(&observed).await;
    assert!(app.mutate(Some(&id(2)), |_| Ok(())).is_err());
    batch.abort();
    assert!(batch.await.unwrap_err().is_cancelled());
    for row in [id(1), id(2)] {
        app.mutate(Some(&row), |_| Ok(()))
            .expect("abort releases both reservations");
    }
    // Let blocking image tools run against real time for this short deadline probe.
    tokio::time::resume();
    let other = app.generate_one(&id(3), request(Source::OpenRouter)).await;
    tokio::time::pause();
    assert_eq!(
        other.unwrap_err().to_string(),
        "provider error: OpenRouter deadline exceeded"
    );
    assert_eq!(
        server.requests().len(),
        1,
        "abort did not clear the cooldown"
    );
    let long = GenerationRequest {
        timeout: Duration::from_secs(10),
        ..request(Source::OpenRouter)
    };
    let unrelated = id(3);
    let (result, ()) = tokio::join!(app.generate_one(&unrelated, long), async {
        tokio::time::advance(Duration::from_millis(3001)).await;
    });
    assert_eq!(result.unwrap().metadata.title, "Unrelated record");
    let saved = f.read();
    assert_eq!((&saved[0], &saved[1]), (&original[0], &original[1]));
    assert_eq!(saved[2]["title"], "Unrelated record");
    server.finish();
}
