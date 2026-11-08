//! Matches cell contours using the template geometry from Java kortty 1ebbe724ee84.

use std::f64::consts::PI;
use std::sync::OnceLock;

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

pub(super) const CELL_WIDTH: usize = 8;
pub(super) const CELL_HEIGHT: usize = 16;
const BLOCK_COLUMNS: usize = CELL_WIDTH / 2;
const BLOCK_ROWS: usize = CELL_HEIGHT / 2;
const BLOCKS: usize = BLOCK_COLUMNS * BLOCK_ROWS;
const CHARACTERS: &str = " .`'\",:;-_~=+|/\\<>^vVxX*oO()LTMW%#@";
const TONAL_RAMP: &[u8] = b" .:-=+*#%@";
const THRESHOLDS: [f64; 9] = [0.03, 0.10, 0.18, 0.27, 0.37, 0.48, 0.60, 0.72, 0.86];

pub(super) fn from_gray(gray: u8) -> f32 {
    static INK: OnceLock<[f32; 256]> = OnceLock::new();
    INK.get_or_init(|| std::array::from_fn(|level| (1.0 - level as f64 / 255.0).powf(0.75) as f32))
        [usize::from(gray)]
}

pub(super) struct Ink<'a> {
    width: usize,
    height: usize,
    values: &'a [f32],
}

impl<'a> Ink<'a> {
    pub(super) fn new(width: usize, height: usize, values: &'a [f32]) -> Self {
        debug_assert_eq!(width * height, values.len());
        Self {
            width,
            height,
            values,
        }
    }

    fn get(&self, x: i32, y: i32) -> f64 {
        let x = (x.max(0) as usize).min(self.width - 1);
        let y = (y.max(0) as usize).min(self.height - 1);
        f64::from(self.values[y * self.width + x])
    }

    fn blocks(&self, x0: usize, y0: usize) -> (f64, [f32; BLOCKS]) {
        let mut sum = 0.0;
        let mut blocks = [0.0; BLOCKS];
        for y in 0..CELL_HEIGHT {
            let start = (y0 + y) * self.width + x0;
            for (x, &value) in self.values[start..start + CELL_WIDTH].iter().enumerate() {
                sum += f64::from(value);
                blocks[(y / 2) * BLOCK_COLUMNS + x / 2] += value / 4.0;
            }
        }
        (sum / (CELL_WIDTH * CELL_HEIGHT) as f64, blur(blocks))
    }

    fn direction(&self, x0: usize, y0: usize) -> (f64, f64) {
        let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        for y in y0..y0 + CELL_HEIGHT as i32 {
            for x in x0..x0 + CELL_WIDTH as i32 {
                let (nw, n, ne) = (
                    self.get(x - 1, y - 1),
                    self.get(x, y - 1),
                    self.get(x + 1, y - 1),
                );
                let (w, e) = (self.get(x - 1, y), self.get(x + 1, y));
                let (sw, s, se) = (
                    self.get(x - 1, y + 1),
                    self.get(x, y + 1),
                    self.get(x + 1, y + 1),
                );
                // Sobel gradient, with positive y pointing up for the glyph orientation.
                let gx = (ne + 2.0 * e + se) - (nw + 2.0 * w + sw);
                let gy = (nw + 2.0 * n + ne) - (sw + 2.0 * s + se);
                xx += gx * gx;
                yy += gy * gy;
                xy += gx * gy;
            }
        }
        let trace = xx + yy;
        if trace <= 1e-9 {
            return (0.0, 0.0);
        }
        let coherence = ((xx - yy).powi(2) + 4.0 * xy.powi(2)).sqrt() / trace;
        let orientation = (0.5 * (2.0 * xy).atan2(xx - yy) + PI / 2.0).rem_euclid(PI);
        (orientation, coherence)
    }

