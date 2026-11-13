use std::io;

use crate::session_journal::export_branding::{PdfBranding, REPOSITORY_URL};

use super::document::{
    Color, Document, Font, Metadata, Orientation, PAGE_HEIGHT, PAGE_WIDTH, VectorStroke,
};
use super::layout_text::{fit, prose_lines, terminal_lines};

pub(crate) const MARGIN: f32 = 48.0;
pub(crate) const WIDTH: f32 = PAGE_WIDTH - 2.0 * MARGIN;
pub(super) const BODY_X: f32 = MARGIN + 52.0;
pub(super) const BODY_WIDTH: f32 = WIDTH - 52.0;
const TOP_INSET: f32 = 60.0;
const TOP: f32 = PAGE_HEIGHT - TOP_INSET;
const BOTTOM: f32 = 60.0;
const PARAGRAPH_LINE_GAP: f32 = 3.4;
const PANEL_SIZE: f32 = 8.6;
const PANEL_LEADING: f32 = PANEL_SIZE + 3.0;
const PANEL_PADDING: f32 = 6.0;
const CONTENTS_ROW_HEIGHT: f32 = 18.0;
const CONTENTS_TEXT_SIZE: f32 = 10.2;

pub(super) const BLUE: Color = [0x0f, 0x62, 0xcc];
pub(super) const MUTED: Color = [0x5e, 0x6e, 0x82];
pub(crate) const HEADING: Color = [0x1f, 0x29, 0x37];
pub(crate) const PROSE: Color = [0x37, 0x41, 0x51];
pub(super) const AMBER: Color = [0x9a, 0x67, 0x00];
pub(super) const WHITE: Color = [255; 3];

#[derive(Clone, Copy)]
pub(crate) struct Style {
    pub font: Font,
    pub size: f32,
    pub color: Color,
}

impl Style {
    pub(crate) const fn new(font: Font, size: f32, color: Color) -> Self {
        Self { font, size, color }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Panel {
    Input,
    Output,
    Diff,
}

impl Panel {
    fn colors(self) -> (Color, Color) {
        match self {
            Self::Input => ([0x11, 0x63, 0x29], [0xef, 0xf7, 0xef]),
            Self::Output | Self::Diff => ([0x0a, 0x30, 0x69], [0xee, 0xf3, 0xfb]),
        }
    }

    fn text_style(self, prefix: &str) -> Style {
        if matches!(self, Self::Diff) {
            match prefix {
                "+" => return Style::new(Font::Mono, PANEL_SIZE, [0x16, 0x65, 0x34]),
                "-" => return Style::new(Font::Mono, PANEL_SIZE, [0x99, 0x1b, 0x1b]),
                _ => {}
            }
        }
        let (accent, _) = self.colors();
        let color = accent.map(|value| (f32::from(value) * 0.7) as u8);
        Style::new(Font::Mono, PANEL_SIZE, color)
    }
}

pub(crate) struct Layout<'a> {
    pub(super) document: Document,
    pub(super) page: usize,
    pub(super) y: f32,
    cancelled: &'a dyn Fn() -> bool,
}

pub(crate) struct ContentsRow {
    page: usize,
    bottom: f32,
}

pub(crate) struct PageBlock<'a> {
    document: &'a mut Document,
    page: usize,
    top: f32,
}

impl PageBlock<'_> {
    pub(crate) fn rect(
        &mut self,
        [left, down, width, height]: [f32; 4],
        color: Color,
    ) -> io::Result<()> {
        self.document.fill_rect(
            self.page,
            [MARGIN + left, self.top - down - height, width, height],
            color,
        )
    }

    pub(crate) fn label(
        &mut self,
        [left, down]: [f32; 2],
        width: f32,
        style: Style,
        label: &str,
    ) -> io::Result<()> {
        let fitted = fit(self.document, style, width, label);
        self.document.draw_text(
            self.page,
            [MARGIN + left, self.top - down],
            style.font,
            style.size,
            style.color,
            &fitted,
        )
    }

    pub(crate) fn curve(&mut self, points: [[f32; 2]; 4], stroke: VectorStroke) -> io::Result<()> {
        self.document.curve(
            self.page,
            points.map(|[left, down]| [MARGIN + left, self.top - down]),
            stroke,
        )
    }

    pub(crate) fn round_rect(
        &mut self,
        [left, down, width, height]: [f32; 4],
        radius: f32,
        fill: Color,
        stroke: VectorStroke,
    ) -> io::Result<()> {
        self.document.round_rect(
            self.page,
            [MARGIN + left, self.top - down - height, width, height],
            radius,
            fill,
            stroke,
        )
    }

    pub(crate) fn clipped(
        &mut self,
        [left, down, width, height]: [f32; 4],
        draw: impl FnOnce(&mut Self) -> io::Result<()>,
    ) -> io::Result<()> {
        self.document.start_clip(
            self.page,
            [MARGIN + left, self.top - down - height, width, height],
        )?;
        let result = draw(self);
        let restored = self.document.finish_clip(self.page);
        result.and(restored)
    }

    pub(crate) fn image(
        &mut self,
        image: usize,
        [left, down, width, height]: [f32; 4],
    ) -> io::Result<()> {
        self.document.draw_image(
            self.page,
            image,
            [MARGIN + left, self.top - down - height, width, height],
        )
    }
}

