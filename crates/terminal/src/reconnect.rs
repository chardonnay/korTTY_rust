//! History handoff between terminal actors when a connection is replaced.

use crate::{
    MAX_IMAGE_PLACEMENTS, STRICT_OSC_CHARS, Terminal, image_lines, prompts::LineMarkKind,
    screen::Screen, strict_http_link,
};

/// Bounded presentation history for a replacement connection in the same pane.
///
/// This contains primary screen rows and scrollback, without the old byte
/// parser, negotiated terminal modes, responses, or unfinished transfers.
pub struct ReconnectHistory(Screen);

impl Terminal {
    /// Captures primary history and archives nonempty visible primary rows.
    /// The active alternate screen is temporary and excluded.
    pub fn reconnect_history(&self) -> ReconnectHistory {
        self.reconnect_history_with_presentation(&[])
    }

    /// Captures opaque presentation marks only on the retained primary rows.
    pub fn reconnect_history_with_presentation(&self, presentation: &[u64]) -> ReconnectHistory {
        ReconnectHistory(self.model.primary.copy_reconnect_history(presentation))
    }

    /// Prepends older styled rows behind the new connection's output.
    ///
    /// Imported rows stay in the scrollback when the live grid grows. The
    /// usual history and image limits prefer newer content. Older prompts
    /// remain navigable, but command output selection belongs to the new
    /// connection. Already-truncated command ranges are invalidated if they
    /// would begin in the earlier session. Strict mode drops imported graphics
    /// and applies the same HTTP(S) and length bounds as incoming OSC 8.
    pub fn prepend_reconnect_history(&mut self, older: ReconnectHistory) {
        self.prepend_reconnect_history_with_presentation(older, &[]);
    }

