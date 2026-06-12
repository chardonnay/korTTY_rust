//! Removes SGR color attributes while preserving other terminal control sequences.
//!
//! Port of the Java `TerminalColorControlSequenceFilter`. The filter is stateful:
//! a CSI sequence that is split across read chunks is buffered in `pending` and
//! re-processed together with the next chunk. Only color parameters are removed
//! from `CSI ... m` (SGR) sequences — simple foreground/background colors
//! (30-37, 39, 40-47, 49, 90-97, 100-107), indexed colors (`38;5;n` / `48;5;n`)
//! and true colors (`38;2;r;g;b` / `48;2;r;g;b`, including the colon-separated
//! form `38:...` / `48:...`). All other SGR parameters (bold, underline, reset,
//! ...) and all non-SGR control sequences pass through unchanged.

const ESC: u8 = 0x1B;

/// Stateful SGR color stripper for a terminal output stream.
#[derive(Debug, Clone, Default)]
pub struct TerminalColorFilter {
    pending: Vec<u8>,
}

impl TerminalColorFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Filters one output chunk. Incomplete trailing CSI sequences are kept in an
    /// internal pending buffer and prepended to the next chunk.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        if input.is_empty() {
            return Vec::new();
        }

        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(input);
        let mut output = Vec::with_capacity(data.len());

        let mut index = 0;
        while index < data.len() {
            let current = data[index];
            if current != ESC {
                output.push(current);
                index += 1;
                continue;
            }

            if index + 1 >= data.len() {
                self.pending = data[index..].to_vec();
                break;
            }

            if data[index + 1] != b'[' {
                output.push(current);
                index += 1;
                continue;
            }

            let Some(final_index) = find_csi_final(&data, index + 2) else {
                self.pending = data[index..].to_vec();
                break;
            };

            let final_byte = data[final_index];
            if final_byte == b'm' {
                let params = String::from_utf8_lossy(&data[index + 2..final_index]);
                if let Some(rewritten) = strip_color_sgr_params(&params) {
                    output.push(ESC);
                    output.push(b'[');
                    output.extend_from_slice(rewritten.as_bytes());
                    output.push(b'm');
                }
            } else {
                output.extend_from_slice(&data[index..=final_index]);
            }
            index = final_index + 1;
        }

        output
    }

    /// Drops any buffered partial sequence (e.g. on reconnect).
    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

fn find_csi_final(data: &[u8], start: usize) -> Option<usize> {
    (start..data.len()).find(|&index| (0x40..=0x7E).contains(&data[index]))
}

/// Rewrites an SGR parameter list without color parameters.
///
/// Returns `Some(params)` with the (possibly unchanged) parameter list to emit,
/// or `None` when every parameter was a color parameter and the whole sequence
/// should be dropped (Java returns `null` in that case).
fn strip_color_sgr_params(params: &str) -> Option<String> {
    if params.is_empty() {
        return Some(String::new());
    }

    let parts: Vec<&str> = params.split(';').collect();
    let mut kept: Vec<&str> = Vec::with_capacity(parts.len());
    let mut removed_color = false;

    let mut index = 0;
    while index < parts.len() {
        let part = parts[index];
        let code = parse_integer(part);
        let Some(code) = code else {
            if is_colon_separated_color_part(part) {
                removed_color = true;
            } else {
                kept.push(part);
            }
            index += 1;
            continue;
        };

        if is_simple_color_code(code) {
            removed_color = true;
            index += 1;
            continue;
        }

        if code == 38 || code == 48 {
            removed_color = true;
            index += extended_color_part_count(&parts, index);
            continue;
        }

        kept.push(part);
        index += 1;
    }

    if !removed_color {
        return Some(params.to_string());
    }
    if kept.is_empty() {
        None
    } else {
        Some(kept.join(";"))
    }
}

fn is_simple_color_code(code: i32) -> bool {
    (30..=37).contains(&code)
        || code == 39
        || (40..=47).contains(&code)
        || code == 49
        || (90..=97).contains(&code)
        || (100..=107).contains(&code)
}

fn extended_color_part_count(parts: &[&str], color_code_index: usize) -> usize {
    if color_code_index + 1 >= parts.len() {
        return 1;
    }
    match parse_integer(parts[color_code_index + 1]) {
        Some(2) => std::cmp::min(5, parts.len() - color_code_index),
        Some(5) => std::cmp::min(3, parts.len() - color_code_index),
        _ => 1,
    }
}

fn is_colon_separated_color_part(part: &str) -> bool {
    part.starts_with("38:") || part.starts_with("48:")
}

fn parse_integer(value: &str) -> Option<i32> {
    if value.is_empty() {
        return None;
    }
    value.parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_str(filter: &mut TerminalColorFilter, input: &str) -> String {
        String::from_utf8(filter.filter(input.as_bytes())).expect("filtered output is UTF-8")
    }

    #[test]
    fn strips_ansi_color_codes_but_keeps_text_and_reset() {
        let mut filter = TerminalColorFilter::new();

        let result = filter_str(
            &mut filter,
            "a\u{1b}[31mred\u{1b}[0m b\u{1b}[44mblue-bg\u{1b}[39m",
        );

        assert_eq!(result, "ared\u{1b}[0m bblue-bg");
    }

    #[test]
    fn preserves_non_color_sgr_attributes() {
        let mut filter = TerminalColorFilter::new();

        let result = filter_str(&mut filter, "\u{1b}[1;31;4mstrong\u{1b}[22;24m");

        assert_eq!(result, "\u{1b}[1;4mstrong\u{1b}[22;24m");
    }

    #[test]
    fn strips_indexed_and_true_color_sequences() {
        let mut filter = TerminalColorFilter::new();

        let result = filter_str(&mut filter, "\u{1b}[38;2;255;0;0;48;5;4;1mtext");

        assert_eq!(result, "\u{1b}[1mtext");
    }

    #[test]
    fn handles_split_control_sequences_across_reads() {
        let mut filter = TerminalColorFilter::new();

        assert!(filter_str(&mut filter, "\u{1b}[3").is_empty());
        assert_eq!(filter_str(&mut filter, "1mred"), "red");
    }

    #[test]
    fn leaves_non_sgr_control_sequences_untouched() {
        let mut filter = TerminalColorFilter::new();

        let result = filter_str(&mut filter, "a\u{1b}[2Jb");

        assert_eq!(result, "a\u{1b}[2Jb");
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let mut filter = TerminalColorFilter::new();
        assert!(filter.filter(&[]).is_empty());
    }

    #[test]
    fn reset_drops_buffered_partial_sequence() {
        let mut filter = TerminalColorFilter::new();
        assert!(filter_str(&mut filter, "\u{1b}[3").is_empty());

        filter.reset();

        assert_eq!(filter_str(&mut filter, "1mred"), "1mred");
    }

    #[test]
    fn keeps_bare_sgr_reset_without_parameters() {
        let mut filter = TerminalColorFilter::new();
        assert_eq!(filter_str(&mut filter, "\u{1b}[mtext"), "\u{1b}[mtext");
    }

    #[test]
    fn strips_colon_separated_true_color_part() {
        let mut filter = TerminalColorFilter::new();
        assert_eq!(
            filter_str(&mut filter, "\u{1b}[38:2:255:0:0;1mtext"),
            "\u{1b}[1mtext"
        );
    }

    #[test]
    fn trailing_lone_escape_is_buffered_until_next_chunk() {
        let mut filter = TerminalColorFilter::new();
        assert_eq!(filter_str(&mut filter, "ab\u{1b}"), "ab");
        assert_eq!(filter_str(&mut filter, "[31mred"), "red");
    }
}
