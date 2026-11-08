//! Bounded native PDF pages, images, text and navigation.

use std::collections::BTreeSet;
use std::io::{self, Write as _};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use pdf_writer::types::{ActionType, AnnotationType};
use pdf_writer::{Content, Filter, Finish as _, Name, Pdf, Rect, Ref, Str, TextStr};

use crate::session_journal::{
    MAX_PNG_PIXELS, export_branding::REPOSITORY_URL, is_display_character,
};

mod fonts;
pub(super) use fonts::Font;

pub(super) const PAGE_WIDTH: f32 = 595.2756;
pub(super) const PAGE_HEIGHT: f32 = 841.8898;
const MAX_PAGES: usize = 4096;
const MAX_IMAGES: usize = 4096;
const MAX_BOOKMARKS: usize = 4096;
const MAX_OPACITIES: usize = 256;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 1024 * 1024;

pub(super) type Color = [u8; 3];

pub(super) struct Document {
    fonts: [fonts::Embedded; 3],
    pages: Vec<Page>,
    images: Vec<Image>,
    opacities: Vec<f32>,
    bookmarks: Vec<Bookmark>,
    retained_bytes: usize,
}

struct Page {
    content: Content,
    fonts: [bool; 3],
    images: BTreeSet<usize>,
    opacities: BTreeSet<usize>,
    footer_link: Option<Rect>,
}

struct Image {
    width: i32,
    height: i32,
    rgb: Vec<u8>,
    alpha: Option<Vec<u8>>,
}

struct Bookmark {
    page: usize,
    top: f32,
    title: String,
    parent: Option<usize>,
}

struct TextPaint {
    font: Font,
    size: f32,
    color: Color,
    matrix: [f32; 6],
    opacity: Option<usize>,
}

impl Document {
    pub(super) fn new() -> io::Result<Self> {
        Ok(Self {
            fonts: [
                fonts::Embedded::new(Font::Regular)?,
                fonts::Embedded::new(Font::Bold)?,
                fonts::Embedded::new(Font::Mono)?,
            ],
            pages: Vec::new(),
            images: Vec::new(),
            opacities: Vec::new(),
            bookmarks: Vec::new(),
            retained_bytes: 0,
        })
    }

    pub(super) fn new_page(&mut self) -> io::Result<usize> {
        if self.pages.len() == MAX_PAGES {
            return Err(invalid("The journal PDF exceeds its page limit"));
        }
        let index = self.pages.len();
        self.pages.push(Page {
            content: Content::new(),
            fonts: [false; 3],
            images: BTreeSet::new(),
            opacities: BTreeSet::new(),
            footer_link: None,
        });
        Ok(index)
    }

    pub(super) fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn text_width(&self, font: Font, size: f32, text: &str) -> f32 {
        self.fonts[font.index()].width(size, text)
    }

    pub(super) fn draw_text(
        &mut self,
        page: usize,
        [x, y]: [f32; 2],
        font: Font,
        size: f32,
        color: Color,
        text: &str,
    ) -> io::Result<()> {
        let paint = TextPaint {
            font,
            size,
            color,
            matrix: [1.0, 0.0, 0.0, 1.0, x, y],
            opacity: None,
        };
        self.text(page, paint, text)
    }