    /// Returns the allowlisted marks that survived reflow and the replacement's history budget.
    pub fn prepend_reconnect_history_with_presentation(
        &mut self,
        ReconnectHistory(mut older): ReconnectHistory,
        presentation: &[u64],
    ) -> Vec<u64> {
        let model = &mut self.model;
        let graphics_enabled = model.kitty_graphics_enabled();
        let newer_marks = if presentation.is_empty() {
            Vec::new()
        } else {
            model.primary.annotations()
        };
        for line in &mut older.history {
            if model.strict {
                line.filter_hyperlinks(|link| {
                    if link.as_deref().is_some_and(|target| {
                        !strict_http_link(target)
                            || target.encode_utf16().count() > STRICT_OSC_CHARS - "8;;".len()
                    }) {
                        *link = None;
                    }
                });
            }
            if !graphics_enabled {
                line.images.clear();
            }
            line.marks.retain_mut(|mark| {
                if mark.kind.is_timestamp() {
                    return true;
                }
                if mark.kind == LineMarkKind::Annotation {
                    return presentation.contains(&mark.id)
                        && !newer_marks.iter().any(|(id, _)| *id == mark.id);
                }
                if mark.kind != LineMarkKind::Prompt {
                    return false;
                }
                model.prompt_serial = model.prompt_serial.wrapping_add(1);
                mark.id = model.prompt_serial;
                true
            });
        }
        // Use normal width reflow with a separate blank grid, so the old last
        // row cannot join or overwrite the replacement's first row.
        older.scrollback_config = model.primary.scrollback_config;
        older.resize(model.columns, 1);
        let mut placements_left = MAX_IMAGE_PLACEMENTS.saturating_sub(
            image_lines(&model.primary)
                .chain(image_lines(&model.alternate))
                .map(<[_]>::len)
                .sum(),
        );
        let retained = model.primary.prepend_history(older);
        let mut image_rows = 0;
        let mut retained_marks = Vec::new();
        for line in model.primary.history.iter_mut().take(retained).rev() {
            if !presentation.is_empty() {
                retained_marks.extend(
                    line.marks.iter().filter_map(|mark| {
                        (mark.kind == LineMarkKind::Annotation).then_some(mark.id)
                    }),
                );
            }
            line.images.retain(|placement| {
                if placements_left == 0 || !model.kitty.register_history_image(&placement.image) {
                    return false;
                }
                placements_left -= 1;
                image_rows = image_rows.max(usize::from(placement.rows));
                true
            });
        }
        model.primary.update_history_image_height(image_rows);
        retained_marks
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::Arc;

    use crate::{Cell, Color, LastOutput, ScrollbackConfig, Terminal};

    const URL: &str = "https://example.test/reconnect";
    const RED: &[u8] = b"\x1b_Ga=T,i=1,f=24,s=1,v=1,q=2,C=1;/wAA\x1b\\";
    const BLUE: &[u8] = b"\x1b_Ga=T,i=1,f=24,s=1,v=1,q=2,C=1;AAD/\x1b\\";

    fn text(cells: &[Cell]) -> String {
        cells.iter().map(|cell| cell.text.as_str()).collect()
    }

    #[test]
    fn reconnect_retains_packed_styles_links_and_new_history() {
        let mut old = Terminal::new(12, 2);
        old.advance(
            format!("\x1b[31;1m\x1b]8;;{URL}\x1b\\old-styled\x1b]8;;\x1b\\\x1b[0m\r\n").as_bytes(),
        );
        old.advance(b"second\r\nthird\r\nfourth\r\nold-live");
        while old.compress_idle_scrollback(0) {}
        assert!(matches!(old.history_line(0), Some(Cow::Owned(_))));

        let mut next = Terminal::new(12, 2);
        next.advance(b"new-history\r\nnew-screen\r\nnew-prompt");
        let before = next.snapshot();
        next.prepend_reconnect_history(old.reconnect_history());
        let after = next.snapshot();
        assert_eq!(after.cells, before.cells);
        assert_eq!(after.cursor, before.cursor);
        assert_eq!(after.scrollback_lines, 6);
        let Some(Cow::Owned(styled)) = next.history_line(0) else {
            panic!("old row must stay packed at the same width");
        };
        assert_eq!(text(&styled), "old-styled");
        assert_eq!(styled[0].style.foreground, Color::Indexed(1));
        assert!(styled[0].style.bold);
        assert_eq!(styled[0].hyperlink.as_deref(), Some(URL));
        assert_eq!(text(&next.history_line(4).unwrap()), "old-live");
        assert_eq!(text(&next.history_line(5).unwrap()), "new-history");
        assert_eq!(text(&old.snapshot().cells), "fourthold-live");
    }

    #[test]
    fn configured_history_survives_reflow_reconnect_and_reset() {
        let config = ScrollbackConfig {
            lines: 11_000,
            memory_mib: 50,
            compression: false,
        };
        let mut old = Terminal::new(16, 2);
        old.set_scrollback_config(config);
        old.advance(&b"row\r\n".repeat(10_200));
        old.advance(RED);
        let image = old.snapshot().images[0].placement.image.clone();
        old.advance(b"\r\nnext\r\nlive");
        old.resize(14, 2);
        assert_eq!(old.scrollback_config(), config);
        assert!(old.snapshot().scrollback_lines > 10_000);

        let mut next = Terminal::new(12, 2);
        next.set_scrollback_config(config);
        next.prepend_reconnect_history(old.reconnect_history());
        let history = next.snapshot().scrollback_lines;
        assert!(history > 10_000);
        let snapshot = next.scrollback_snapshot();
        let oldest = snapshot.anchor(history).unwrap();
        let retained = snapshot.anchor(3).unwrap();
        let trimmed = ScrollbackConfig {
            lines: 100,
            ..config
        };
        assert!(next.set_scrollback_config(trimmed));
        assert_eq!(next.snapshot().scrollback_lines, 100);
        assert_eq!(next.scrollback_offset(Some(&oldest)), 100);
        assert_eq!(next.scrollback_offset(Some(&retained)), 3);
        assert!((1..=6).any(|offset| {
            next.viewport_snapshot(offset)
                .images
                .iter()
                .any(|placed| Arc::ptr_eq(&placed.placement.image, &image))
        }));

        next.advance(b"\x1bc");
        assert_eq!(next.scrollback_config(), trimmed);
        assert_eq!(next.scrollback_offset(Some(&retained)), 0);
        next.advance(&b"fresh\r\n".repeat(130));
        assert_eq!(next.snapshot().scrollback_lines, 100);
        assert!(!next.compress_idle_scrollback(0));
    }

    #[test]
    fn reconnect_reflows_hidden_primary_without_importing_alternate_or_modes() {
        let mut old = Terminal::new(6, 2);
        old.advance("abc界de".as_bytes());
        old.advance(b"\x1b[?1049h\x1b[?2004h\x1b[?1hALT");
        assert!(old.snapshot().alternate_screen);

        let mut next = Terminal::new(4, 2);
        next.advance(b"new");
        next.prepend_reconnect_history(old.reconnect_history());
        let viewport = next.viewport_snapshot(2);
        assert_eq!(viewport.scrollback_lines, 2);
        assert_eq!(text(viewport.row(0).unwrap()), "abc");
        assert_eq!(viewport.wrapped_columns(0), Some(3));
        assert_eq!(text(viewport.row(1).unwrap()), "界de");
        assert_eq!(viewport.wrapped_columns(1), None);
        let visible = next.snapshot();
        assert_eq!(text(visible.row(0).unwrap()), "new");
        assert!(!visible.alternate_screen);
        assert!(!visible.application_cursor_keys);
        assert!(!next.bracketed_paste());
    }

    #[test]
    fn reconnect_keeps_only_approved_presentation_marks_with_retained_reflowed_rows() {
        let mut old = Terminal::new(8, 4);
        old.advance(b"oldest");
        old.mark_presentation_row(1);
        old.advance(&b"\r\nfill".repeat(104));
        old.advance(b"\r\nabcdef");
        old.mark_presentation_row(2);
        old.advance(b"\r\npending");
        old.mark_presentation_row(3);

        let mut next = Terminal::new(4, 2);
        next.set_scrollback_config(ScrollbackConfig {
            lines: 100,
            ..ScrollbackConfig::default()
        });
        next.advance(b"new");
        let before = next.snapshot();
        let retained = next.prepend_reconnect_history_with_presentation(
            old.reconnect_history_with_presentation(&[1, 2, 3]),
            &[1, 2],
        );
        assert_eq!(retained, [2]);
        let [(id, row)]: [(u64, usize); 1] = next.presentation_rows().try_into().unwrap();
        assert_eq!(id, 2);
        assert_eq!(text(&next.history_line(row).unwrap()), "abcd");
        assert_eq!(text(&next.history_line(row + 1).unwrap()), "ef");
        let after = next.snapshot();
        assert_eq!(after.cells, before.cells);
        assert_eq!(after.cursor, before.cursor);
        assert_eq!(after.scrollback_lines, 100);

        let mut duplicate = Terminal::new(8, 4);
        duplicate.advance(b"replacement");
        duplicate.mark_presentation_row(2);
        assert!(
            duplicate
                .prepend_reconnect_history_with_presentation(
                    old.reconnect_history_with_presentation(&[2]),
                    &[2],
                )
                .is_empty()
        );
        assert_eq!(duplicate.presentation_rows().len(), 1);
        assert_eq!(
            duplicate.presentation_rows()[0].1,
            duplicate.snapshot().scrollback_lines
        );
    }

    #[test]
    fn reconnect_keeps_kitty_pixels_outside_the_new_protocol_namespace() {
        let mut old = Terminal::new(10, 2);
        old.advance(RED);
        let original = old.snapshot().images[0].placement.image.clone();

        let mut next = Terminal::new(10, 2);
        next.prepend_reconnect_history(old.reconnect_history());
        assert!(next.snapshot().images.is_empty());
        let old_view = next.viewport_snapshot(1);
        assert!(Arc::ptr_eq(&old_view.images[0].placement.image, &original));

        next.advance(b"\x1b_Ga=p,i=1\x1b\\");
        assert!(String::from_utf8_lossy(&next.take_responses()).contains("ENOENT"));
        next.advance(BLUE);
        let fresh = next.snapshot().images[0].placement.image.clone();
        assert_ne!(fresh.key, original.key);
        assert_eq!(fresh.id, original.id);
        next.advance(b"\x1b_Ga=d,d=R,x=1,y=1\x1b\\");
        assert!(next.snapshot().images.is_empty());
        let after_erase = next.viewport_snapshot(1);
        assert_eq!(after_erase.images.len(), 1);
        assert!(Arc::ptr_eq(
            &after_erase.images[0].placement.image,
            &original,
        ));
    }

    #[test]
    fn reconnect_applies_strict_link_and_image_rules_to_history() {
        let mut old = Terminal::new(16, 2);
        old.advance(
            format!("\x1b]8;;{URL}\x1b\\web\x1b]8;;file:///scratch\x1b\\file\x1b]8;;\x1b\\")
                .as_bytes(),
        );
        old.advance(RED);
        let mut next = Terminal::new(16, 2);
        next.set_strict_escape_filter(true);
        next.prepend_reconnect_history(old.reconnect_history());
        let history = next.viewport_snapshot(1);
        let row = history.row(0).unwrap();
        assert_eq!(text(row), "webfile");
        assert_eq!(row[0].hyperlink.as_deref(), Some(URL));
        assert!(row[3].hyperlink.is_none());
        assert!(history.images.is_empty());
    }

    #[test]
    fn reconnect_history_budget_prefers_newer_rows_and_placements() {
        let mut old = Terminal::new(10, 16);
        old.advance(b"old");
        old.advance(RED);

        let mut next = Terminal::new(10, 16);
        next.advance(BLUE);
        let placement = next.snapshot().images[0].placement.clone();
        for row in 0..16 {
            next.model.primary.cursor.row = row;
            for _ in 0..crate::kitty::MAX_PLACEMENTS_PER_LINE {
                next.model.primary.place_image(placement.clone());
            }
        }
        next.prepend_reconnect_history(old.reconnect_history());
        assert_eq!(next.snapshot().images.len(), crate::MAX_IMAGE_PLACEMENTS);
        assert!(next.model.primary.history[0].images.is_empty());

        next.advance(b"\x1b[3J");
        for _ in 0..10_016 {
            next.advance(b"new\r\n");
        }
        assert_eq!(next.snapshot().scrollback_lines, 10_000);
        next.prepend_reconnect_history(old.reconnect_history());
        assert_eq!(next.snapshot().scrollback_lines, 10_000);
        assert!(next.search("old", false).matches.is_empty());
    }

    #[test]
    fn reconnect_archive_stays_behind_the_live_grid_when_resizing() {
        let mut old = Terminal::new(12, 2);
        old.set_prompt_navigation_enabled(true);
        old.advance(b"\x1b]133;A\x07old-prompt>");
        old.advance(b"\x1b_Ga=T,i=1,f=24,s=1,v=1,c=1,r=3,q=2,C=1;/wAA\x1b\\");
        let image = old.snapshot().images[0].placement.image.clone();

        let mut next = Terminal::new(12, 2);
        next.set_prompt_navigation_enabled(true);
        next.advance(b"new-history\r\nnew-main\r\nnew-tail");
        next.prepend_reconnect_history(old.reconnect_history());
        assert_eq!(next.snapshot().scrollback_lines, 2);
        assert!(next.snapshot().images.is_empty());

        next.resize(12, 6);
        let live = next.snapshot();
        assert_eq!(live.scrollback_lines, 1);
        assert_eq!(text(live.row(0).unwrap()), "new-history");
        assert_eq!(text(live.row(1).unwrap()), "new-main");
        assert!(live.images.is_empty());
        let archived = next.viewport_snapshot(1);
        assert_eq!(text(archived.row(0).unwrap()), "old-prompt>");
        assert!(Arc::ptr_eq(&archived.images[0].placement.image, &image));

        next.resize(8, 6);
        next.resize(8, 10);
        let live = next.snapshot();
        assert_eq!(live.scrollback_lines, 2);
        assert!(!text(&live.cells).contains("old-"));
        assert!(live.images.is_empty());
        let archived = next.viewport_snapshot(2);
        assert_eq!(text(archived.row(0).unwrap()), "old-prom");
        assert_eq!(text(archived.row(1).unwrap()), "pt>");
        assert_eq!(archived.images[0].row, 1);
        assert_eq!(archived.images[0].placement.column, 3);
        assert!(Arc::ptr_eq(&archived.images[0].placement.image, &image));
        assert_eq!(next.prompt_layout().unwrap().rows, [0]);
    }

    #[test]
    fn reconnect_keeps_prompt_navigation_and_current_command_output_separate() {
        let mut old = Terminal::new(12, 2);
        old.set_prompt_navigation_enabled(true);
        old.advance(b"\x1b]133;A\x07old>\r\n\x1b]133;C\x07unfinished");

        let mut next = Terminal::new(12, 2);
        next.set_prompt_navigation_enabled(true);
        next.advance(b"\x1b]133;A\x07new>\r\n\x1b]133;C\x07finished\x1b]133;D;0\x07");
        next.prepend_reconnect_history(old.reconnect_history());
        assert_eq!(next.prompt_layout().unwrap().rows, [0, 2]);
        let LastOutput::Found(output) = next.last_output() else {
            panic!("the new connection's completed command must remain selectable");
        };
        assert_eq!(next.last_output_text(output).as_deref(), Some("finished"));
    }
}
