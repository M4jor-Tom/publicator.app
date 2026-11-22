mod common;
use common::{Fixture, id, row};
use publicator::{
    model::{Phase, phase},
    store::Store,
    workflow::{
        Metadata, MetadataPatch, approve_metadata, merge_generated_blanks, return_to_metadata,
        save_generated_pair, save_metadata,
    },
};
use serde_json::json;
use std::sync::{Arc, Barrier};

/// A batch merge of `pair` from the row as stored now, as a batch record starts.
fn merge(store: &Store, pair: &Metadata) -> publicator::error::Result<()> {
    let started = Metadata::of(&store.read().unwrap()[0]);
    merge_generated_blanks(store, &id(1), &started, pair)
}

#[test]
fn metadata_draft_does_not_approve_until_explicit_approval() {
    let f = Fixture::new();
    f.write(json!([row(1, "SCHEDULED")]));
    save_metadata(
        &f.store,
        &id(1),
        &MetadataPatch {
            title: Some("Draft".into()),
            description: Some("Details".into()),
        },
    )
    .unwrap();
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["state"], "SCHEDULED");
    assert_eq!(saved[0]["title"], "Draft");
    approve_metadata(&f.store, &id(1)).unwrap();
    assert_eq!(
        phase(&f.store.read().unwrap()[0]).unwrap(),
        Phase::Described
    );
}

#[test]
fn generated_pair_advances_only_with_complete_valid_metadata() {
    let f = Fixture::new();
    f.write(json!([row(1, "SCHEDULED")]));
    let pair = Metadata {
        title: "A title".into(),
        description: "A description".into(),
    };
    save_generated_pair(&f.store, &id(1), &row_pair(), &pair).unwrap();
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "A title");
    assert_eq!(phase(&saved[0]).unwrap(), Phase::Described);
}

#[test]
fn blank_metadata_blocks_approval() {
    for (title, description) in [("", "Description"), ("Title", " \n ")] {
        let f = Fixture::new();
        let mut publication = row(1, "SCHEDULED");
        publication["title"] = json!(title);
        publication["description"] = json!(description);
        f.write(json!([publication]));
        assert!(approve_metadata(&f.store, &id(1)).is_err());
        assert_eq!(f.store.read().unwrap()[0]["state"], "SCHEDULED");
    }
}

