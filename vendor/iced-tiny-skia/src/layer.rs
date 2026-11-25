use crate::Primitive;
use crate::core::renderer::Quad;
use crate::core::{
    self, Background, Color, Point, Rectangle, Svg, Transformation,
};
use crate::graphics::damage;
use crate::graphics::layer;
use crate::graphics::text::{Editor, Paragraph, Text};
use crate::graphics::{self, Image};

use std::sync::Arc;

pub type Stack = layer::Stack<Layer>;

fn text_visible_bounds(text: &Text) -> Option<Rectangle> {
    match text {
        Text::Editor {
            clip_bounds,
            transformation,
            ..
        } => Some(*clip_bounds * *transformation),
        Text::Cached {
            align_x,
            align_y,
            bounds,
            clip_bounds,
            ..
        } => {
            // Cached text positions are alignment anchors, not top-left corners.
            // Include the pixels above/left of an anchor when invalidating a frame.
            let x = match align_x {
                core::text::Alignment::Center => bounds.x - bounds.width / 2.0,
                core::text::Alignment::Right => bounds.x - bounds.width,
                _ => bounds.x,
            };
            let y = match align_y {
                core::alignment::Vertical::Center => {
                    bounds.y - bounds.height / 2.0
                }
                core::alignment::Vertical::Bottom => bounds.y - bounds.height,
                core::alignment::Vertical::Top => bounds.y,
            };
            Rectangle { x, y, ..*bounds }.intersection(clip_bounds)
        }
        _ => text.visible_bounds(),
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub bounds: Rectangle,
    pub(crate) opacity_group: Option<OpacityGroup>,
    pub quads: Vec<(Quad, Background)>,
    pub primitives: Vec<Item<Primitive>>,
    pub images: Vec<Image>,
    pub text: Vec<Item<Text>>,
}

#[derive(Debug, Clone)]
pub(crate) struct OpacityGroup {
    pub(crate) opacity: f32,
    pub(crate) ellipse: Option<Rectangle>,
    pub(crate) layers: Arc<[Layer]>,
}

impl Layer {
    pub fn draw_quad(
        &mut self,
        mut quad: Quad,
        background: Background,
        transformation: Transformation,
    ) {
        quad.bounds = quad.bounds * transformation;
        self.quads.push((quad, background));
    }

    pub fn draw_paragraph(
        &mut self,
        paragraph: &Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let paragraph = Text::Paragraph {
            paragraph: paragraph.downgrade(),
            position,
            color,
            clip_bounds,
            transformation,
        };

        self.text.push(Item::Live(paragraph));
    }

    pub fn draw_editor(
        &mut self,
        editor: &Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let editor = Text::Editor {
            editor: editor.downgrade(),
            position,
            color,
            clip_bounds,
            transformation,
        };

        self.text.push(Item::Live(editor));
    }

    pub fn draw_text(
        &mut self,
        text: core::Text,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let text = Text::Cached {
            content: text.content,
            bounds: Rectangle::new(position, text.bounds) * transformation,
            color,
            size: text.size * transformation.scale_factor(),
            line_height: text.line_height.to_absolute(text.size)
                * transformation.scale_factor(),
            font: text.font,
            align_x: text.align_x,
            align_y: text.align_y,
            shaping: text.shaping,
            clip_bounds: clip_bounds * transformation,
        };

        self.text.push(Item::Live(text));
    }

    pub fn draw_text_raw(
        &mut self,
        raw: graphics::text::Raw,
        transformation: Transformation,
    ) {
        let raw = Text::Raw {
            raw,
            transformation,
        };

        self.text.push(Item::Live(raw));
    }

    pub fn draw_text_group(
        &mut self,
        text: Vec<Text>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        self.text
            .push(Item::Group(text, clip_bounds, transformation));
    }

    pub fn draw_text_cache(
        &mut self,
        text: Arc<[Text]>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        self.text
            .push(Item::Cached(text, clip_bounds, transformation));
    }

    pub fn draw_image(&mut self, image: Image, transformation: Transformation) {
        match image {
            Image::Raster {
                image,
                bounds,
                clip_bounds,
            } => {
                self.draw_raster(image, bounds, clip_bounds, transformation);
            }
            Image::Vector {
                svg,
                bounds,
                clip_bounds,
            } => {
                self.draw_svg(svg, bounds, clip_bounds, transformation);
            }
        }
    }

    pub fn draw_raster(
        &mut self,
        image: core::Image,
        bounds: Rectangle,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let image = Image::Raster {
            image: core::Image {
                border_radius: image.border_radius
                    * transformation.scale_factor(),
                ..image
            },
            bounds: bounds * transformation,
            clip_bounds: clip_bounds * transformation,
        };

        self.images.push(image);
    }

    pub fn draw_svg(
        &mut self,
        svg: Svg,
        bounds: Rectangle,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let svg = Image::Vector {
            svg,
            bounds: bounds * transformation,
            clip_bounds: clip_bounds * transformation,
        };

        self.images.push(svg);
    }

    pub fn draw_primitive_group(
        &mut self,
        primitives: Vec<Primitive>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        self.primitives.push(Item::Group(
            primitives,
            clip_bounds * transformation,
            transformation,
        ));
    }

    pub fn draw_primitive_cache(
        &mut self,
        primitives: Arc<[Primitive]>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        self.primitives.push(Item::Cached(
            primitives,
            clip_bounds * transformation,
            transformation,
        ));
    }

    pub fn damage(previous: &Self, current: &Self) -> Vec<Rectangle> {
        if previous.bounds != current.bounds {
            return vec![previous.bounds, current.bounds];
        }
        if previous.opacity_group.is_some() || current.opacity_group.is_some() {
            return vec![current.bounds];
        }

        let mut damage = damage::list(
            &previous.quads,
            &current.quads,
            |(quad, _)| {
                quad.bounds
                    .expand(1.0)
                    .intersection(&current.bounds)
                    .into_iter()
                    .collect()
            },
            |(quad_a, background_a), (quad_b, background_b)| {
                quad_a == quad_b && background_a == background_b
            },
        );

        let text = damage::diff(
            &previous.text,
            &current.text,
            |item| {
                item.as_slice()
                    .iter()
                    .filter_map(text_visible_bounds)
                    .map(|bounds| bounds * item.transformation())
                    .collect()
            },
            |text_a, text_b| {
                damage::list(
                    text_a.as_slice(),
                    text_b.as_slice(),
                    |text| {
                        text_visible_bounds(text)
                            .into_iter()
                            .map(|bounds| bounds * text_a.transformation())
                            .collect()
                    },
                    |text_a, text_b| {
                        if let (
                            Text::Editor {
                                editor: editor_a, ..
                            },
                            Text::Editor {
                                editor: editor_b, ..
                            },
                        ) = (text_a, text_b)
                        {
                            // Editor equality only compares font and dimensions. Parked
                            // tabs keep both buffers alive, so their identities must also
                            // match before retaining the previous frame's pixels.
                            let same_buffer = match (
                                editor_a.upgrade(),
                                editor_b.upgrade(),
                            ) {
                                (Some(a), Some(b)) => {
                                    std::ptr::eq(a.buffer(), b.buffer())
                                }
                                _ => false,
                            };
                            if !same_buffer {
                                return false;
                            }
                        }
                        text_a == text_b
                    },
                )
            },
        );

        let primitives = damage::list(
            &previous.primitives,
            &current.primitives,
            |item| match item {
                Item::Live(primitive) => vec![primitive.visible_bounds()],
                Item::Group(primitives, group_bounds, transformation) => {
                    primitives
                        .as_slice()
                        .iter()
                        .map(Primitive::visible_bounds)
                        .map(|bounds| bounds * *transformation)
                        .filter_map(|bounds| bounds.intersection(group_bounds))
                        .collect()
                }
                Item::Cached(_primitives, bounds, _transformation) => {
                    vec![*bounds]
                }
            },
            |primitive_a, primitive_b| match (primitive_a, primitive_b) {
                (
                    Item::Cached(cache_a, bounds_a, transformation_a),
                    Item::Cached(cache_b, bounds_b, transformation_b),
                ) => {
                    Arc::ptr_eq(cache_a, cache_b)
                        && bounds_a == bounds_b
                        && transformation_a == transformation_b
                }
                _ => false,
            },
        );

        let images = damage::list(
            &previous.images,
            &current.images,
            |image| vec![image.bounds().expand(1.0)],
            Image::eq,
        );

        damage.extend(text);
        damage.extend(primitives);
        damage.extend(images);
        damage
    }
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            bounds: Rectangle::INFINITE,
            opacity_group: None,
            quads: Vec::new(),
            primitives: Vec::new(),
            text: Vec::new(),
            images: Vec::new(),
        }
    }
}

