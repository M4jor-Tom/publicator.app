use super::{App, BatchProgress, GenerationState, Message, style};
use crate::{
    metadata::{Source, VISION_MODELS},
    model::{GENERATE_MARKER, Phase, phase},
};
use iced::{
    Element, Length, Task,
    widget::{button, column, container, row, text, text_input},
};
use serde_json::Value;

/// Publication thumbnails for the on-screen placeholders of the Schedule, Describe and Publish
/// lists, which report row UUIDs (Pick's report candidate paths and match no row).
pub(super) fn preview_tasks(state: &mut App) -> Task<Message> {
    let Some(rows) = state.publications.as_ref() else {
        return Task::none();
    };
    let capacity = 4usize.saturating_sub(
        state
            .row_previews
            .values()
            .filter(|result| result.is_none())
            .count(),
    );
    let records: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            let uuid = row.get("uuid")?.as_str()?;
            if !state.shown_previews.contains(uuid) || state.row_previews.contains_key(uuid) {
                return None;
            }
            Some((uuid.to_owned(), row.clone()))
        })
        .take(capacity)
        .collect();
    let root = state.application.store().paths().data_dir.clone();
    let cache = root.join(".thumbs");
    let mut tasks = Vec::new();
    for (uuid, row) in records {
        state.row_previews.insert(uuid.clone(), None);
        let task_uuid = uuid.clone();
        let root = root.clone();
        let cache = cache.clone();
        let index = state.artwork_index.clone();
        tasks.push(Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let index = super::artwork_index(&index, &root)?;
                    crate::artwork::preview_source(index, &row)
                        .and_then(|(source, sha512)| {
                            let thumb = crate::images::ensure_thumb(&source, &cache, &sha512)?;
                            Ok((source, thumb))
                        })
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()))
            },
            move |result| Message::RowPreviewFinished {
                uuid: task_uuid,
                result,
            },
        ));
    }
    Task::batch(tasks)
}

pub fn view(state: &App) -> Element<'_, Message> {
    let mut controls = column![text("AI provider")].spacing(8);
    for source in crate::metadata::available_sources() {
        let label = match source {
            Source::OpenRouter => "OpenRouter",
            Source::Local => "Local Ollama",
        };
        controls = controls.push(
            style::wide(label)
                .style(style::tab(state.source == *source))
                .on_press(Message::SelectSource(*source)),
        );
    }
    controls = controls.push(text("Model"));
    for model in VISION_MODELS.iter() {
        controls = controls.push(
            style::wide(model.ollama_name.clone())
                .style(style::tab(state.model == *model))
                .on_press(Message::SelectModel(model.clone())),
        );
    }
    controls = controls
        .push(text(format!(
            "Selected: {:?} / {}",
            state.source, state.model.ollama_name
        )))
        .push(style::wide("Generate marked").on_press(Message::GenerateMarked));
    if let Some(summary) = batch_summary(state) {
        controls = controls.push(text(summary));
    }
    if state.source == Source::Local {
        controls = controls.push(local_fit_view(state));
    }
    let mut cards = vec![controls.into()];
    for row in ordered_rows(
        state.publications.as_deref().unwrap_or_default(),
        state.revealed_publication(),
    ) {
        let Some(uuid) = row.get("uuid").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        cards.push(metadata_card(state, row, uuid));
    }
    let mut content = column![]
        .push(super::floating(
            super::tab_scrollable(super::Tab::Describe, style::cards(cards)),
            super::schedule::delete_banner(state),
        ))
        .spacing(12);
    if state.local_install_consent_pending() {
        content = content.push(
            container(
                row![
                    text(format!("Install {} and generate?", state.model.ollama_name))
                        .width(Length::Fill),
                    button("Confirm install and generate").on_press(Message::ConfirmLocalInstall),
                    button("Cancel").on_press(Message::CancelLocalInstall),
                ]
                .spacing(8),
            )
            .padding(8)
            .width(Length::Fill)
            .style(style::notice),
        );
    }
    content.into()
}

/// The summary line under "Generate marked".
pub fn batch_summary(state: &App) -> Option<String> {
    Some(match state.batch.as_ref()? {
        BatchProgress::Running {
            ids, active: None, ..
        } => format!("Preparing {}…", ids.len()),
        BatchProgress::Running {
            ids,
            active: Some(active),
            done,
            failed,
        } => format!(
            "Generating {} of {} · {done} done · {failed} failed",
            active + 1,
            ids.len()
        ),
        BatchProgress::Finished { succeeded, failed } => {
            let mut summary = format!("Generated {succeeded} of {}", succeeded + failed);
            if *failed > 0 {
                summary.push_str(&format!(" · {failed} failed"));
            }
            summary
        }
        BatchProgress::Failed(error) => format!("Generation failed: {error}"),
    })
}