impl<'a> Layout<'a> {
    pub(crate) fn new(cancelled: &'a dyn Fn() -> bool) -> io::Result<Self> {
        Self::with_orientation(cancelled, Orientation::Portrait)
    }

    pub(crate) fn with_orientation(
        cancelled: &'a dyn Fn() -> bool,
        orientation: Orientation,
    ) -> io::Result<Self> {
        check_cancelled(cancelled)?;
        let mut document = Document::new()?;
        let page = document.new_page(orientation)?;
        Ok(Self {
            document,
            page,
            y: orientation.size()[1] - TOP_INSET,
            cancelled,
        })
    }

    pub(crate) fn new_page(&mut self) -> io::Result<()> {
        self.start_page(Orientation::Portrait)
    }

    pub(crate) fn start_page(&mut self, orientation: Orientation) -> io::Result<()> {
        self.check_cancelled()?;
        self.page = self.document.new_page(orientation)?;
        self.y = orientation.size()[1] - TOP_INSET;
        Ok(())
    }

    pub(crate) fn ensure_portrait(&mut self) -> io::Result<()> {
        if matches!(self.document.orientation(self.page), Orientation::Landscape) {
            self.new_page()?;
        }
        Ok(())
    }

    pub(super) fn check_cancelled(&self) -> io::Result<()> {
        check_cancelled(self.cancelled)
    }

    pub(crate) fn ensure_space(&mut self, height: f32) -> io::Result<()> {
        if self.y - height < BOTTOM {
            self.new_page()?;
        }
        Ok(())
    }