#[test]
fn title_limit_counts_unicode_characters_not_utf8_bytes() {
    let f = Fixture::new();
    let mut publication = row(1, "SCHEDULED");
    publication["title"] = json!("é".repeat(50));
    publication["description"] = json!("Description");
    f.write(json!([publication]));
    approve_metadata(&f.store, &id(1)).unwrap();

    let f = Fixture::new();
    let mut publication = row(1, "SCHEDULED");
    publication["title"] = json!("é".repeat(51));
    f.write(json!([publication]));
    assert!(approve_metadata(&f.store, &id(1)).is_err());
    assert_eq!(f.store.read().unwrap()[0]["state"], "SCHEDULED");
    let before = std::fs::read(&f.store.paths().json).unwrap();
    assert!(
        save_metadata(
            &f.store,
            &id(1),
            &MetadataPatch {
                title: Some("é".repeat(51)),
                description: None,
            },
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
}

#[test]
fn text_embedding_the_old_marker_is_ordinary_and_can_be_approved() {
    let f = Fixture::new();
    f.write(json!([row(1, "SCHEDULED")]));
    save_metadata(
        &f.store,
        &id(1),
        &MetadataPatch {
            title: Some("[[PUBLICATOR:TODO_GENERATE]] embedded".into()),
            description: None,
        },
    )
    .unwrap();
    let saved = &f.store.read().unwrap()[0];
    assert_eq!(saved["title"], "[[PUBLICATOR:TODO_GENERATE]] embedded");
    assert_eq!(saved["state"], "SCHEDULED");
    approve_metadata(&f.store, &id(1)).unwrap();
    assert_eq!(f.read()[0]["state"], "DESCRIBED");
}

#[test]
fn batch_fills_only_the_blank_field_of_a_partial_draft_and_approves_it() {
    let f = Fixture::new();
    let mut p = row(1, "SCHEDULED");
    p["title"] = "Manual".into();
    p["description"] = "".into();
    f.write(json!([p]));
    merge(
        &f.store,
        &Metadata {
            title: "AI".into(),
            description: "Story".into(),
        },
    )
    .unwrap();
    assert_eq!(f.read()[0]["title"], "Manual");
    assert_eq!(f.read()[0]["description"], "Story");
    assert_eq!(f.read()[0]["state"], "DESCRIBED");
}

#[test]
fn generated_title_preserves_a_manual_description() {
    let f = Fixture::new();
    let mut publication = row(1, "SCHEDULED");
    publication["title"] = json!("");
    publication["description"] = json!("Manual description");
    f.write(json!([publication]));
    merge(
        &f.store,
        &Metadata {
            title: "Generated".into(),
            description: "Ignored stale description".into(),
        },
    )
    .unwrap();
    let saved = f.read();
    assert_eq!(saved[0]["title"], "Generated");
    assert_eq!(saved[0]["description"], "Manual description");
    assert_eq!(saved[0]["state"], "DESCRIBED");
}

#[test]
fn batch_merge_skips_a_complete_manual_draft() {
    let f = Fixture::new();
    let mut publication = row(1, "SCHEDULED");
    publication["title"] = json!("Manual title");
    publication["description"] = json!("Manual description");
    f.write(json!([publication]));
    merge(
        &f.store,
        &Metadata {
            title: "Stale AI title".into(),
            description: "Stale AI description".into(),
        },
    )
    .unwrap();
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "Manual title");
    assert_eq!(saved[0]["description"], "Manual description");
    assert_eq!(saved[0]["state"], "SCHEDULED");
}

#[test]
fn merging_both_blank_fields_with_valid_pair_advances_to_described() {
    // A whitespace-only field counts as blank.
    for title in ["", " \n "] {
        let f = Fixture::new();
        let mut publication = common::with_blank_metadata(row(1, "SCHEDULED"));
        publication["title"] = json!(title);
        f.write(json!([publication]));
        merge(
            &f.store,
            &Metadata {
                title: "AI title".into(),
                description: "AI description".into(),
            },
        )
        .unwrap();
        let saved = f.store.read().unwrap();
        assert_eq!(saved[0]["title"], "AI title");
        assert_eq!(saved[0]["description"], "AI description");
        assert_eq!(saved[0]["state"], "DESCRIBED");
        let before_reapproval = std::fs::read(&f.store.paths().json).unwrap();
        assert!(approve_metadata(&f.store, &id(1)).is_err());
        assert_eq!(
            std::fs::read(&f.store.paths().json).unwrap(),
            before_reapproval
        );
    }
}

fn merge_after_manual_edit(pair: Metadata, patch: MetadataPatch) -> Fixture {
    let f = Fixture::new();
    f.write(json!([common::with_blank_metadata(row(1, "SCHEDULED"))]));
    let snapshot = Metadata::of(&f.read()[0]);
    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let worker = {
        let (store, started, release) =
            (f.store.clone(), Arc::clone(&started), Arc::clone(&release));
        std::thread::spawn(move || {
            started.wait();
            release.wait();
            merge_generated_blanks(&store, &id(1), &snapshot, &pair).unwrap();
        })
    };
    started.wait(); // Provider is blocked before returning its generated pair.
    save_metadata(&f.store, &id(1), &patch).unwrap();
    release.wait();
    worker.join().unwrap();
    f
}

#[test]
fn provider_result_merges_only_fields_still_blank_after_concurrent_edit() {
    let f = merge_after_manual_edit(
        Metadata {
            title: "AI title".into(),
            description: "AI description".into(),
        },
        MetadataPatch {
            title: Some("New manual title".into()),
            description: None,
        },
    );
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "New manual title");
    assert_eq!(saved[0]["description"], "AI description");
    assert_eq!(saved[0]["state"], "DESCRIBED");
}

#[test]
fn provider_result_preserves_a_newer_manual_description_and_generates_title() {
    let f = merge_after_manual_edit(
        Metadata {
            title: "AI title".into(),
            description: "Stale AI description".into(),
        },
        MetadataPatch {
            title: None,
            description: Some("Concurrent draft".into()),
        },
    );
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "AI title");
    assert_eq!(saved[0]["description"], "Concurrent draft");
    assert_eq!(saved[0]["state"], "DESCRIBED");
}