    pub(super) fn glyph(&self, column: usize, row: usize) -> char {
        let (x, y) = (column * CELL_WIDTH, row * CELL_HEIGHT);
        let (mean, samples) = self.blocks(x, y);
        if mean < THRESHOLDS[0] {
            return ' ';
        }
        let average = samples.iter().map(|&v| f64::from(v)).sum::<f64>() / BLOCKS as f64;
        let variance = samples
            .iter()
            .map(|&v| (f64::from(v) - average).powi(2))
            .sum::<f64>()
            / BLOCKS as f64;
        if variance < 0.08_f64.powi(2) {
            return char::from(TONAL_RAMP[THRESHOLDS.partition_point(|&limit| mean >= limit)]);
        }
        let (orientation, coherence) = self.direction(x, y);
        if coherence >= 0.7
            && (0.25..=0.75).contains(&mean)
            && let Some(glyph) = edge_glyph(&samples, average, orientation)
        {
            return glyph;
        }
        match_template(&samples, orientation, coherence)
    }
}

fn blur(blocks: [f32; BLOCKS]) -> [f32; BLOCKS] {
    let horizontal: [f32; BLOCKS] = std::array::from_fn(|index| {
        let col = index % BLOCK_COLUMNS;
        let left = index - usize::from(col > 0);
        let right = index + usize::from(col + 1 < BLOCK_COLUMNS);
        (blocks[left] + 2.0 * blocks[index] + blocks[right]) / 4.0
    });
    std::array::from_fn(|index| {
        let up = index
            .saturating_sub(BLOCK_COLUMNS)
            .max(index % BLOCK_COLUMNS);
        let down = (index + BLOCK_COLUMNS).min(BLOCKS - BLOCK_COLUMNS + index % BLOCK_COLUMNS);
        (horizontal[up] + 2.0 * horizontal[index] + horizontal[down]) / 4.0
    })
}

fn edge_glyph(samples: &[f32; BLOCKS], average: f64, angle: f64) -> Option<char> {
    let degrees = angle.to_degrees();
    let glyph = if (45.0..80.0).contains(&degrees) {
        '/'
    } else if (80.0..=100.0).contains(&degrees) {
        '|'
    } else if (100.0..=135.0).contains(&degrees) {
        '\\'
    } else {
        return None;
    };
    let (sine, cosine) = angle.sin_cos();
    let (mut covariance, mut position_variance, mut ink_variance) = (0.0, 0.0, 0.0);
    for (index, &sample) in samples.iter().enumerate() {
        let x = (2 * (index % BLOCK_COLUMNS) + 1) as f64 - CELL_WIDTH as f64 / 2.0;
        let y = (2 * (index / BLOCK_COLUMNS) + 1) as f64 - CELL_HEIGHT as f64 / 2.0;
        let position = -x * sine - y * cosine;
        let ink = f64::from(sample) - average;
        covariance += position * ink;
        position_variance += position * position;
        ink_variance += ink * ink;
    }
    let denominator = (position_variance * ink_variance).sqrt();
    (denominator >= 1e-9 && covariance.abs() >= 0.6 * denominator).then_some(glyph)
}

struct Template {
    character: char,
    samples: [f32; BLOCKS],
    angle: f64,
    coherence: f64,
}

fn match_template(samples: &[f32; BLOCKS], angle: f64, coherence: f64) -> char {
    static TEMPLATES: OnceLock<Vec<Template>> = OnceLock::new();
    let templates = TEMPLATES.get_or_init(|| CHARACTERS.chars().map(template).collect());
    let mut best = ' ';
    let mut best_cost = f64::INFINITY;
    for glyph in templates {
        let difference = samples
            .iter()
            .zip(glyph.samples)
            .map(|(&a, b)| f64::from(a - b).powi(2))
            .sum::<f64>()
            / BLOCKS as f64;
        let agreement = glyph.coherence * (angle - glyph.angle).cos().powi(2);
        let bias = if glyph.character == ' ' { 0.0 } else { 0.004 };
        let cost = difference + 0.05 * coherence * (coherence - agreement).max(0.0) + bias;
        if cost < best_cost {
            best = glyph.character;
            best_cost = cost;
        }
    }
    best
}

