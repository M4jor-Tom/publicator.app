use super::{App, BatchProgress, GenerationState, Message, style};
use crate::{
    metadata::{Source, VISION_MODELS},
    model::{Phase, metadata_blank, phase},
};
use iced::{
    Element, Length, Rectangle, Renderer, Size, Task, Theme,
    advanced::{Layout, Widget, layout, mouse, renderer, text::paragraph, widget::Tree},
    widget::{Column, button, column, container, row, text, text_editor, text_input},
};
use serde_json::Value;

const ROW_ART_HEIGHT: f32 = 108.0;
const ROWS_MIN_WINDOW_WIDTH: f32 = 760.0;

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

/// The provider, model and layout choices, Generate marked and its summary.
fn controls(state: &App) -> Column<'_, Message> {
    let compact = state.describe_layout == DescribeLayout::Rows;
    let choice = |label: String, selected, message| {
        let button = if compact {
            button(text(label)).padding([5, 10])
        } else {
            style::wide(label)
        };
        button.style(style::tab(selected)).on_press(message)
    };
    let providers = crate::metadata::available_sources().iter().map(|&source| {
        let label = match source {
            Source::OpenRouter => "OpenRouter",
            Source::Local => "Local Ollama",
        };
        choice(
            label.into(),
            state.source == source,
            Message::SelectSource(source),
        )
        .into()
    });
    let models = VISION_MODELS.iter().map(|model| {
        choice(
            model.ollama_name.clone(),
            state.model == *model,
            Message::SelectModel(model.clone()),
        )
        .into()
    });
    let layouts = [
        (DescribeLayout::Grid, "Grid"),
        (DescribeLayout::Rows, "Rows"),
    ]
    .into_iter()
    .map(|(layout, label)| {
        choice(
            label.into(),
            state.describe_layout == layout,
            Message::SelectDescribeLayout(layout),
        )
        .into()
    });
    let mut controls = column![].spacing(8);
    if compact {
        fn group<'a>(
            label: &'static str,
            choices: Vec<Element<'a, Message>>,
        ) -> iced::widget::Row<'a, Message> {
            row![text(label)]
                .extend(choices)
                .spacing(6)
                .align_y(iced::Alignment::Center)
        }
        controls = controls.push(
            row![
                group("AI provider", providers.collect()),
                group("Model", models.collect()),
                group("Layout", layouts.collect()),
                button("Generate marked")
                    .padding([5, 10])
                    .on_press(Message::GenerateMarked)
            ]
            .spacing(16)
            .wrap(),
        );
    } else {
        controls = controls
            .push(text("AI provider"))
            .extend(providers)
            .push(text("Model"))
            .extend(models)
            .push(text("Layout"))
            .extend(layouts)
            .push(text(format!(
                "Selected: {:?} / {}",
                state.source, state.model.ollama_name
            )))
            .push(style::wide("Generate marked").on_press(Message::GenerateMarked));
    }
    if let Some(summary) = batch_summary(state) {
        controls = controls.push(text(summary));
    }
    if state.source == Source::Local {
        controls = controls.push(local_fit_view(state));
    }
    controls
}

pub fn view(state: &App) -> Element<'_, Message> {
    let mut cards = vec![controls(state).into()];
    cards.extend(publication_cards(state));
    let list = match state.describe_layout {
        DescribeLayout::Grid => style::cards(cards),
        DescribeLayout::Rows => style::rows(cards),
    };
    let list = super::tab_scrollable(super::Tab::Describe, list);
    let list = if state.describe_layout == DescribeLayout::Rows {
        list.spacing(4)
    } else {
        list
    };
    let mut content =
        column![super::floating(list, super::schedule::delete_banner(state))].spacing(12);
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