    /// The given position is the page-space origin of the rotated baseline.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_rotated_text(
        &mut self,
        page: usize,
        [x, y]: [f32; 2],
        font: Font,
        size: f32,
        color: Color,
        text: &str,
        [angle_degrees, opacity]: [f32; 2],
    ) -> io::Result<()> {
        if !angle_degrees.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(invalid("Invalid PDF watermark geometry"));
        }
        let opacity = if opacity == 1.0 {
            None
        } else {
            Some(self.opacity(opacity)?)
        };
        let (sin, cos) = angle_degrees.to_radians().sin_cos();
        let paint = TextPaint {
            font,
            size,
            color,
            matrix: [cos, sin, -sin, cos, x, y],
            opacity,
        };
        self.text(page, paint, text)
    }

    fn text(&mut self, page: usize, paint: TextPaint, text: &str) -> io::Result<()> {
        if text.len() > MAX_TEXT_BYTES || !(0.0..=512.0).contains(&paint.size) || paint.size == 0.0
        {
            return Err(invalid("Invalid journal PDF text size"));
        }
        coordinates(&paint.matrix)?;
        let bytes = self.fonts[paint.font.index()].encode(text)?;
        self.paint(page, |page| {
            if bytes.is_empty() {
                return;
            }
            page.fonts[paint.font.index()] = true;
            let stream = &mut page.content;
            stream.save_state();
            if let Some(index) = paint.opacity {
                page.opacities.insert(index);
                let resource = opacity_name(index);
                stream.set_parameters(Name(resource.as_bytes()));
            }
            let [red, green, blue] = rgb(paint.color);
            stream
                .set_fill_rgb(red, green, blue)
                .begin_text()
                .set_font(paint.font.resource(), paint.size)
                .set_text_matrix(paint.matrix)
                .show(Str(&bytes))
                .end_text()
                .restore_state();
        })
    }

    pub(super) fn fill_rect(
        &mut self,
        page: usize,
        bounds: [f32; 4],
        color: Color,
    ) -> io::Result<()> {
        rectangle(bounds)?;
        let [x, y, width, height] = bounds;
        let [red, green, blue] = rgb(color);
        self.paint(page, |page| {
            page.content
                .set_fill_rgb(red, green, blue)
                .rect(x, y, width, height)
                .fill_nonzero();
        })
    }

    pub(super) fn line(
        &mut self,
        page: usize,
        from: [f32; 2],
        to: [f32; 2],
        color: Color,
        width: f32,
    ) -> io::Result<()> {
        coordinates(&from)?;
        coordinates(&to)?;
        line_width(width)?;
        let [red, green, blue] = rgb(color);
        self.paint(page, |page| {
            page.content
                .set_stroke_rgb(red, green, blue)
                .set_line_width(width)
                .move_to(from[0], from[1])
                .line_to(to[0], to[1])
                .stroke();
        })
    }

    pub(super) fn embed_image(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u8],
        alpha: Option<&[u8]>,
    ) -> io::Result<usize> {
        let area = u64::from(width) * u64::from(height);
        if !(1..=MAX_PNG_PIXELS).contains(&area) || self.images.len() == MAX_IMAGES {
            return Err(invalid(
                "The journal PDF image exceeds its size or count limit",
            ));
        }
        let area = area as usize;
        if pixels.len() != area * 3 || alpha.is_some_and(|alpha| alpha.len() != area) {
            return Err(invalid("Invalid journal PDF image samples"));
        }
        let rgb = deflate(pixels)?;
        let alpha = alpha
            .filter(|bytes| bytes.iter().any(|&byte| byte != 255))
            .map(deflate)
            .transpose()?;
        self.retain(rgb.len() + alpha.as_ref().map_or(0, Vec::len))?;
        let index = self.images.len();
        self.images.push(Image {
            width: width as i32,
            height: height as i32,
            rgb,
            alpha,
        });
        Ok(index)
    }

    pub(super) fn draw_image(
        &mut self,
        page: usize,
        image: usize,
        bounds: [f32; 4],
    ) -> io::Result<()> {
        rectangle(bounds)?;
        if image >= self.images.len() {
            return Err(invalid("Unknown journal PDF image"));
        }
        let [x, y, width, height] = bounds;
        let name = image_name(image);
        self.paint(page, |page| {
            page.images.insert(image);
            page.content
                .save_state()
                .transform([width, 0.0, 0.0, height, x, y])
                .x_object(Name(name.as_bytes()))
                .restore_state();
        })
    }

    pub(super) fn bookmark(
        &mut self,
        page: usize,
        top: f32,
        title: &str,
        parent: Option<usize>,
    ) -> io::Result<usize> {
        let index = self.bookmarks.len();
        if page >= self.pages.len()
            || index == MAX_BOOKMARKS
            || parent.is_some_and(|parent| parent >= index)
        {
            return Err(invalid("Invalid journal PDF outline"));
        }
        coordinates(&[top])?;
        let title = title
            .chars()
            .filter(|&character| is_display_character(character))
            .take(512)
            .collect();
        self.bookmarks.push(Bookmark {
            page,
            top: top.clamp(0.0, PAGE_HEIGHT),
            title,
            parent,
        });
        Ok(index)
    }

    pub(super) fn footer_uri(&mut self, page: usize, bounds: [f32; 4]) -> io::Result<()> {
        rectangle(bounds)?;
        let [x, y, width, height] = bounds;
        let page = self.page(page)?;
        page.footer_link = Some(Rect::new(x, y, x + width, y + height));
        Ok(())
    }

    fn opacity(&mut self, alpha: f32) -> io::Result<usize> {
        if let Some(index) = self.opacities.iter().position(|&value| value == alpha) {
            return Ok(index);
        }
        if self.opacities.len() == MAX_OPACITIES {
            return Err(invalid("The journal PDF exceeds its opacity limit"));
        }
        self.opacities.push(alpha);
        Ok(self.opacities.len() - 1)
    }

    fn page(&mut self, page: usize) -> io::Result<&mut Page> {
        self.pages
            .get_mut(page)
            .ok_or_else(|| invalid("Unknown journal PDF page"))
    }

    fn paint(&mut self, page: usize, draw: impl FnOnce(&mut Page)) -> io::Result<()> {
        let page = self.page(page)?;
        let before = page.content.len();
        draw(page);
        let bytes = page.content.len() - before;
        self.retain(bytes)
    }

    fn retain(&mut self, bytes: usize) -> io::Result<()> {
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        if self.retained_bytes > MAX_BYTES {
            Err(invalid("The journal PDF exceeds its 64 MiB content limit"))
        } else {
            Ok(())
        }
    }

    pub(super) fn finish(self) -> io::Result<Vec<u8>> {
        if self.pages.is_empty() || self.retained_bytes > MAX_BYTES {
            return Err(invalid(
                "The journal PDF has no pages or exceeds its size limit",
            ));
        }
        let mut pdf = Pdf::new();
        let mut ids = Ref::new(1);
        let catalog = ids.bump();
        let page_tree = ids.bump();
        let page_ids: Vec<_> = (0..self.pages.len()).map(|_| ids.bump()).collect();
        let outline = (!self.bookmarks.is_empty()).then(|| ids.bump());
        let mut entry = pdf.catalog(catalog);
        entry.pages(page_tree);
        if let Some(outline) = outline {
            entry.outlines(outline);
        }
        entry.finish();
        pdf.document_info(ids.bump())
            .creator(TextStr("kortty"))
            .producer(TextStr("kortty"));
        pdf.pages(page_tree)
            .kids(page_ids.iter().copied())
            .count(page_ids.len() as i32);

        let mut fonts = [None; 3];
        for (index, font) in self.fonts.into_iter().enumerate() {
            fonts[index] = font.write(&mut pdf, &mut ids)?;
        }
        let images: Vec<_> = self
            .images
            .into_iter()
            .map(|image| image.write(&mut pdf, &mut ids))
            .collect();
        let opacities: Vec<_> = self
            .opacities
            .into_iter()
            .map(|alpha| {
                let id = ids.bump();
                pdf.ext_graphics(id).non_stroking_alpha(alpha);
                id
            })
            .collect();
        let resources = Resources {
            fonts,
            images: &images,
            opacities: &opacities,
        };
        for (page, &id) in self.pages.into_iter().zip(&page_ids) {
            page.write(&mut pdf, &mut ids, id, page_tree, &resources)?;
        }
        if let Some(outline) = outline {
            write_outline(&mut pdf, &mut ids, outline, &page_ids, self.bookmarks);
        }
        let bytes = pdf.finish();
        if bytes.len() > MAX_BYTES {
            return Err(invalid("The serialized journal PDF exceeds 64 MiB"));
        }
        Ok(bytes)
    }
}