impl graphics::Layer for Layer {
    fn with_bounds(bounds: Rectangle) -> Self {
        Self {
            bounds,
            ..Self::default()
        }
    }

    fn bounds(&self) -> Rectangle {
        self.bounds
    }

    fn flush(&mut self) {}

    fn resize(&mut self, bounds: Rectangle) {
        self.bounds = bounds;
    }

    fn reset(&mut self) {
        self.bounds = Rectangle::INFINITE;
        self.opacity_group = None;

        self.quads.clear();
        self.primitives.clear();
        self.text.clear();
        self.images.clear();
    }

    fn start(&self) -> usize {
        if self.opacity_group.is_some() {
            return 0;
        }
        if !self.quads.is_empty() {
            return 1;
        }

        if !self.primitives.is_empty() {
            return 2;
        }

        if !self.images.is_empty() {
            return 3;
        }

        if !self.text.is_empty() {
            return 4;
        }

        usize::MAX
    }

    fn end(&self) -> usize {
        if self.opacity_group.is_some() {
            return 5;
        }
        if !self.text.is_empty() {
            return 4;
        }

        if !self.images.is_empty() {
            return 3;
        }

        if !self.primitives.is_empty() {
            return 2;
        }

        if !self.quads.is_empty() {
            return 1;
        }

        0
    }