/// The listed publications' cards, the current batch's first.
fn publication_cards(state: &App) -> Vec<Element<'_, Message>> {
    let generated = |row: &Value| {
        row.get("uuid")
            .and_then(Value::as_str)
            .and_then(|uuid| state.generation_state(uuid))
            == Some(&GenerationState::Generated)
    };
    let mut cards = Vec::new();
    for row in ordered_rows(
        state.publications.as_deref().unwrap_or_default(),
        state.batch_ids(),
        state.revealed_publication(),
        generated,
    ) {
        let Some(uuid) = row.get("uuid").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        cards.push(if phase(row).ok() == Some(Phase::Scheduled) {
            metadata_card(state, row, uuid)
        } else {
            generated_card(state, row, uuid)
        });
    }
    cards
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
        BatchProgress::Finished {
            succeeded, failed, ..
        } => {
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
        Some(GenerationState::Failed(error)) => text(format!("Failed: {error}"))
            .color(style::ERROR)
            .wrapping(text::Wrapping::WordOrGlyph)
            .into(),
        None if marked => style::warning("Marked for generation"),
        None => return None,
    })
}

fn layout_card<'a>(
    state: &'a App,
    uuid: &str,
    details: Element<'a, Message>,
) -> Element<'a, Message> {
    if state.describe_layout == DescribeLayout::Grid {
        return style::card(column![state.row_art(uuid, 240.0), details].spacing(8)).into();
    }
    let content: Element<'a, Message> = if state.window_width >= ROWS_MIN_WINDOW_WIDTH {
        row![
            container(state.row_art(uuid, ROW_ART_HEIGHT)).width(164),
            details
        ]
        .spacing(12)
        .into()
    } else {
        column![state.row_art(uuid, ROW_ART_HEIGHT), details]
            .spacing(6)
            .into()
    };
    style::card(content).padding(8).into()
}

/// A card whose generation moved it on to DESCRIBED: read-only, it stays listed until Refresh
/// so its `Generated` state remains in view; its actions belong to Publish now.
fn generated_card<'a>(state: &'a App, row: &Value, uuid: String) -> Element<'a, Message> {
    let saved = crate::workflow::Metadata::of(row);
    let mut details = column![text(super::schedule::date_label(state, row))].spacing(8);
    if let Some(line) = generation_line(state, &uuid, false) {
        details = details.push(line);
    }
    let details = details
        .push(text("Title"))
        .push(text(saved.title))
        .push(text("Description"))
        .push(text(saved.description));
    let status = text("Described; listed here until Refresh").color(style::MUTED);
    layout_card(state, &uuid, details.push(status).into())
}