#[test]
fn blocked_stale_provider_result_does_not_approve_manual_edits() {
    let f = merge_after_manual_edit(
        Metadata {
            title: "Stale AI title".into(),
            description: "Stale AI description".into(),
        },
        MetadataPatch {
            title: Some("New manual title".into()),
            description: Some("New manual description".into()),
        },
    );
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "New manual title");
    assert_eq!(saved[0]["description"], "New manual description");
    assert_eq!(saved[0]["state"], "SCHEDULED");
}

#[test]
fn returning_to_metadata_and_editing_are_forbidden_after_delivery() {
    let f = Fixture::new();
    let mut publication = row(1, "DESCRIBED");
    publication["apparitions"] =
        json!([{"platformName":"old-platform","state":"published_or_scheduled"}]);
    f.write(json!([publication]));
    assert!(return_to_metadata(&f.store, &id(1)).is_err());
    assert!(
        save_metadata(
            &f.store,
            &id(1),
            &MetadataPatch {
                title: Some("Changed".into()),
                description: None
            }
        )
        .is_err()
    );
    assert_eq!(f.store.read().unwrap()[0]["title"], "Title");
}

#[test]
fn return_to_metadata_preserves_draft_for_correction_and_reapproval() {
    let f = Fixture::new();
    let mut publication = row(1, "DESCRIBED");
    publication["title"] = json!("Keep title");
    publication["description"] = json!("Keep description");
    f.write(json!([publication]));
    return_to_metadata(&f.store, &id(1)).unwrap();
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["state"], "SCHEDULED");
    assert_eq!(saved[0]["title"], "Keep title");
    assert_eq!(saved[0]["description"], "Keep description");
    approve_metadata(&f.store, &id(1)).unwrap();
    assert_eq!(f.store.read().unwrap()[0]["state"], "DESCRIBED");
}

#[test]
fn invalid_generated_pair_does_not_advance_or_partially_overwrite() {
    let f = Fixture::new();
    let mut publication = row(1, "SCHEDULED");
    publication["title"] = json!("Manual title");
    publication["description"] = json!("");
    f.write(json!([publication]));
    assert!(
        merge(
            &f.store,
            &Metadata {
                title: "AI".into(),
                description: "".into()
            }
        )
        .is_err()
    );
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "Manual title");
    assert_eq!(saved[0]["description"], "");
    assert_eq!(saved[0]["state"], "SCHEDULED");
}

#[test]
fn metadata_patch_preserves_omitted_field_and_requires_scheduled_state() {
    let f = Fixture::new();
    f.write(json!([row(1, "SCHEDULED")]));
    save_metadata(
        &f.store,
        &id(1),
        &MetadataPatch {
            title: Some("New title".into()),
            description: None,
        },
    )
    .unwrap();
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "New title");
    assert_eq!(saved[0]["description"], "Description");

    let f = Fixture::new();
    f.write(json!([row(1, "PICKED")]));
    assert!(
        save_metadata(
            &f.store,
            &id(1),
            &MetadataPatch {
                title: Some("Not allowed".into()),
                description: None
            }
        )
        .is_err()
    );
}

/// The draft `row()` admits a single generation with.
fn row_pair() -> Metadata {
    Metadata {
        title: "Title".into(),
        description: "Description".into(),
    }
}

#[test]
fn single_completion_overwrites_only_fields_unchanged_since_admission() {
    let generated = Metadata {
        title: "AI title".into(),
        description: "AI description".into(),
    };
    let f = Fixture::new();
    f.write(json!([row(1, "SCHEDULED")]));
    let newer = MetadataPatch {
        title: Some("Newer title".into()),
        description: None,
    };
    save_metadata(&f.store, &id(1), &newer).unwrap();
    save_generated_pair(&f.store, &id(1), &row_pair(), &generated).unwrap();
    let saved = f.store.read().unwrap();
    assert_eq!(saved[0]["title"], "Newer title");
    assert_eq!(saved[0]["description"], "AI description");
    assert_eq!(
        saved[0]["state"], "SCHEDULED",
        "a newer edit is never approved"
    );
}
