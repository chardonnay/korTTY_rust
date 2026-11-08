use std::io;

use crate::session_journal::export_branding::{PdfBranding, REPOSITORY_URL};

use super::document::{Color, Document, Font, PAGE_HEIGHT, PAGE_WIDTH};
use super::layout_text::{fit, prose_lines, terminal_lines};

pub(super) const MARGIN: f32 = 48.0;
pub(super) const WIDTH: f32 = PAGE_WIDTH - 2.0 * MARGIN;
pub(super) const BODY_X: f32 = MARGIN + 52.0;
pub(super) const BODY_WIDTH: f32 = WIDTH - 52.0;
const TOP: f32 = PAGE_HEIGHT - 60.0;
const BOTTOM: f32 = 60.0;
const PANEL_SIZE: f32 = 8.6;
const PANEL_LEADING: f32 = PANEL_SIZE + 3.0;
const PANEL_PADDING: f32 = 6.0;

pub(super) const BLUE: Color = [0x0f, 0x62, 0xcc];
pub(super) const MUTED: Color = [0x5e, 0x6e, 0x82];
pub(super) const HEADING: Color = [0x1f, 0x29, 0x37];
pub(super) const PROSE: Color = [0x37, 0x41, 0x51];
pub(super) const AMBER: Color = [0x9a, 0x67, 0x00];
pub(super) const WHITE: Color = [255; 3];

#[derive(Clone, Copy)]
pub(super) struct Style {
    pub font: Font,
    pub size: f32,
    pub color: Color,
}

impl Style {
    pub(super) const fn new(font: Font, size: f32, color: Color) -> Self {
        Self { font, size, color }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Panel {
    Input,
    Output,
}

impl Panel {
    fn colors(self) -> (Color, Color) {
        match self {
            Self::Input => ([0x11, 0x63, 0x29], [0xef, 0xf7, 0xef]),
            Self::Output => ([0x0a, 0x30, 0x69], [0xee, 0xf3, 0xfb]),
        }
    }

    fn text_style(self) -> Style {
        let (accent, _) = self.colors();
        let color = accent.map(|value| (f32::from(value) * 0.7) as u8);
        Style::new(Font::Mono, PANEL_SIZE, color)
    }
}

pub(super) struct Layout<'a> {
    pub document: Document,
    pub page: usize,
    pub y: f32,
    cancelled: &'a dyn Fn() -> bool,
}

impl<'a> Layout<'a> {
    pub(super) fn new(cancelled: &'a dyn Fn() -> bool) -> io::Result<Self> {
        check_cancelled(cancelled)?;
        let mut document = Document::new()?;
        let page = document.new_page()?;
        Ok(Self {
            document,
            page,
            y: TOP,
            cancelled,
        })
    }

    pub(super) fn new_page(&mut self) -> io::Result<()> {
        self.check_cancelled()?;
        self.page = self.document.new_page()?;
        self.y = TOP;
        Ok(())
    }

    pub(super) fn check_cancelled(&self) -> io::Result<()> {
        check_cancelled(self.cancelled)
    }

    pub(super) fn ensure_space(&mut self, height: f32) -> io::Result<()> {
        if self.y - height < BOTTOM {
            self.new_page()?;
        }
        Ok(())
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

    pub(super) fn paragraph(
        &mut self,
        [x, width]: [f32; 2],
        style: Style,
        text: &str,
        trailing: f32,
    ) -> io::Result<()> {
        let leading = style.size + 3.4;
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

    pub(super) fn panel<'b>(
        &mut self,
        geometry: [f32; 2],
        panel: Panel,
        lines: impl IntoIterator<Item = (&'b str, &'b str)>,
    ) -> io::Result<()> {
        let style = panel.text_style();
        let width = geometry[1] - 2.0 * PANEL_PADDING - 4.0;
        let mut chunk = Vec::new();
        let mut fit = 0;
        for (prefix, value) in lines {
            for (index, line) in value.split('\n').enumerate() {
                self.check_cancelled()?;
                let prefix = if index == 0 { prefix } else { "" };
                for line in terminal_lines(&self.document, style, width, prefix, line) {
                    if chunk.is_empty() {
                        self.ensure_space(2.0 * (PANEL_LEADING + PANEL_PADDING))?;
                        fit = ((self.y - BOTTOM - 2.0 * PANEL_PADDING) / PANEL_LEADING).floor()
                            as usize;
                    }
                    chunk.push(line);
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
        lines: &[String],
    ) -> io::Result<()> {
        let (accent, background) = panel.colors();
        let height = lines.len() as f32 * PANEL_LEADING + 2.0 * PANEL_PADDING;
        self.document
            .fill_rect(self.page, [x, self.y - height, width, height], background)?;
        self.document
            .fill_rect(self.page, [x, self.y - height, 3.0, height], accent)?;
        let style = panel.text_style();
        let mut baseline = self.y - PANEL_PADDING - PANEL_SIZE;
        for line in lines {
            self.text([x + PANEL_PADDING + 4.0, baseline], style, line)?;
            baseline -= PANEL_LEADING;
        }
        self.y -= height + 4.0;
        Ok(())
    }

    pub(super) fn finish(mut self, branding: &PdfBranding) -> io::Result<Vec<u8>> {
        let total = self.document.page_count();
        let footer = Style::new(Font::Regular, 8.4, [0x9c, 0xa3, 0xaf]);
        for page in 0..total {
            self.check_cancelled()?;
            self.page = page;
            self.watermark(branding)?;
            self.document.line(
                page,
                [MARGIN, 42.0],
                [MARGIN + WIDTH, 42.0],
                [0xe5, 0xe7, 0xeb],
                0.7,
            )?;
            let label = format!("{} / {total}", page + 1);
            let label_width = self.document.text_width(footer.font, footer.size, &label);
            let footer_width = WIDTH - label_width - 12.0;
            self.text([MARGIN + WIDTH - label_width, 28.0], footer, &label)?;
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
        self.document.finish()
    }

    fn watermark(&mut self, branding: &PdfBranding) -> io::Result<()> {
        let Some(watermark) = &branding.watermark else {
            return Ok(());
        };
        const ANGLE: f32 = 38.0;
        const OPACITY: f32 = 0.08;
        let mut style = Style::new(Font::Bold, 26.0, watermark.color);
        let width = PAGE_WIDTH - 120.0;
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
        self.document.draw_rotated_text(
            self.page,
            [
                PAGE_WIDTH / 2.0 + x * cos - y * sin,
                PAGE_HEIGHT / 2.0 + x * sin + y * cos,
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