fn metadata_card<'a>(state: &'a App, row: &'a Value, uuid: String) -> Element<'a, Message> {
    let compact = state.describe_layout == DescribeLayout::Rows;
    let draft = state.drafts.get(&uuid);
    let title = draft
        .map(super::Draft::title)
        .or_else(|| row.get("title")?.as_str())
        .unwrap_or("");
    let description = draft
        .map(super::Draft::description)
        .or_else(|| row.get("description")?.as_str())
        .unwrap_or("");
    let title_uuid = uuid.clone();
    let save_uuid = uuid.clone();
    let title_input = text_input("Title", title)
        .on_input(move |value| Message::Edited {
            uuid: title_uuid.clone(),
            field: super::drafts::Field::Title,
            text: value,
        })
        .on_submit(Message::SaveDraft(save_uuid))
        .style(style::input);
    let saved = |field| {
        if draft.is_some_and(|d| d.is_field_dirty(field)) {
            "unsaved"
        } else {
            "saved"
        }
    };
    let status = style::warning(format!(
        "Title: {}; description: {}",
        saved(super::drafts::Field::Title),
        saved(super::drafts::Field::Description)
    ));
    let progress = generation_line(state, &uuid, metadata_blank(title, description));
    let date = text(super::schedule::date_label(state, row));
    let revealed = state.revealed_publication() == Some(uuid.as_str());
    let actions = [
        ("Generate one", Message::GenerateOne(uuid.clone())),
        ("Approve metadata", Message::ApproveMetadata(uuid.clone())),
        (
            "Return to Schedule",
            Message::ReturnToSchedule(uuid.clone()),
        ),
        ("Save draft", Message::SaveDraft(uuid.clone())),
        ("Delete…", Message::AskDeletePublication(uuid.clone())),
    ];
    let mut details;
    if compact {
        details = column![row![date, status].push(progress).spacing(12).wrap()]
            .spacing(4)
            .width(Length::Fill);
        if revealed {
            details = details.push(style::warning("Opened from Calendar"));
        }
        details = details.push(
            row![text("Title"), title_input]
                .spacing(8)
                .align_y(iced::Alignment::Center),
        );
        let expanded = state.expanded_description.as_ref() == Some(&uuid);
        let edit = button(if expanded {
            "Done editing"
        } else {
            "Edit description"
        })
        .on_press(Message::ToggleDescriptionEditor(uuid.clone()));
        details = if expanded {
            details
                .push(
                    row![text("Description"), edit]
                        .spacing(8)
                        .align_y(iced::Alignment::Center),
                )
                .push(description_editor(state, &uuid, description))
        } else {
            details.push(
                row![Element::new(DescriptionPreview(description)), edit]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
            )
        };
        details = details.push(
            iced::widget::Row::with_children(
                actions
                    .map(|(label, action)| button(label).padding([5, 10]).on_press(action).into()),
            )
            .spacing(6)
            .wrap(),
        );
    } else {
        details = column![date].spacing(8).push(progress);
        if revealed {
            details = details.push(style::warning("Opened from Calendar"));
        }
        details = details
            .push(text("Title"))
            .push(title_input)
            .push(text("Description"))
            .push(description_editor(state, &uuid, description))
            .push(
                Column::with_children(
                    actions.map(|(label, action)| style::wide(label).on_press(action).into()),
                )
                .spacing(8),
            )
            .push(status);
    }
    layout_card(state, &uuid, details.into())
}

fn description_editor<'a>(
    state: &'a App,
    uuid: &str,
    description: &'a str,
) -> Element<'a, Message> {
    // tiny-skia can paint an editor's last line over the viewer; keep its height without the editor.
    if state.viewer.is_some() {
        return container(text(description)).height(120).clip(true).into();
    }
    let Some(editor) = state.description_editors.get(uuid) else {
        return text(description).into();
    };
    let uuid = uuid.to_owned();
    text_editor(editor)
        .placeholder("Description")
        .height(120)
        .on_action(move |action| Message::DescriptionAction {
            uuid: uuid.clone(),
            action,
        })
        .style(style::editor)
        .into()
}

/// A two-line resting preview. Measure with the same font and wrapping that we draw with;
/// counting characters or source lines cannot tell whether a particular window clips the text.
struct DescriptionPreview<'a>(&'a str);

#[derive(Default)]
struct PreviewText {
    full: paragraph::Plain<<Renderer as iced::advanced::text::Renderer>::Paragraph>,
    clipped: paragraph::Plain<<Renderer as iced::advanced::text::Renderer>::Paragraph>,
    cue: paragraph::Plain<<Renderer as iced::advanced::text::Renderer>::Paragraph>,
    overflow: bool,
}

