use super::{Message, style};
use crate::prompts::{Lineage, PromptMatch};
use iced::{
    Alignment, Element, Length,
    widget::{button, column, container, row, text},
};

/// The whole prompt panel, always open (Calendar: the event chip already collapses it).
pub fn view(match_: &PromptMatch) -> Element<'_, Message> {
    panel(match_, None)
}

/// The prompt panel with only its heading shown until `toggle` opens it (Pick cards).
pub fn collapsible(match_: &PromptMatch, open: bool, toggle: Message) -> Element<'_, Message> {
    panel(match_, Some((open, toggle)))
}

fn panel(match_: &PromptMatch, toggle: Option<(bool, Message)>) -> Element<'_, Message> {
    let mut lines = plain_text(match_).into_iter();
    let heading = text(lines.next().unwrap_or_default()).size(13);
    if let PromptMatch::Unknown(_) = match_ {
        return heading.color(style::MUTED).into();
    }
    let open = toggle.as_ref().is_none_or(|(open, _)| *open);
    let heading: Element<'_, Message> = match toggle {
        Some((open, toggle)) => row![
            heading.width(Length::Fill),
            button(text(if open { "Hide" } else { "Show" }).size(12))
                .padding([2, 8])
                .on_press(toggle),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        None => heading.into(),
    };
    let mut block = column![heading].spacing(4);
    if open {
        block = block.push(
            column(lines.map(|line| {
                container(text(line).size(13))
                    .padding(6)
                    .width(Length::Fill)
                    .style(style::prompt_text)
                    .into()
            }))
            .spacing(4),
        );
    }
    match match_ {
        PromptMatch::Exact(_) => container(block).style(style::prompt_exact),
        _ => container(block).style(style::notice),
    }
    .padding(6)
    .width(Length::Fill)
    .into()
}

pub fn plain_text(match_: &PromptMatch) -> Vec<String> {
    match match_ {
        PromptMatch::Exact(version) => {
            vec!["Archived prompt".into(), version.text.clone()]
        }
        PromptMatch::Nearest(candidates) => nearest_text(candidates),
        PromptMatch::Unknown(reason) => vec![format!("Prompt unknown: {reason}")],
    }
}

fn nearest_text(candidates: &[Lineage]) -> Vec<String> {
    let mut lines =
        vec!["Prompt NOT archived — candidate prompts only; these are not authoritative".into()];
    // Each line carries the label: the header scrolls away on long lineages.
    for lineage in candidates {
        for version in &lineage.versions {
            lines.push(format!(
                "Candidate only, not authoritative: {} — {}",
                lineage.path, version.text
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::PromptVersion;

    #[test]
    fn nearest_has_uncertainty_label_and_every_candidate_lineage() {
        let match_ = PromptMatch::Nearest(vec![
            Lineage {
                path: "one/lineage.txt".into(),
                versions: vec![PromptVersion {
                    digest: "a".into(),
                    text: "first".into(),
                    paths: vec![],
                    committed: 0,
                }],
            },
            Lineage {
                path: "two/lineage.txt".into(),
                versions: ["second", "third"]
                    .map(|text| PromptVersion {
                        digest: text.into(),
                        text: text.into(),
                        paths: vec![],
                        committed: 0,
                    })
                    .into(),
            },
        ]);
        let lines = plain_text(&match_);
        assert!(lines[0].contains("NOT archived"));
        assert!(lines[0].contains("not authoritative"));
        assert!(lines.iter().any(|line| line.contains("one/lineage.txt")));
        assert!(lines.iter().any(|line| line.contains("two/lineage.txt")));
        assert_eq!(lines.len(), 4);
        for line in &lines[1..] {
            assert!(
                line.starts_with("Candidate only, not authoritative:"),
                "unlabelled candidate line: {line}"
            );
        }
    }
}
