use vte::Params;

use crate::{Cell, Model, Style};

#[derive(Clone, Copy)]
pub(crate) struct Rectangle {
    pub(crate) top: usize,
    pub(crate) left: usize,
    pub(crate) bottom: usize,
    pub(crate) right: usize,
}

impl Model {
    pub(crate) fn horizontal_bounds(&self, column: usize) -> (usize, usize) {
        if (self.scroll_left..=self.scroll_right).contains(&column) {
            (self.scroll_left, self.scroll_right)
        } else {
            (0, self.columns - 1)
        }
    }

    pub(crate) fn set_horizontal_margins(&mut self, enabled: bool) {
        self.horizontal_margins = enabled;
        if !enabled {
            self.scroll_left = 0;
            self.scroll_right = self.columns - 1;
        }
        self.screen_mut().wrap_pending = false;
    }

    pub(crate) fn set_horizontal_region(&mut self, left: usize, right: usize) {
        let left = left.min(self.columns - 1);
        let right = right.min(self.columns - 1);
        if left < right {
            self.scroll_left = left;
            self.scroll_right = right;
            self.address_cursor(0, 0);
        }
    }

    pub(crate) fn horizontal_index(&mut self, forward: bool) {
        let cursor = self.cursor();
        let edge = if forward {
            self.scroll_right
        } else {
            self.scroll_left
        };
        if cursor.column == edge && (self.scroll_top..=self.scroll_bottom).contains(&cursor.row) {
            let rectangle = Rectangle {
                top: self.scroll_top,
                left: self.scroll_left,
                bottom: self.scroll_bottom,
                right: self.scroll_right,
            };
            self.screen_mut().shift_rectangle(rectangle, forward);
        } else {
            self.move_columns(if forward { 1 } else { -1 });
        }
    }

    fn rectangular_position(&self, row: u16, column: u16) -> (usize, usize) {
        let (top, left) = if self.origin_mode {
            (self.scroll_top, self.scroll_left)
        } else {
            (0, 0)
        };
        (
            (top + usize::from(row.saturating_sub(1))).min(self.rows - 1),
            (left + usize::from(column.saturating_sub(1))).min(self.columns - 1),
        )
    }

    fn rectangle(&self, [top, left, bottom, right]: [u16; 4]) -> Option<Rectangle> {
        let (top, left) = self.rectangular_position(top, left);
        let (bottom, right) = self.rectangular_position(
            if bottom == 0 {
                self.rows as u16
            } else {
                bottom
            },
            if right == 0 {
                self.columns as u16
            } else {
                right
            },
        );
        (top <= bottom && left <= right).then_some(Rectangle {
            top,
            left,
            bottom,
            right,
        })
    }

    pub(crate) fn edit_rectangle(&mut self, params: &Params, action: char) {
        let mut values = params.iter().map(|value| value[0]);
        let fill = if action == 'x' {
            values.next().unwrap_or(0)
        } else {
            0
        };
        let coordinates = std::array::from_fn(|_| values.next().unwrap_or(0));
        let Some(rectangle) = self.rectangle(coordinates) else {
            return;
        };
        match action {
            'x' => {
                let byte = match fill {
                    32..=126 | 160..=255 => fill as u8,
                    _ => return,
                };
                let slot = if byte >= 0x80 {
                    self.right_charset
                } else {
                    self.charset
                };
                let ch = self.charsets[slot].map(char::from(byte & 0x7f));
                let cell = Cell {
                    text: ch.to_string(),
                    width: 1,
                    style: self.style,
                    hyperlink: None,
                    protected: self.protected,
                };
                self.screen_mut().fill_rectangle(rectangle, &cell);
            }
            'z' | '{' => {
                for row in rectangle.top..=rectangle.bottom {
                    self.screen_mut().erase_line(
                        row,
                        rectangle.left,
                        rectangle.right + 1,
                        Style::default(),
                        action == '{',
                    );
                }
            }
            'v' => {
                let source_page = values.next().unwrap_or(0);
                let row = values.next().unwrap_or(0);
                let column = values.next().unwrap_or(0);
                let target_page = values.next().unwrap_or(0);
                if source_page <= 1 && target_page <= 1 {
                    let (row, column) = self.rectangular_position(row, column);
                    self.screen_mut().copy_rectangle(rectangle, row, column);
                }
            }
            'r' | 't' => {
                let reverse = action == 't';
                let rectangle_only = self.rectangle_attributes;
                let columns = self.columns;
                let attributes = values.collect::<Vec<_>>();
                for row in rectangle.top..=rectangle.bottom {
                    let left = if rectangle_only || row == rectangle.top {
                        rectangle.left
                    } else {
                        0
                    };
                    let right = if rectangle_only || row == rectangle.bottom {
                        rectangle.right
                    } else {
                        columns - 1
                    };
                    let line = &mut self.screen_mut().lines[row];
                    let (left, right) = line.glyph_bounds(left, right);
                    for cell in &mut line.cells[left..=right] {
                        apply_attributes(&mut cell.style, &attributes, reverse);
                    }
                }
            }
            _ => {}
        }
    }
}

fn apply_attributes(style: &mut Style, attributes: &[u16], reverse: bool) {
    let attributes = if attributes.is_empty() {
        &[0]
    } else {
        attributes
    };
    for &value in attributes {
        match value {
            0 => {
                style.bold = reverse && !style.bold;
                style.underline = reverse && !style.underline;
                style.blink = reverse && !style.blink;
                style.inverse = reverse && !style.inverse;
            }
            1 => style.bold = !reverse || !style.bold,
            4 => style.underline = !reverse || !style.underline,
            5 => style.blink = !reverse || !style.blink,
            7 => style.inverse = !reverse || !style.inverse,
            22 if !reverse => style.bold = false,
            24 if !reverse => style.underline = false,
            25 if !reverse => style.blink = false,
            27 if !reverse => style.inverse = false,
            _ => {}
        }
    }
}