fn template(character: char) -> Template {
    let mut canvas = Pixmap::new(CELL_WIDTH as u32, CELL_HEIGHT as u32).expect("valid glyph size");
    canvas.fill(Color::WHITE);
    Shape(&mut canvas).draw(character);
    let values: Vec<_> = canvas
        .pixels()
        .iter()
        .map(|pixel| from_gray(pixel.green()))
        .collect();
    let ink = Ink::new(CELL_WIDTH, CELL_HEIGHT, &values);
    let (_, samples) = ink.blocks(0, 0);
    let (angle, coherence) = ink.direction(0, 0);
    Template {
        character,
        samples,
        angle,
        coherence,
    }
}

struct Shape<'a>(&'a mut Pixmap);

impl Shape<'_> {
    fn stroke(&mut self, path: Option<Path>) {
        if let Some(path) = path {
            let stroke = Stroke {
                width: 2.0,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            self.0.stroke_path(
                &path,
                &Paint::default(),
                &stroke,
                Transform::identity(),
                None,
            );
        }
    }

    fn line(&mut self, x: f32, y: f32, end_x: f32, end_y: f32) {
        self.polyline(&[(x, y), (end_x, end_y)]);
    }

    fn polyline(&mut self, points: &[(f32, f32)]) {
        let Some((&(x, y), rest)) = points.split_first() else {
            return;
        };
        let mut path = PathBuilder::new();
        path.move_to(x, y);
        for &(x, y) in rest {
            path.line_to(x, y);
        }
        self.stroke(path.finish());
    }

    fn curve(&mut self, start: (f32, f32), segments: &[(f32, f32, f32, f32)]) {
        let mut path = PathBuilder::new();
        path.move_to(start.0, start.1);
        for &(x, y, end_x, end_y) in segments {
            path.quad_to(x, y, end_x, end_y);
        }
        self.stroke(path.finish());
    }

    fn oval(&mut self, x: f32, y: f32, width: f32, height: f32) {
        self.stroke(Rect::from_xywh(x, y, width, height).and_then(PathBuilder::from_oval));
    }

    fn dot(&mut self, x: f32, y: f32) {
        if let Some(path) = PathBuilder::from_circle(x, y, 1.2) {
            self.0.fill_path(
                &path,
                &Paint::default(),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn draw(&mut self, character: char) {
        match character {
            ' ' => {}
            '.' => self.dot(4.0, 12.5),
            '`' => self.line(2.5, 2.0, 4.5, 4.5),
            '\'' => self.line(4.5, 2.0, 4.5, 4.5),
            '"' => {
                self.line(2.5, 2.0, 2.5, 4.5);
                self.line(5.5, 2.0, 5.5, 4.5);
            }
            ',' => {
                self.dot(4.5, 12.5);
                self.line(4.5, 12.5, 3.0, 15.0);
            }
            ':' => {
                self.dot(4.0, 6.5);
                self.dot(4.0, 12.5);
            }
            ';' => {
                self.dot(4.5, 6.5);
                self.dot(4.5, 12.5);
                self.line(4.5, 12.5, 3.0, 15.0);
            }
            '-' => self.line(1.5, 8.5, 6.5, 8.5),
            '_' => self.line(0.5, 13.5, 7.5, 13.5),
            '~' => self.curve((1.0, 9.5), &[(2.5, 6.0, 4.0, 8.5), (5.5, 11.0, 7.0, 7.5)]),
            '=' => {
                self.line(1.5, 7.0, 6.5, 7.0);
                self.line(1.5, 10.5, 6.5, 10.5);
            }
            '+' => {
                self.line(1.0, 8.5, 7.0, 8.5);
                self.line(4.0, 5.0, 4.0, 12.0);
            }
            '|' => self.line(4.0, 1.5, 4.0, 14.5),
            '/' => self.line(6.5, 1.5, 1.5, 14.5),
            '\\' => self.line(1.5, 1.5, 6.5, 14.5),
            '<' => self.polyline(&[(6.0, 4.5), (2.0, 8.5), (6.0, 12.5)]),
            '>' => self.polyline(&[(2.0, 4.5), (6.0, 8.5), (2.0, 12.5)]),
            '^' => self.polyline(&[(1.5, 6.5), (4.0, 2.5), (6.5, 6.5)]),
            'v' => self.polyline(&[(1.5, 6.5), (4.0, 12.5), (6.5, 6.5)]),
            'V' => self.polyline(&[(1.0, 2.5), (4.0, 12.5), (7.0, 2.5)]),
            'x' => {
                self.line(1.5, 6.5, 6.5, 12.5);
                self.line(6.5, 6.5, 1.5, 12.5);
            }
            'X' => {
                self.line(1.0, 2.5, 7.0, 12.5);
                self.line(7.0, 2.5, 1.0, 12.5);
            }
            '*' => {
                self.line(4.0, 4.0, 4.0, 10.0);
                self.line(1.5, 5.5, 6.5, 8.5);
                self.line(6.5, 5.5, 1.5, 8.5);
            }
            'o' => self.oval(1.5, 6.5, 5.0, 6.0),
            'O' => self.oval(1.0, 2.5, 6.0, 10.0),
            '(' => self.curve((5.5, 1.5), &[(0.5, 8.0, 5.5, 14.5)]),
            ')' => self.curve((2.5, 1.5), &[(7.5, 8.0, 2.5, 14.5)]),
            'L' => self.polyline(&[(2.0, 2.5), (2.0, 12.5), (6.5, 12.5)]),
            'T' => {
                self.line(1.0, 2.5, 7.0, 2.5);
                self.line(4.0, 2.5, 4.0, 12.5);
            }
            'M' => self.polyline(&[(1.0, 12.5), (1.0, 2.5), (4.0, 8.0), (7.0, 2.5), (7.0, 12.5)]),
            'W' => self.polyline(&[(1.0, 2.5), (2.0, 12.5), (4.0, 6.0), (6.0, 12.5), (7.0, 2.5)]),
            '%' => {
                self.line(7.0, 2.0, 1.0, 13.0);
                self.oval(0.8, 2.8, 3.4, 3.4);
                self.oval(3.8, 8.8, 3.4, 3.4);
            }
            '#' => {
                self.line(3.0, 3.5, 2.5, 12.0);
                self.line(5.5, 3.5, 5.0, 12.0);
                self.line(1.5, 6.0, 6.5, 6.0);
                self.line(1.5, 9.5, 6.5, 9.5);
            }
            '@' => {
                self.oval(0.8, 2.5, 6.4, 11.0);
                self.oval(2.5, 5.5, 3.5, 5.0);
                self.line(6.0, 5.5, 6.0, 11.0);
                self.line(3.0, 9.5, 6.5, 12.5);
            }
            _ => unreachable!("unknown built-in glyph"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ink_at: impl Fn(usize, usize) -> f32) -> char {
        let ink: [f32; CELL_WIDTH * CELL_HEIGHT] =
            std::array::from_fn(|index| ink_at(index % CELL_WIDTH, index / CELL_WIDTH));
        Ink::new(CELL_WIDTH, CELL_HEIGHT, &ink).glyph(0, 0)
    }

    #[test]
    fn flat_tones_and_vertical_edges_follow_different_matching_paths() {
        assert_eq!(cell(|_, _| 0.01), ' ');
        assert_eq!(cell(|_, _| 0.5), '*');
        assert_eq!(cell(|_, _| 0.9), '@');
        assert_eq!(cell(|x, _| if x < 4 { 1.0 } else { 0.0 }), '|');
        assert_eq!(
            cell(|x, _| if (3..5).contains(&x) { 1.0 } else { 0.0 }),
            '|'
        );
    }

    #[test]
    fn diagonal_strokes_choose_opposite_glyphs() {
        for (template, expected) in [('/', '/'), ('\\', '\\')] {
            let mut canvas = Pixmap::new(CELL_WIDTH as u32, CELL_HEIGHT as u32).unwrap();
            canvas.fill(Color::WHITE);
            Shape(&mut canvas).draw(template);
            let ink: Vec<_> = canvas
                .pixels()
                .iter()
                .map(|pixel| from_gray(pixel.green()))
                .collect();
            assert_eq!(
                Ink::new(CELL_WIDTH, CELL_HEIGHT, &ink).glyph(0, 0),
                expected
            );
        }
    }
}