impl Widget<Message, Theme, Renderer> for DescriptionPreview<'_> {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<PreviewText>()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(PreviewText::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(40.0))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = self.size();
        let bounds = limits.resolve(size.width, size.height, Size::ZERO);
        let state = tree.state.downcast_mut::<PreviewText>();
        let format = || text::Format {
            line_height: text::LineHeight::Absolute(20.0.into()),
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::WordOrGlyph,
            ..text::Format::default()
        };
        let measure = |width| layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY));
        let body = if self.0.trim().is_empty() {
            "No description"
        } else {
            self.0
        };
        text::layout(
            &mut state.full,
            renderer,
            &measure(bounds.width),
            body,
            format(),
        );
        state.overflow = state.full.min_height() > bounds.height;
        if state.overflow {
            text::layout(
                &mut state.cue,
                renderer,
                &measure(bounds.width),
                "…",
                format(),
            );
            let body_width = (bounds.width - state.cue.min_width() - 6.0).max(0.0);
            text::layout(
                &mut state.clipped,
                renderer,
                &measure(body_width),
                body,
                format(),
            );
        }
        layout::Node::new(bounds)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<PreviewText>();
        let body = if state.overflow {
            &state.clipped
        } else {
            &state.full
        };
        text::draw(
            renderer,
            style,
            bounds,
            body.raw(),
            text::Style::default(),
            &clip,
        );
        if state.overflow {
            let cue_bounds = Rectangle {
                x: bounds.x + bounds.width - state.cue.min_width(),
                y: bounds.y + bounds.height - state.cue.min_height(),
                width: state.cue.min_width(),
                height: state.cue.min_height(),
            };
            text::draw(
                renderer,
                style,
                cue_bounds,
                state.cue.raw(),
                text::Style {
                    color: Some(style::ACCENT),
                },
                &clip,
            );
        }
    }
}

/// SCHEDULED rows, plus those `kept` (cards whose generation just described them); the `batch`
/// ones first, in its order, the others in store order.
pub(super) fn ordered_rows<'a>(
    rows: &'a [Value],
    batch: &[String],
    target: Option<&str>,
    kept: impl Fn(&Value) -> bool,
) -> Vec<&'a Value> {
    let mut listed: Vec<_> = rows
        .iter()
        .filter(|row| phase(row).ok() == Some(Phase::Scheduled) || kept(row))
        .collect();
    // A stable sort: rows outside the batch keep their store order.
    listed.sort_by_key(|row| {
        row.get("uuid")
            .and_then(Value::as_str)
            .and_then(|uuid| batch.iter().position(|id| id == uuid))
            .unwrap_or(usize::MAX)
    });
    super::revealed_rows(listed.into_iter(), target)
}

/// The Describe card arrangement, kept in memory for the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DescribeLayout {
    /// Cards in a responsive grid.
    Grid,
    /// One full-width card per row.
    #[default]
    Rows,
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
        let ordered = ordered_rows(&rows, &[], Some("target"), |_| false);
        assert_eq!(ordered[0]["uuid"], "target");
        assert_eq!(rows[0]["uuid"], "first");
    }

    #[test]
    fn only_kept_described_rows_join_the_scheduled_ones() {
        let rows = vec![
            serde_json::json!({"uuid":"scheduled", "state":"SCHEDULED"}),
            serde_json::json!({"uuid":"generated", "state":"DESCRIBED"}),
            serde_json::json!({"uuid":"other", "state":"DESCRIBED"}),
        ];
        let ordered = ordered_rows(&rows, &[], None, |row| row["uuid"] == "generated");
        let uuids: Vec<_> = ordered.iter().map(|row| row["uuid"].clone()).collect();
        assert_eq!(uuids, ["scheduled", "generated"]);
    }

    fn uuids(ordered: &[&Value]) -> Vec<String> {
        ordered
            .iter()
            .map(|row| row["uuid"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn batch_rows_come_first_in_batch_order_and_the_rest_keep_store_order() {
        let rows: Vec<_> = ["a", "b", "c", "d", "e"]
            .map(|uuid| serde_json::json!({"uuid": uuid, "state": "SCHEDULED"}))
            .into();
        let batch = ["d".to_owned(), "b".to_owned()];
        assert_eq!(
            uuids(&ordered_rows(&rows, &batch, None, |_| false)),
            ["d", "b", "a", "c", "e"]
        );
        // The Calendar-revealed row still swaps to the top.
        assert_eq!(
            uuids(&ordered_rows(&rows, &batch, Some("e"), |_| false)),
            ["e", "b", "a", "c", "d"]
        );
    }
}