    fn merge(&mut self, layer: &mut Self) {
        self.quads.append(&mut layer.quads);
        self.primitives.append(&mut layer.primitives);
        self.text.append(&mut layer.text);
        self.images.append(&mut layer.images);
    }
}

#[derive(Debug, Clone)]
pub enum Item<T> {
    Live(T),
    Group(Vec<T>, Rectangle, Transformation),
    Cached(Arc<[T]>, Rectangle, Transformation),
}

impl<T> Item<T> {
    pub fn transformation(&self) -> Transformation {
        match self {
            Item::Live(_) => Transformation::IDENTITY,
            Item::Group(_, _, transformation)
            | Item::Cached(_, _, transformation) => *transformation,
        }
    }

    pub fn clip_bounds(&self) -> Rectangle {
        match self {
            Item::Live(_) => Rectangle::INFINITE,
            Item::Group(_, clip_bounds, _)
            | Item::Cached(_, clip_bounds, _) => *clip_bounds,
        }
    }

    pub fn as_slice(&self) -> &[T] {
        match self {
            Item::Live(item) => std::slice::from_ref(item),
            Item::Group(group, _, _) => group.as_slice(),
            Item::Cached(cache, _, _) => cache,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Renderer;
    use crate::core::Renderer as _;
    use crate::core::text::Renderer as _;
    use crate::core::{Font, Size, alignment};
    use crate::graphics::Viewport;

    fn cached_label(
        renderer: &mut Renderer,
        position: Point,
        horizontal: core::text::Alignment,
        vertical: alignment::Vertical,
        clip: Rectangle,
    ) {
        renderer.fill_text(
            core::Text {
                content: "Own process".to_owned(),
                bounds: Size::new(130.0, 20.0),
                size: 16.0.into(),
                line_height: core::text::LineHeight::Absolute(20.0.into()),
                font: Font::DEFAULT,
                align_x: horizontal,
                align_y: vertical,
                shaping: core::text::Shaping::Advanced,
                wrapping: core::text::Wrapping::None,
            },
            position,
            Color::BLACK,
            clip,
        );
    }

    #[test]
    fn cached_text_respects_its_local_clip() {
        let viewport = Viewport::with_physical_size(Size::new(300, 180), 1.0);
        let full = Rectangle::with_size(viewport.logical_size());
        let clip =
            Rectangle::new(Point::new(20.0, 20.0), Size::new(260.0, 60.0));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        renderer.reset(full);
        cached_label(
            &mut renderer,
            Point::new(25.0, 78.0),
            core::text::Alignment::Left,
            alignment::Vertical::Center,
            clip,
        );
        let mut pixels = tiny_skia::Pixmap::new(300, 180).unwrap();
        let mut mask = tiny_skia::Mask::new(300, 180).unwrap();
        renderer.draw(
            &mut pixels.as_mut(),
            &mut mask,
            &viewport,
            &[full],
            Color::WHITE,
        );
        let white = pixels.pixels()[0];
        assert!(pixels.pixels().iter().any(|pixel| *pixel != white));
        for y in 80..180 {
            assert!(
                pixels.pixels()[y * 300..(y + 1) * 300]
                    .iter()
                    .all(|pixel| *pixel == white),
                "cached label escaped below its clip at row {y}"
            );
        }
    }

    #[test]
    fn cached_text_incremental_scroll_matches_a_full_repaint() {
        for scale in [1.0, 2.0] {
            for (horizontal, x) in [
                (core::text::Alignment::Left, 25.0),
                (core::text::Alignment::Center, 150.0),
                (core::text::Alignment::Right, 275.0),
            ] {
                for (vertical, y) in [
                    (alignment::Vertical::Top, 98.0),
                    (alignment::Vertical::Center, 112.0),
                    (alignment::Vertical::Bottom, 118.0),
                ] {
                    let width = (300.0 * scale) as u32;
                    let height = (220.0 * scale) as u32;
                    let viewport = Viewport::with_physical_size(
                        Size::new(width, height),
                        scale,
                    );
                    let full = Rectangle::with_size(viewport.logical_size());
                    let clip = Rectangle::new(
                        Point::new(20.0, 20.0),
                        Size::new(260.0, 90.0),
                    );
                    let mut renderer =
                        Renderer::new(Font::DEFAULT, 16.0.into());
                    renderer.reset(full);
                    cached_label(
                        &mut renderer,
                        Point::new(x, y),
                        horizontal,
                        vertical,
                        clip,
                    );
                    let previous = renderer.layers().to_vec();
                    let mut actual =
                        tiny_skia::Pixmap::new(width, height).unwrap();
                    let mut mask = tiny_skia::Mask::new(width, height).unwrap();
                    renderer.draw(
                        &mut actual.as_mut(),
                        &mut mask,
                        &viewport,
                        &[full],
                        Color::WHITE,
                    );
                    let white = actual.pixels()[0];
                    assert!(
                        actual.pixels().iter().any(|pixel| *pixel != white),
                        "fixture text must be visible"
                    );

                    renderer.reset(full);
                    cached_label(
                        &mut renderer,
                        Point::new(x, 180.0),
                        horizontal,
                        vertical,
                        clip,
                    );
                    let damage = damage::group(
                        damage::diff(
                            &previous,
                            renderer.layers(),
                            |layer| vec![layer.bounds],
                            Layer::damage,
                        ),
                        full,
                    );
                    renderer.draw(
                        &mut actual.as_mut(),
                        &mut mask,
                        &viewport,
                        &damage,
                        Color::WHITE,
                    );
                    let mut expected =
                        tiny_skia::Pixmap::new(width, height).unwrap();
                    renderer.draw(
                        &mut expected.as_mut(),
                        &mut mask,
                        &viewport,
                        &[full],
                        Color::WHITE,
                    );
                    let different = actual
                        .pixels()
                        .iter()
                        .zip(expected.pixels())
                        .filter(|(a, b)| a != b)
                        .count();
                    assert_eq!(
                        different, 0,
                        "stale pixels after {horizontal:?}/{vertical:?} scroll at scale {scale}"
                    );
                }
            }
        }
    }
}