struct Resources<'a> {
    fonts: [Option<Ref>; 3],
    images: &'a [Ref],
    opacities: &'a [Ref],
}

impl Page {
    fn write(
        self,
        pdf: &mut Pdf,
        ids: &mut Ref,
        id: Ref,
        parent: Ref,
        resources: &Resources<'_>,
    ) -> io::Result<()> {
        let stream = ids.bump();
        pdf.stream(stream, &deflate(&self.content.finish())?)
            .filter(Filter::FlateDecode);
        let annotation = self.footer_link.map(|bounds| {
            let annotation = ids.bump();
            pdf.annotation(annotation)
                .subtype(AnnotationType::Link)
                .page(id)
                .rect(bounds)
                .border(0.0, 0.0, 0.0, None)
                .action()
                .action_type(ActionType::Uri)
                .uri(Str(REPOSITORY_URL.as_bytes()));
            annotation
        });
        let mut page = pdf.page(id);
        page.parent(parent)
            .media_box(Rect::new(0.0, 0.0, PAGE_WIDTH, PAGE_HEIGHT))
            .contents(stream);
        if let Some(annotation) = annotation {
            page.annotations([annotation]);
        }
        let mut page_resources = page.resources();
        if self.fonts.iter().any(|&used| used) {
            let mut dictionary = page_resources.fonts();
            for font in Font::ALL {
                if self.fonts[font.index()] {
                    dictionary.pair(
                        font.resource(),
                        resources.fonts[font.index()].expect("page font was used"),
                    );
                }
            }
        }
        if !self.images.is_empty() {
            let mut dictionary = page_resources.x_objects();
            for index in self.images {
                let name = image_name(index);
                dictionary.pair(Name(name.as_bytes()), resources.images[index]);
            }
        }
        if !self.opacities.is_empty() {
            let mut dictionary = page_resources.ext_g_states();
            for index in self.opacities {
                let name = opacity_name(index);
                dictionary.pair(Name(name.as_bytes()), resources.opacities[index]);
            }
        }
        Ok(())
    }
}

