#![allow(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub mod window;

mod clip_mask;
mod editor_caret;
mod engine;
pub mod font_scale;
mod layer;
mod primitive;
mod rect_fill;
mod settings;
mod text;

#[cfg(feature = "image")]
mod raster;

#[cfg(feature = "svg")]
mod vector;

#[cfg(feature = "geometry")]
pub mod geometry;

pub use iced_graphics as graphics;
pub use iced_graphics::core;

pub use editor_caret::{TextEditorCaret, TextEditorCaretShape};
pub use layer::Layer;
pub use primitive::Primitive;
pub use settings::Settings;

#[cfg(feature = "geometry")]
pub use geometry::Geometry;

use crate::core::renderer;
use crate::core::{
    Background, Color, Font, Pixels, Point, Rectangle, Size, Transformation,
};
use crate::engine::Engine;
use crate::graphics::Viewport;
use crate::graphics::compositor;

/// A [`tiny-skia`] graphics renderer for [`iced`].
///
/// [`tiny-skia`]: https://github.com/RazrFalcon/tiny-skia
/// [`iced`]: https://github.com/iced-rs/iced
#[derive(Debug)]
pub struct Renderer {
    default_font: Font,
    default_text_size: Pixels,
    editor_caret: Option<editor_caret::Scope>,
    layers: layer::Stack,
    engine: Engine, // TODO: Shared engine
}

impl Renderer {
    pub fn new(default_font: Font, default_text_size: Pixels) -> Self {
        Self {
            default_font,
            default_text_size,
            editor_caret: None,
            layers: layer::Stack::new(),
            engine: Engine::new(),
        }
    }

    pub fn layers(&mut self) -> &[Layer] {
        self.layers.flush();
        self.layers.as_slice()
    }

    /// Renders the accumulated backdrop over an opaque color in a small region.
    /// The caller supplies a scale suitable for the sampled effect.
    pub fn snapshot_region(
        &mut self,
        bounds: Rectangle,
        scale: f32,
        background: Color,
    ) -> Option<tiny_skia::Pixmap> {
        let bounds = bounds * self.layers.transformation();
        let width = (bounds.width * scale).ceil() as u32;
        let height = (bounds.height * scale).ceil() as u32;
        if width > 512 || height > 256 {
            return None;
        }
        let mut pixels = tiny_skia::Pixmap::new(width, height)?;
        let mut mask = tiny_skia::Mask::new(width, height)?;
        let transform = Transformation::scale(scale)
            * Transformation::translate(-bounds.x, -bounds.y);
        pixels.fill(engine::into_color(Color { a: 1.0, ..background }));
        self.layers.flush();
        self.engine.draw_layers(
            self.layers.as_slice(),
            transform,
            &mut pixels.as_mut(),
            &mut clip_mask::ClipMask::new(&mut mask),
            bounds * transform,
        );
        Some(pixels)
    }

    /// Composites the content as a single layer within the given bounds.
    pub fn with_opacity(
        &mut self,
        bounds: Rectangle,
        opacity: f32,
        draw: impl FnOnce(&mut Self),
    ) {
        if opacity >= 1.0 {
            draw(self);
            return;
        }
        if opacity > 0.0 {
            self.with_group(bounds, opacity, None, draw);
        }
    }

    /// Draws a group clipped by a circle in the current logical coordinates.
    pub fn with_circle_clip(
        &mut self,
        bounds: Rectangle,
        center: Point,
        radius: f32,
        draw: impl FnOnce(&mut Self),
    ) {
        if !radius.is_finite() || radius <= 0.0 {
            return;
        }
        let ellipse = Rectangle {
            x: center.x - radius,
            y: center.y - radius,
            width: radius * 2.0,
            height: radius * 2.0,
        };
        if let Some(clip) = bounds.intersection(&ellipse) {
            self.with_group(clip, 1.0, Some(ellipse), draw);
        }
    }

