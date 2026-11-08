use std::io::{self, Cursor};
use std::path::Path;

use image::{
    ImageDecoder as _, ImageEncoder as _, ImageFormat, ImageReader, Limits, Rgba, RgbaImage,
    imageops,
};
use resvg::{tiny_skia, usvg};
use roxmltree::{Document, ParsingOptions};

use crate::profile;

use super::{invalid, read_bounded};

const EDGE: u32 = 128;
const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_EDGE: u32 = 2048;
const MAX_SVG_BYTES: usize = 256 * 1024;
const SVG_NS: &str = "http://www.w3.org/2000/svg";

pub(super) fn load(path: &Path) -> io::Result<Vec<u8>> {
    let bytes = read_bounded(profile::open_preference(path)?, MAX_INPUT_BYTES)?;
    let mut square = match image::guess_format(&bytes) {
        Ok(format @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif)) => {
            let image = raster(&bytes, format, MAX_IMAGE_EDGE)?;
            let mut square = RgbaImage::new(EDGE, EDGE);
            let image = image
                .resize(EDGE, EDGE, imageops::FilterType::Triangle)
                .into_rgba8();
            let x = i64::from((EDGE - image.width()) / 2);
            let y = i64::from((EDGE - image.height()) / 2);
            imageops::overlay(&mut square, &image, x, y);
            square
        }
        Err(_) => render_svg(&bytes)?,
        _ => return Err(invalid("Choose a PNG, JPEG, GIF, or static SVG image.")),
    };
    round_corners(&mut square);
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(square.as_raw(), EDGE, EDGE, image::ExtendedColorType::Rgba8)
        .map_err(io::Error::other)?;
    Ok(png)
}

pub(super) fn validate_stored(bytes: Vec<u8>) -> io::Result<Vec<u8>> {
    let image = raster(&bytes, ImageFormat::Png, EDGE)?;
    if image.width() != EDGE || image.height() != EDGE {
        return Err(invalid("The stored OS symbol has unexpected dimensions."));
    }
    Ok(bytes)
}

fn raster(bytes: &[u8], format: ImageFormat, limit: u32) -> io::Result<image::DynamicImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(limit);
    limits.max_image_height = Some(limit);
    limits.max_alloc = Some(u64::from(limit) * u64::from(limit) * 8);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(io::Error::other)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || width > limit || height > limit {
        return Err(invalid("The OS symbol has unsupported dimensions."));
    }
    limits
        .reserve(decoder.total_bytes())
        .map_err(io::Error::other)?;
    decoder.set_limits(limits).map_err(io::Error::other)?;
    image::DynamicImage::from_decoder(decoder).map_err(io::Error::other)
}

fn render_svg(bytes: &[u8]) -> io::Result<RgbaImage> {
    if bytes.len() > MAX_SVG_BYTES {
        return Err(invalid("The SVG exceeds 256 KiB."));
    }
    let source = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let document = Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 2048,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Choose a well-formed SVG without DTDs."))?;
    if document.root_element().tag_name().name() != "svg" {
        return Err(invalid("Choose a PNG, JPEG, GIF, or static SVG image."));
    }
    for node in document.descendants() {
        if node.is_pi() || (node.is_element() && !safe_element(node)) {
            return Err(invalid(
                "The SVG uses unsupported content. Use static shapes and local gradients.",
            ));
        }
    }
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(source, &options).map_err(io::Error::other)?;
    let size = tree.size();
    if [size.width(), size.height()]
        .iter()
        .any(|side| !(0.001..=16_384.0).contains(side))
    {
        return Err(invalid("The SVG viewport exceeds the supported bounds."));
    }
    let edge = EDGE as f32;
    let scale = (edge / size.width()).min(edge / size.height());
    let x = (edge - size.width() * scale) / 2.0;
    let y = (edge - size.height() * scale) / 2.0;
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, x, y);
    let mut surface = tiny_skia::Pixmap::new(EDGE, EDGE)
        .ok_or_else(|| invalid("Could not allocate the OS symbol raster."))?;
    resvg::render(&tree, transform, &mut surface.as_mut());
    let mut square = RgbaImage::new(EDGE, EDGE);
    for (output, pixel) in square.pixels_mut().zip(surface.pixels()) {
        let pixel = pixel.demultiply();
        *output = Rgba([pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]);
    }
    Ok(square)
}

fn safe_element(node: roxmltree::Node<'_, '_>) -> bool {
    matches!(node.tag_name().namespace(), None | Some(SVG_NS))
        && matches!(
            node.tag_name().name(),
            "svg"
                | "g"
                | "path"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "defs"
                | "linearGradient"
                | "radialGradient"
                | "stop"
                | "title"
                | "desc"
        )
        && node.ancestors().take(34).count() < 34
        && node.attributes().all(|attribute| {
            let name = attribute.name();
            let value = attribute.value();
            !name.starts_with("on")
                && name != "base"
                && (name != "href" || local_reference(value))
                && value
                    .to_ascii_lowercase()
                    .split("url(")
                    .skip(1)
                    .all(|tail| {
                        tail.split_once(')').is_some_and(|(url, _)| {
                            local_reference(url.trim().trim_matches(['\'', '"']))
                        })
                    })
        })
}

fn local_reference(value: &str) -> bool {
    value.strip_prefix('#').is_some_and(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    })
}

fn round_corners(image: &mut RgbaImage) {
    let radius = EDGE as f32 * 0.21;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let x = (radius - (x.min(EDGE - 1 - x) as f32 + 0.5)).max(0.0);
        let y = (radius - (y.min(EDGE - 1 - y) as f32 + 0.5)).max(0.0);
        let coverage = (radius + 0.5 - x.hypot(y)).clamp(0.0, 1.0);
        pixel[3] = (f32::from(pixel[3]) * coverage).round() as u8;
    }
}