impl Image {
    fn write(self, pdf: &mut Pdf, ids: &mut Ref) -> Ref {
        let mask = self.alpha.as_ref().map(|alpha| {
            let mask = ids.bump();
            let mut image = pdf.image_xobject(mask, alpha);
            image
                .width(self.width)
                .height(self.height)
                .bits_per_component(8)
                .color_space()
                .device_gray();
            image.filter(Filter::FlateDecode);
            mask
        });
        let id = ids.bump();
        let mut image = pdf.image_xobject(id, &self.rgb);
        image
            .width(self.width)
            .height(self.height)
            .bits_per_component(8)
            .color_space()
            .device_rgb();
        image.filter(Filter::FlateDecode);
        if let Some(mask) = mask {
            image.s_mask(mask);
        }
        id
    }
}

fn write_outline(
    pdf: &mut Pdf,
    ids: &mut Ref,
    outline: Ref,
    pages: &[Ref],
    bookmarks: Vec<Bookmark>,
) {
    let refs: Vec<_> = bookmarks.iter().map(|_| ids.bump()).collect();
    let mut children = vec![Vec::new(); bookmarks.len() + 1];
    let mut descendants = vec![0; children.len()];
    for (index, bookmark) in bookmarks.iter().enumerate() {
        children[bookmark.parent.map_or(0, |parent| parent + 1)].push(index);
    }
    for (index, bookmark) in bookmarks.iter().enumerate().rev() {
        let slot = bookmark.parent.map_or(0, |parent| parent + 1);
        descendants[slot] += 1 + descendants[index + 1];
    }
    pdf.outline(outline)
        .first(refs[children[0][0]])
        .last(refs[*children[0].last().expect("outline root exists")])
        .count(descendants[0]);
    for (slot, siblings) in children.iter().enumerate() {
        let parent = if slot == 0 { outline } else { refs[slot - 1] };
        for (offset, &index) in siblings.iter().enumerate() {
            let bookmark = &bookmarks[index];
            let mut item = pdf.outline_item(refs[index]);
            item.title(TextStr(&bookmark.title)).parent(parent);
            if offset > 0 {
                item.prev(refs[siblings[offset - 1]]);
            }
            if let Some(&next) = siblings.get(offset + 1) {
                item.next(refs[next]);
            }
            if let Some((&first, &last)) =
                children[index + 1].first().zip(children[index + 1].last())
            {
                item.first(refs[first])
                    .last(refs[last])
                    .count(descendants[index + 1]);
            }
            item.dest()
                .page(pages[bookmark.page])
                .xyz(0.0, bookmark.top, None);
        }
    }
}

fn image_name(index: usize) -> String {
    format!("Im{index}")
}

fn opacity_name(index: usize) -> String {
    format!("GS{index}")
}

fn rgb(color: Color) -> [f32; 3] {
    color.map(|channel| f32::from(channel) / 255.0)
}

fn coordinates(values: &[f32]) -> io::Result<()> {
    if values
        .iter()
        .all(|value| (-1_000_000.0..=1_000_000.0).contains(value))
    {
        Ok(())
    } else {
        Err(invalid("Invalid journal PDF coordinates"))
    }
}

fn rectangle(bounds: [f32; 4]) -> io::Result<()> {
    coordinates(&bounds)?;
    if bounds[2] < 0.0 || bounds[3] < 0.0 {
        return Err(invalid("Invalid journal PDF rectangle"));
    }
    Ok(())
}

fn line_width(width: f32) -> io::Result<()> {
    if (0.0..=512.0).contains(&width) {
        Ok(())
    } else {
        Err(invalid("Invalid journal PDF stroke width"))
    }
}

fn deflate(input: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(input)?;
    encoder.finish()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