    fn with_group(
        &mut self,
        bounds: Rectangle,
        opacity: f32,
        ellipse: Option<Rectangle>,
        draw: impl FnOnce(&mut Self),
    ) {
        let (parent, transformation) = self.layers.current_mut();
        let Some(clip) = (bounds * transformation).intersection(&parent.bounds) else {
            return;
        };
        let outer = std::mem::take(&mut self.layers);
        self.layers.reset(clip);
        self.layers.push_transformation(transformation);
        draw(self);
        self.layers.flush();
        let group = layer::OpacityGroup {
            opacity,
            ellipse: ellipse.map(|ellipse| ellipse * transformation),
            layers: self.layers.as_slice().into(),
        };
        self.layers = outer;
        self.layers.push_clip(bounds);
        let (layer, _) = self.layers.current_mut();
        layer.bounds = clip;
        layer.opacity_group = Some(group);
        self.layers.pop_clip();
    }

    pub fn draw(
        &mut self,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut tiny_skia::Mask,
        viewport: &Viewport,
        damage: &[Rectangle],
        background_color: Color,
    ) {
        let scale_factor = viewport.scale_factor();
        let transformation = Transformation::scale(scale_factor);
        let mut clip_mask = clip_mask::ClipMask::new(clip_mask);

        self.layers.flush();

        for &damage_bounds in damage {
            let damage_bounds = damage_bounds * scale_factor;

            rect_fill::clear(
                pixels,
                damage_bounds,
                engine::into_color(background_color),
            );

            self.engine.draw_layers(
                self.layers.as_slice(),
                transformation,
                pixels,
                &mut clip_mask,
                damage_bounds,
            );
        }

        self.engine.trim();
    }
}

impl core::Renderer for Renderer {
    fn start_layer(&mut self, bounds: Rectangle) {
        self.layers.push_clip(bounds);
    }

    fn end_layer(&mut self) {
        self.layers.pop_clip();
    }

    fn start_transformation(&mut self, transformation: Transformation) {
        self.layers.push_transformation(transformation);
    }

    fn end_transformation(&mut self) {
        self.layers.pop_transformation();
    }

    fn fill_quad(
        &mut self,
        quad: renderer::Quad,
        background: impl Into<Background>,
    ) {
        let background = background.into();
        if background == Background::Color(TextEditorCaret::VALUE_COLOR)
            && self.editor_caret.is_some()
        {
            self.paint_editor_caret(quad);
            return;
        }
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_quad(quad, background, transformation);
    }

    fn reset(&mut self, new_bounds: Rectangle) {
        self.layers.reset(new_bounds);
    }

    fn allocate_image(
        &mut self,
        _handle: &core::image::Handle,
        callback: impl FnOnce(Result<core::image::Allocation, core::image::Error>)
        + Send
        + 'static,
    ) {
        #[cfg(feature = "image")]
        #[allow(unsafe_code)]
        // TODO: Concurrency
        callback(self.engine.raster_pipeline.load(_handle));

        #[cfg(not(feature = "image"))]
        callback(Err(core::image::Error::Unsupported));
    }
}

impl core::text::Renderer for Renderer {
    type Font = Font;
    type Paragraph = font_scale::Paragraph;
    type Editor = font_scale::Editor;

    const ICON_FONT: Font = Font::with_name("Iced-Icons");
    const CHECKMARK_ICON: char = '\u{f00c}';
    const ARROW_DOWN_ICON: char = '\u{e800}';
    const ICED_LOGO: char = '\u{e801}';
    const SCROLL_UP_ICON: char = '\u{e802}';
    const SCROLL_DOWN_ICON: char = '\u{e803}';
    const SCROLL_LEFT_ICON: char = '\u{e804}';
    const SCROLL_RIGHT_ICON: char = '\u{e805}';

    fn default_font(&self) -> Self::Font {
        self.default_font
    }

    fn default_size(&self) -> Pixels {
        self.default_text_size
    }

