use iced::widget::{checkbox, column, container, row, rule, space, text, text_input};
use iced::{Alignment, Element, Length};

use crate::session_journal::export_branding::{
    DEFAULT_FOOTER, DEFAULT_WATERMARK, Preferences, brand_input,
};
use crate::terminal_theme::Rgb;

use super::{Desktop, Dialog, Message, Setting, highlighted};

#[derive(Clone, Debug)]
pub enum Edit {
    Watermark(bool),
    WatermarkText(String),
    WatermarkColor(String),
    Footer(bool),
    FooterText(String),
}

#[derive(Default)]
pub(super) struct Editor {
    color_input: Option<String>,
}

impl Editor {
    pub(super) fn valid(&self, preferences: &Preferences) -> bool {
        !preferences.watermark_enabled
            || self
                .color_input
                .as_deref()
                .is_none_or(|input| Rgb::parse(input).is_some())
    }

    pub(super) fn update(&mut self, preferences: &mut Preferences, edit: Edit) {
        match edit {
            Edit::Watermark(enabled) => preferences.watermark_enabled = enabled,
            Edit::Footer(enabled) => preferences.footer_enabled = enabled,
            Edit::WatermarkText(text) if preferences.watermark_enabled => {
                preferences.watermark_text = brand_input(&text);
            }
            Edit::WatermarkColor(mut text) if preferences.watermark_enabled => {
                text.truncate(text.floor_char_boundary(12));
                if let Some(color) = Rgb::parse(&text) {
                    preferences.watermark_color = color;
                }
                self.color_input = Some(text);
            }
            Edit::FooterText(text) if preferences.footer_enabled => {
                preferences.footer_text = brand_input(&text);
            }
            _ => {}
        }
    }
}

pub(super) fn view(dialog: &Dialog) -> Element<'_, Desktop> {
    let id = dialog.id;
    let send = move |edit| Desktop::Settings(Message::Export(id, edit));
    let preferences = &dialog.draft.preferences.export_branding;
    let color = preferences.watermark_color;
    let color_value = color.to_string();
    let watermark_text = text_input(DEFAULT_WATERMARK, &preferences.watermark_text).on_input_maybe(
        preferences
            .watermark_enabled
            .then_some(move |text| send(Edit::WatermarkText(text))),
    );
    let watermark_color = text_input(
        "#RRGGBB",
        dialog.export.color_input.as_deref().unwrap_or(&color_value),
    )
    .on_input_maybe(
        preferences
            .watermark_enabled
            .then_some(move |text| send(Edit::WatermarkColor(text))),
    )
    .width(120);
    let footer_text = text_input(DEFAULT_FOOTER, &preferences.footer_text).on_input_maybe(
        preferences
            .footer_enabled
            .then_some(move |text| send(Edit::FooterText(text))),
    );
    let swatch = container(space::horizontal())
        .width(20)
        .height(20)
        .style(move |_| container::Style::default().background(color.color()));
    column![
        text("Exported documents").size(18),
        text("Applies to session journal exports.").size(13),
        highlighted(dialog, Setting::ExportWatermark, column![
            checkbox(preferences.watermark_enabled)
                .label(Setting::ExportWatermark.label())
                .on_toggle(move |enabled| send(Edit::Watermark(enabled))),
            row![text("Watermark text").width(Length::Fixed(140.0)), watermark_text].spacing(8).align_y(Alignment::Center),
            row![text("Watermark colour").width(Length::Fixed(140.0)), watermark_color, swatch].spacing(8).align_y(Alignment::Center),
            text("Drawn faintly across every PDF page. Leave the text empty for the built-in watermark.").size(13),
        ].spacing(8)),
        rule::horizontal(1),
        highlighted(dialog, Setting::ExportFooter, column![
            checkbox(preferences.footer_enabled)
                .label(Setting::ExportFooter.label())
                .on_toggle(move |enabled| send(Edit::Footer(enabled))),
            row![text("Footer text").width(Length::Fixed(140.0)), footer_text].spacing(8).align_y(Alignment::Center),
            text("Shown in PDF, Markdown and the exported journal page. Leave it empty for the built-in text and repository link.").size(13),
        ].spacing(8)),
    ].spacing(12).into()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::super::{Section, WindowSettings, Workspace};
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn journal_export_branding_settings_validate_cancel_and_persist() {
        let scratch = Scratch(std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-export-preferences-{}",
            crate::private_files::nonce().unwrap()
        )));
        fs::create_dir(&scratch.0).unwrap();
        let path = scratch.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        workspace.window_settings.save(&path).unwrap();
        let original = fs::read(&path).unwrap();
        let legacy: WindowSettings = serde_json::from_str("{}").unwrap();
        assert!(legacy.export_branding.is_default());
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Export));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::Export(id, Edit::Watermark(true)));
        let _ = workspace.update_settings(Message::Export(id, Edit::WatermarkColor("#123".into())));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid color reached persistence"));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::ExportWatermark)
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            workspace.window_settings.export_branding,
            legacy.export_branding
        );

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Export));
        let id = workspace.settings.0.as_ref().unwrap().id;
        for edit in [
            Edit::Watermark(true),
            Edit::WatermarkColor("#364A71".into()),
            Edit::WatermarkText("Example watermark".into()),
            Edit::FooterText("Example footer".into()),
        ] {
            let _ = workspace.update_settings(Message::Export(id, edit));
        }
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.0.is_none());
        let saved: WindowSettings = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            saved.export_branding,
            workspace.window_settings.export_branding
        );
        let branding = saved.export_branding.resolve();
        let watermark = branding.watermark.unwrap();
        assert_eq!(watermark.color, [0x36, 0x4a, 0x71]);
        assert_eq!(watermark.brand.text, "Example watermark");
        let footer = branding.footer.unwrap();
        assert_eq!(footer.text, "Example footer");
        assert!(!footer.repository_link);
    }
}