fn local_fit_view(state: &App) -> Element<'_, Message> {
    let Some(status) = state.local_fit_status() else {
        return text("Checking local model fit…").into();
    };
    let reason: String = status.fit.reason.chars().take(512).collect();
    let availability = if !status.fit.fit {
        "This model does not fit this host."
    } else if !status.available {
        "Ollama is unavailable; generation cannot continue."
    } else if status.installed {
        "Model is installed."
    } else {
        "Model is not installed; installation requires confirmation."
    };
    text(format!(
        "Local fit: {} — {}. {}",
        status.fit.level,
        if reason.is_empty() {
            "No additional fit notes"
        } else {
            &reason
        },
        availability
    ))
    .into()
}

/// A card's one generation state line; `Generating…` alone is boxed, so the active card
/// stands out.
fn generation_line<'a>(state: &App, uuid: &str, marked: bool) -> Option<Element<'a, Message>> {
    Some(match state.generation_state(uuid) {
        Some(GenerationState::Queued) => text("Queued").color(style::MUTED).into(),
        Some(GenerationState::Generating) => container(text("Generating…"))
            .padding([2, 8])
            .style(style::active)
            .into(),
        Some(GenerationState::Generated) => text("Generated").color(style::TODAY).into(),
        Some(GenerationState::Failed(error)) => {
            text(format!("Failed: {error}")).color(style::ERROR).into()
        }
        None if marked => style::warning("Marked for generation"),
        None => return None,
    })
}

fn metadata_card<'a>(state: &'a App, row: &Value, uuid: String) -> Element<'a, Message> {
    let saved = crate::workflow::Metadata::of(row);
    let (title, description) = state
        .drafts
        .get(&uuid)
        .map(|draft| (draft.title().to_owned(), draft.description().to_owned()))
        .unwrap_or((saved.title, saved.description));
    let title_uuid = uuid.clone();
    let mut card = column![
        state.row_art(&uuid, 240.0),
        text(super::schedule::date_label(state, row)),
    ]
    .spacing(8);
    let marked = title.contains(GENERATE_MARKER) || description.contains(GENERATE_MARKER);
    if let Some(line) = generation_line(state, &uuid, marked) {
        card = card.push(line);
    }
    if state.revealed_publication() == Some(uuid.as_str()) {
        card = card.push(style::warning("Opened from Calendar"));
    }
    card = card.push(text("Title")).push(
        text_input("Title", &title)
            .on_input(move |value| Message::Edited {
                uuid: title_uuid.clone(),
                field: super::drafts::Field::Title,
                text: value,
            })
            .on_submit(Message::SaveDraft(uuid.clone()))
            .style(style::input),
    );
    card = card.push(text("Description"));
    if let Some(editor) = state.description_editors.get(&uuid)
        && state.viewer.is_some()
    {
        // tiny-skia paints an editor's overflowing last line unclipped, over the viewer; a
        // clipped text of the same height keeps the layout and scroll.
        card = card.push(
            container(text(editor.text()))
                .height(Length::Fixed(120.0))
                .clip(true),
        );
    } else if let Some(editor) = state.description_editors.get(&uuid) {
        let description_uuid = uuid.clone();
        card = card.push(
            iced::widget::text_editor(editor)
                .placeholder("Description")
                .height(Length::Fixed(120.0))
                .on_action(move |action| Message::DescriptionAction {
                    uuid: description_uuid.clone(),
                    action,
                })
                .style(style::editor),
        );
    } else {
        card = card.push(text(description.clone()));
    }
    let dirty = |field| {
        state
            .drafts
            .get(&uuid)
            .is_some_and(|d| d.is_field_dirty(field))
    };
    let saved = |field| if dirty(field) { "unsaved" } else { "saved" };
    let status = format!(
        "Title: {}; description: {}",
        saved(super::drafts::Field::Title),
        saved(super::drafts::Field::Description)
    );
    style::card(
        card.push(style::wide("Generate one").on_press(Message::GenerateOne(uuid.clone())))
            .push(style::wide("Approve metadata").on_press(Message::ApproveMetadata(uuid.clone())))
            .push(
                style::wide("Return to Schedule").on_press(Message::ReturnToSchedule(uuid.clone())),
            )
            .push(style::wide("Save draft").on_press(Message::SaveDraft(uuid.clone())))
            .push(super::schedule::delete_button(uuid))
            .push(style::warning(status)),
    )
    .into()
}

pub(super) fn ordered_rows<'a>(rows: &'a [Value], target: Option<&str>) -> Vec<&'a Value> {
    super::revealed_rows(
        rows.iter()
            .filter(|row| phase(row).ok() == Some(Phase::Scheduled)),
        target,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revealed_scheduled_publication_is_first_without_reordering_saved_rows() {
        let rows = vec![
            serde_json::json!({"uuid":"first", "state":"SCHEDULED"}),
            serde_json::json!({"uuid":"target", "state":"SCHEDULED"}),
        ];
        let ordered = ordered_rows(&rows, Some("target"));
        assert_eq!(ordered[0]["uuid"], "target");
        assert_eq!(rows[0]["uuid"], "first");
    }
}