    fn fill_paragraph(
        &mut self,
        text: &Self::Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();

        layer.draw_paragraph(
            &text.0,
            position,
            color,
            clip_bounds,
            transformation,
        );
    }

    fn fill_editor(
        &mut self,
        editor: &Self::Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let color = match self.editor_caret.as_mut() {
            Some(scope) if color == TextEditorCaret::VALUE_COLOR => {
                scope.record_text(&editor.0, position)
            }
            _ => color,
        };
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_editor(
            &editor.0,
            position,
            color,
            clip_bounds,
            transformation,
        );
    }

    fn fill_text(
        &mut self,
        text: core::Text,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text(
            font_scale::text(text),
            position,
            color,
            clip_bounds,
            transformation,
        );
    }
}

impl graphics::text::Renderer for Renderer {
    fn fill_raw(&mut self, raw: graphics::text::Raw) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text_raw(raw, transformation);
    }
}

#[cfg(feature = "geometry")]
impl graphics::geometry::Renderer for Renderer {
    type Geometry = Geometry;
    type Frame = geometry::Frame;

    fn new_frame(&self, bounds: Rectangle) -> Self::Frame {
        geometry::Frame::new(bounds)
    }

    fn draw_geometry(&mut self, geometry: Self::Geometry) {
        let (layer, transformation) = self.layers.current_mut();

        match geometry {
            Geometry::Live {
                primitives,
                images,
                text,
                clip_bounds,
            } => {
                layer.draw_primitive_group(
                    primitives,
                    clip_bounds,
                    transformation,
                );

                for image in images {
                    layer.draw_image(image, transformation);
                }

                layer.draw_text_group(text, clip_bounds, transformation);
            }
            Geometry::Cache(cache) => {
                layer.draw_primitive_cache(
                    cache.primitives,
                    cache.clip_bounds,
                    transformation,
                );

                for image in cache.images.iter() {
                    layer.draw_image(image.clone(), transformation);
                }

                layer.draw_text_cache(
                    cache.text,
                    cache.clip_bounds,
                    transformation,
                );
            }
        }
    }
}

impl graphics::mesh::Renderer for Renderer {
    fn draw_mesh(&mut self, _mesh: graphics::Mesh) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }

    fn draw_mesh_cache(&mut self, _cache: iced_graphics::mesh::Cache) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }
}

#[cfg(feature = "image")]
impl core::image::Renderer for Renderer {
    type Handle = core::image::Handle;

    fn load_image(
        &self,
        handle: &Self::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.engine.raster_pipeline.load(handle)
    }

    fn measure_image(
        &self,
        handle: &Self::Handle,
    ) -> Option<crate::core::Size<u32>> {
        self.engine.raster_pipeline.dimensions(handle)
    }

    fn draw_image(
        &mut self,
        image: core::Image,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_raster(image, bounds, clip_bounds, transformation);
    }
}

#[cfg(feature = "svg")]
impl core::svg::Renderer for Renderer {
    fn measure_svg(
        &self,
        handle: &core::svg::Handle,
    ) -> crate::core::Size<u32> {
        self.engine.vector_pipeline.viewport_dimensions(handle)
    }

    fn draw_svg(
        &mut self,
        svg: core::Svg,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_svg(svg, bounds, clip_bounds, transformation);
    }
}

impl compositor::Default for Renderer {
    type Compositor = window::Compositor;
}

impl renderer::Headless for Renderer {
    async fn new(
        default_font: Font,
        default_text_size: Pixels,
        backend: Option<&str>,
    ) -> Option<Self> {
        if backend.is_some_and(|backend| {
            !["tiny-skia", "tiny_skia"].contains(&backend)
        }) {
            return None;
        }

        Some(Self::new(default_font, default_text_size))
    }

    fn name(&self) -> String {
        "tiny-skia".to_owned()
    }

    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8> {
        let viewport = Viewport::with_physical_size(size, scale_factor);

        window::compositor::screenshot(self, &viewport, background_color)
    }
}