    pub(crate) fn vector_block(&mut self, height: f32) -> io::Result<PageBlock<'_>> {
        self.check_cancelled()?;
        self.ensure_space(height)?;
        let top = self.y;
        self.y -= height;
        Ok(PageBlock {
            document: &mut self.document,
            page: self.page,
            top,
        })
    }

    pub(crate) fn page_block(&mut self) -> PageBlock<'_> {
        let top = self.document.orientation(self.page).size()[1];
        PageBlock {
            document: &mut self.document,
            page: self.page,
            top,
        }
    }

    pub(crate) fn embed_rgba(&mut self, image: &image::RgbaImage) -> io::Result<usize> {
        let mut rgb = Vec::with_capacity(image.as_raw().len() / 4 * 3);
        let mut alpha = Vec::with_capacity(image.as_raw().len() / 4);
        for &[r, g, b, a] in image.as_raw().as_chunks::<4>().0 {
            rgb.extend_from_slice(&[r, g, b]);
            alpha.push(a);
        }
        self.document
            .embed_image(image.width(), image.height(), &rgb, Some(&alpha))
    }

    pub(crate) fn wrap_text(&self, width: f32, style: Style, value: &str) -> Vec<String> {
        prose_lines(&self.document, style, width, value)
    }

    pub(super) fn text(&mut self, at: [f32; 2], style: Style, text: &str) -> io::Result<()> {
        self.document
            .draw_text(self.page, at, style.font, style.size, style.color, text)
    }

    pub(super) fn fitted(
        &mut self,
        at: [f32; 2],
        width: f32,
        style: Style,
        text: &str,
    ) -> io::Result<()> {
        let text = fit(&self.document, style, width, text);
        self.text(at, style, &text)
    }

    pub(crate) fn paragraph(
        &mut self,
        [x, width]: [f32; 2],
        style: Style,
        text: &str,
        trailing: f32,
    ) -> io::Result<()> {
        let leading = style.size + PARAGRAPH_LINE_GAP;
        for line in prose_lines(&self.document, style, width, text) {
            self.ensure_space(leading)?;
            self.text([x, self.y - style.size], style, &line)?;
            self.y -= leading;
        }
        self.y -= trailing;
        Ok(())
    }

    pub(super) fn divider(&mut self, label: &str) -> io::Result<()> {
        self.ensure_space(32.0)?;
        self.y -= 10.0;
        self.text(
            [MARGIN, self.y],
            Style::new(Font::Bold, 12.0, HEADING),
            label,
        )?;
        self.document.line(
            self.page,
            [MARGIN, self.y - 5.0],
            [MARGIN + WIDTH, self.y - 5.0],
            [0xd0, 0xd7, 0xde],
            0.8,
        )?;
        self.y -= 18.0;
        Ok(())
    }

    pub(crate) fn panel<'b>(
        &mut self,
        geometry: [f32; 2],
        panel: Panel,
        lines: impl IntoIterator<Item = (&'b str, &'b str)>,
    ) -> io::Result<()> {
        let width = geometry[1] - 2.0 * PANEL_PADDING - 4.0;
        let mut chunk = Vec::new();
        let mut fit = 0;
        for (prefix, value) in lines {
            let style = panel.text_style(prefix);
            for (index, line) in value.split('\n').enumerate() {
                self.check_cancelled()?;
                let prefix = if index == 0 { prefix } else { "" };
                for line in terminal_lines(&self.document, style, width, prefix, line) {
                    if chunk.is_empty() {
                        self.ensure_space(2.0 * (PANEL_LEADING + PANEL_PADDING))?;
                        fit = ((self.y - BOTTOM - 2.0 * PANEL_PADDING) / PANEL_LEADING).floor()
                            as usize;
                    }
                    chunk.push((line, style));
                    if chunk.len() == fit {
                        self.panel_chunk(geometry, panel, &chunk)?;
                        chunk.clear();
                    }
                }
            }
        }
        if !chunk.is_empty() {
            self.panel_chunk(geometry, panel, &chunk)?;
        }
        Ok(())
    }

    fn panel_chunk(
        &mut self,
        [x, width]: [f32; 2],
        panel: Panel,
        lines: &[(String, Style)],
    ) -> io::Result<()> {
        let (accent, background) = panel.colors();
        let height = lines.len() as f32 * PANEL_LEADING + 2.0 * PANEL_PADDING;
        self.document
            .fill_rect(self.page, [x, self.y - height, width, height], background)?;
        self.document
            .fill_rect(self.page, [x, self.y - height, 3.0, height], accent)?;
        let mut baseline = self.y - PANEL_PADDING - PANEL_SIZE;
        for (line, style) in lines {
            self.text([x + PANEL_PADDING + 4.0, baseline], *style, line)?;
            baseline -= PANEL_LEADING;
        }
        self.y -= height + 4.0;
        Ok(())
    }

    pub(crate) fn bookmark(&mut self, title: &str) -> io::Result<()> {
        self.document
            .bookmark(self.page, self.y, title, None)
            .map(|_| ())
    }

    pub(crate) fn contents_row(&mut self, title: &str) -> io::Result<ContentsRow> {
        self.check_cancelled()?;
        self.ensure_space(CONTENTS_ROW_HEIGHT)?;
        self.y -= CONTENTS_ROW_HEIGHT;
        let style = Style::new(Font::Regular, CONTENTS_TEXT_SIZE, BLUE);
        self.fitted([MARGIN, self.y + 5.0], WIDTH - 42.0, style, title)?;
        Ok(ContentsRow {
            page: self.page,
            bottom: self.y,
        })
    }

    pub(crate) fn link_contents_row(&mut self, row: ContentsRow) -> io::Result<()> {
        let number = (self.page + 1).to_string();
        let width = self
            .document
            .text_width(Font::Regular, CONTENTS_TEXT_SIZE, &number);
        self.document.draw_text(
            row.page,
            [MARGIN + WIDTH - width, row.bottom + 5.0],
            Font::Regular,
            CONTENTS_TEXT_SIZE,
            MUTED,
            &number,
        )?;
        self.document.link_page(
            row.page,
            [MARGIN, row.bottom, WIDTH, CONTENTS_ROW_HEIGHT],
            self.page,
            self.y,
        )
    }

    /// Fit wide 2x rasters to a landscape page and continue tall ones on portrait pages.
    pub(crate) fn report_image(
        &mut self,
        image: &image::RgbImage,
        [title, continuation]: [&str; 2],
        style: Style,
    ) -> io::Result<()> {
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 {
            return Ok(());
        }
        let heading_gap = 4.0;
        let heading_height = style.size + PARAGRAPH_LINE_GAP + heading_gap;
        let portrait_height = TOP - BOTTOM - heading_height - 8.0;
        let portrait_width_scale = 0.5_f32.min(WIDTH / width as f32);
        let portrait_fit = portrait_width_scale.min(portrait_height / height as f32);
        let landscape_width = PAGE_HEIGHT - 2.0 * MARGIN;
        let landscape_height = PAGE_WIDTH - TOP_INSET - BOTTOM - heading_height - 8.0;
        let landscape_fit = 0.5_f32
            .min(landscape_width / width as f32)
            .min(landscape_height / height as f32);
        let landscape = landscape_fit > 1.15 * portrait_fit;
        let (scale, rows, content_width) = if landscape {
            self.start_page(Orientation::Landscape)?;
            (landscape_fit, height, landscape_width)
        } else {
            self.ensure_portrait()?;
            let rows = (portrait_height / portrait_width_scale).floor() as u32;
            (portrait_width_scale, rows, WIDTH)
        };
        let draw_width = width as f32 * scale;
        let x = if landscape {
            MARGIN + (content_width - draw_width) / 2.0
        } else {
            MARGIN
        };
        let stride = width as usize * 3;
        for start in (0..height).step_by(rows as usize) {
            self.check_cancelled()?;
            let end = start.saturating_add(rows).min(height);
            let slice_height = (end - start) as f32 * scale;
            if !landscape {
                self.ensure_space(heading_height + slice_height + 8.0)?;
            }
            let heading = if start == 0 { title } else { continuation };
            self.paragraph([MARGIN, content_width], style, heading, heading_gap)?;
            let pixels = &image.as_raw()[start as usize * stride..end as usize * stride];
            let index = self
                .document
                .embed_image(width, end - start, pixels, None)?;
            self.document.draw_image(
                self.page,
                index,
                [x, self.y - slice_height, draw_width, slice_height],
            )?;
            self.y -= slice_height + 8.0;
        }
        Ok(())
    }

    pub(crate) fn finish(self, branding: &PdfBranding) -> io::Result<Vec<u8>> {
        self.finish_with_metadata(branding, None)
    }

    pub(crate) fn finish_with_metadata(
        mut self,
        branding: &PdfBranding,
        metadata: Option<Metadata<'_>>,
    ) -> io::Result<Vec<u8>> {
        let total = self.document.page_count();
        let footer = Style::new(Font::Regular, 8.4, [0x9c, 0xa3, 0xaf]);
        for page in 0..total {
            self.check_cancelled()?;
            self.page = page;
            let [page_width, _] = self.document.orientation(page).size();
            let content_width = page_width - 2.0 * MARGIN;
            self.watermark(branding)?;
            self.document.line(
                page,
                [MARGIN, 42.0],
                [MARGIN + content_width, 42.0],
                [0xe5, 0xe7, 0xeb],
                0.7,
            )?;
            let label = format!("{} / {total}", page + 1);
            let label_width = self.document.text_width(footer.font, footer.size, &label);
            let footer_width = content_width - label_width - 12.0;
            self.text([MARGIN + content_width - label_width, 28.0], footer, &label)?;
            if let Some(brand) = &branding.footer {
                self.fitted([MARGIN, 28.0], footer_width, footer, &brand.text)?;
                if brand.repository_link
                    && let Some(prefix) = brand.text.strip_suffix(REPOSITORY_URL)
                    && self
                        .document
                        .text_width(footer.font, footer.size, &brand.text)
                        <= footer_width
                {
                    let start = self.document.text_width(footer.font, footer.size, prefix);
                    let width = self
                        .document
                        .text_width(footer.font, footer.size, REPOSITORY_URL);
                    self.document
                        .footer_uri(page, [MARGIN + start, 24.0, width, footer.size + 4.0])?;
                }
            }
        }
        self.check_cancelled()?;
        self.document.finish_with_metadata(metadata)
    }

    fn watermark(&mut self, branding: &PdfBranding) -> io::Result<()> {
        let Some(watermark) = &branding.watermark else {
            return Ok(());
        };
        const ANGLE: f32 = 38.0;
        const OPACITY: f32 = 0.08;
        let mut style = Style::new(Font::Bold, 26.0, watermark.color);
        let width = self.document.orientation(self.page).size()[0] - 120.0;
        while style.size > 14.0
            && self
                .document
                .text_width(style.font, style.size, &watermark.brand.text)
                > width
        {
            style.size -= 1.0;
        }
        let title = fit(&self.document, style, width, &watermark.brand.text);
        self.watermark_line(style, &title, 0.0, [ANGLE, OPACITY])?;
        if watermark.brand.repository_link {
            let url_style = Style::new(Font::Regular, style.size * 0.45, style.color);
            self.watermark_line(url_style, REPOSITORY_URL, -style.size, [ANGLE, OPACITY])?;
        }
        Ok(())
    }

    fn watermark_line(
        &mut self,
        style: Style,
        text: &str,
        y: f32,
        [angle, opacity]: [f32; 2],
    ) -> io::Result<()> {
        let (sin, cos) = angle.to_radians().sin_cos();
        let x = -self.document.text_width(style.font, style.size, text) / 2.0;
        let [width, height] = self.document.orientation(self.page).size();
        self.document.draw_rotated_text(
            self.page,
            [
                width / 2.0 + x * cos - y * sin,
                height / 2.0 + x * sin + y * cos,
            ],
            style.font,
            style.size,
            style.color,
            text,
            [angle, opacity],
        )
    }
}

pub(super) fn check_cancelled(cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Journal export cancelled",
        ))
    } else {
        Ok(())
    }
}
