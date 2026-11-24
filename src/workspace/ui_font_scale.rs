use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse};
use iced_tiny_skia::font_scale;

use super::appearance::Preferences;

const DEFAULT_PERCENT: u16 = 100;
pub(super) const MIN_PERCENT: u16 = 80;
pub(super) const MAX_PERCENT: u16 = 160;

pub(super) fn clamp_percent(percent: i32) -> u16 {
    percent.clamp(i32::from(MIN_PERCENT), i32::from(MAX_PERCENT)) as u16
}

pub(super) fn manual(preferences: Preferences) -> u16 {
    clamp_percent(i32::from(
        preferences.ui_font_scale_percent.unwrap_or(DEFAULT_PERCENT),
    ))
}

pub(super) fn step(preferences: Preferences, larger: bool) -> u16 {
    clamp_percent(i32::from(manual(preferences)) + if larger { 5 } else { -5 })
}

pub(super) fn effective(preferences: Preferences, display: Option<Size>) -> u16 {
    if !preferences.ui_font_scale_auto.unwrap_or(false) {
        return manual(preferences);
    }
    match display.map(|size| size.height) {
        Some(height) if height >= 2300.0 => 140,
        Some(height) if height >= 1800.0 => 125,
        Some(height) if height >= 1400.0 => 110,
        _ => DEFAULT_PERCENT,
    }
}

pub(super) fn ratio(preferences: Preferences, display: Option<Size>) -> f32 {
    f32::from(effective(preferences, display)) / f32::from(DEFAULT_PERCENT)
}

pub(super) fn dialog_size(base: Size, scale: f32, display: Option<Size>) -> Size {
    let scale = scale.max(1.0);
    display.map_or(base * scale, |size| {
        Size::new(
            (base.width * scale).min(size.width).max(base.width),
            (base.height * scale).min(size.height).max(base.height),
        )
    })
}

pub(crate) fn unscaled<'a, Message: 'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    scope(content, 1.0)
}

pub(super) fn scope<'a, Message: 'a>(
    content: Element<'a, Message>,
    scale: f32,
) -> Element<'a, Message> {
    Element::new(Scoped { content, scale })
}

// Preserve layout coordinates and widget state. Only font metrics are scoped.
struct Scoped<'a, Message> {
    content: Element<'a, Message>,
    scale: f32,
}

impl<Message> Widget<Message, Theme, Renderer> for Scoped<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        font_scale::with(self.scale, || {
            self.content.as_widget_mut().layout(tree, renderer, limits)
        })
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        font_scale::with(self.scale, || {
            self.content
                .as_widget_mut()
                .operate(tree, layout, renderer, operation)
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        font_scale::with(self.scale, || {
            self.content.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            )
        });
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        font_scale::with(self.scale, || {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport)
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        font_scale::with(self.scale, || {
            self.content
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer)
        })
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        let scale = self.scale;
        font_scale::with(scale, || {
            self.content
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, translation)
        })
        .map(|content| ScopedOverlay::element(content, scale))
    }
}

struct ScopedOverlay<'a, Message> {
    content: overlay::Element<'a, Message, Theme, Renderer>,
    scale: f32,
}

impl<'a, Message: 'a> ScopedOverlay<'a, Message> {
    fn element(
        content: overlay::Element<'a, Message, Theme, Renderer>,
        scale: f32,
    ) -> overlay::Element<'a, Message, Theme, Renderer> {
        overlay::Element::new(Box::new(Self { content, scale }))
    }
}

impl<Message> overlay::Overlay<Message, Theme, Renderer> for ScopedOverlay<'_, Message> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        font_scale::with(self.scale, || {
            self.content.as_overlay_mut().layout(renderer, bounds)
        })
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        font_scale::with(self.scale, || {
            self.content
                .as_overlay_mut()
                .operate(layout, renderer, operation)
        });
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        font_scale::with(self.scale, || {
            self.content
                .as_overlay_mut()
                .update(event, layout, cursor, renderer, clipboard, shell)
        });
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        font_scale::with(self.scale, || {
            self.content
                .as_overlay()
                .draw(renderer, theme, style, layout, cursor)
        });
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        font_scale::with(self.scale, || {
            self.content
                .as_overlay()
                .mouse_interaction(layout, cursor, renderer)
        })
    }

    fn overlay<'a>(
        &'a mut self,
        layout: Layout<'a>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        let scale = self.scale;
        font_scale::with(scale, || {
            self.content.as_overlay_mut().overlay(layout, renderer)
        })
        .map(|content| ScopedOverlay::element(content, scale))
    }

    fn index(&self) -> f32 {
        self.content.as_overlay().index()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_font_scale_follows_tagged_boundaries_and_keeps_the_manual_value() {
        let mut settings = Preferences {
            ui_font_scale_percent: Some(135),
            ui_font_scale_auto: Some(true),
            ..Preferences::default()
        };
        assert_eq!(effective(settings, None), 100);
        for (height, expected) in [
            (1399.0, 100),
            (1400.0, 110),
            (1799.0, 110),
            (1800.0, 125),
            (2299.0, 125),
            (2300.0, 140),
        ] {
            assert_eq!(
                effective(settings, Some(Size::new(2000.0, height))),
                expected
            );
        }
        settings.ui_font_scale_auto = Some(false);
        assert_eq!(effective(settings, None), 135);
        assert_eq!(step(settings, true), 140);
        settings.ui_font_scale_percent = Some(159);
        assert_eq!(step(settings, true), 160);
        settings.ui_font_scale_percent = Some(81);
        assert_eq!(step(settings, false), 80);
    }

    #[test]
    fn ui_font_scale_reflows_widgets_without_resizing_independent_text() {
        use iced::widget::{pick_list, row, text, text_input};
        use iced::{Font, Pixels};

        let renderer = Renderer::new(Font::DEFAULT, Pixels(16.0));
        let limits = layout::Limits::new(Size::ZERO, Size::new(1000.0, 400.0));
        let measure = |scale| {
            let mut root: Element<'_, ()> = scope(
                row![
                    text("Settings").size(16),
                    unscaled(text("Settings").size(16).into()),
                    text_input("Profile", "Ocean").width(180),
                    pick_list(["Ocean", "Paper"], Some("Ocean"), |_| ()),
                ]
                .into(),
                scale,
            );
            let mut tree = Tree::new(root.as_widget());
            root.as_widget_mut()
                .layout(&mut tree, &renderer, &limits)
                .children()
                .iter()
                .map(|child| child.size())
                .collect::<Vec<_>>()
        };
        let normal = measure(1.0);
        let large = measure(1.6);
        let small = measure(0.8);
        assert_eq!(normal[1], large[1]);
        assert_eq!(normal[1], small[1]);
        for index in [0, 2, 3] {
            assert!(large[index].height > normal[index].height);
            assert!(small[index].height < normal[index].height);
        }
        assert!(large[0].width > normal[0].width);
    }
}
