//! Native cover statistics calculated from the report's exported analysis and apply run.

use std::io;

use crate::session_journal::export::pdf::{Font, HEADING, Layout, PageBlock, Style, WIDTH};

use super::super::{SECTIONS, finding_status, label, section, severity, single_line};
use super::Item;

const BLUE: [u8; 3] = [0x00, 0x66, 0xcc];
const MUTED: [u8; 3] = [0x5e, 0x6e, 0x82];
const TRACK: [u8; 3] = [0xe2, 0xe8, 0xf0];
const DEPENDENCIES: [u8; 3] = [0x0d, 0x94, 0x88];
const BAR_HEIGHT: f32 = 10.0;
const BAR_X: f32 = 120.0;
const BAR_WIDTH: f32 = WIDTH - BAR_X - 38.0;
const SEVERITIES: [(&str, &str, [u8; 3]); 5] = [
    ("critical", "Critical", [0x99, 0x1b, 0x1b]),
    ("high", "High", [0xc0, 0x39, 0x2b]),
    ("medium", "Medium", [0xe6, 0x7e, 0x22]),
    ("low", "Low", [0xb7, 0x95, 0x0b]),
    ("info", "Info", [0x7f, 0x8c, 0x8d]),
];
const RESULTS: [(&str, &str, [u8; 3]); 4] = [
    ("applied", "Applied", [0x15, 0x80, 0x3d]),
    ("rejected", "Rejected", [0xb9, 0x1c, 0x1c]),
    ("unconfirmed", "Unconfirmed", [0x6b, 0x72, 0x80]),
    ("proposed", "Proposed", [0x1d, 0x4e, 0xd8]),
];

pub(super) fn draw(document: &mut Layout<'_>, item: &Item) -> io::Result<()> {
    let analysis = &item.record.analysis;
    let mut counts = [[0; SEVERITIES.len()]; SECTIONS.len()];
    for finding in &analysis.improvements {
        let category = section(&finding.category);
        let row = SECTIONS
            .iter()
            .position(|entry| entry.0 == category)
            .unwrap_or(2);
        let severity = severity(&finding.severity);
        let column = SEVERITIES
            .iter()
            .position(|entry| entry.0 == severity)
            .unwrap_or(4);
        counts[row][column] += 1;
    }
    let critical_high: usize = counts.iter().map(|row| row[0] + row[1]).sum();
    let selected = analysis
        .improvements
        .iter()
        .map(|item| item.id.as_str())
        .chain(analysis.dependencies.iter().map(|item| item.id.as_str()))
        .filter(|id| item.selected(id));
    let selected_count = selected.clone().count();
    let results = item.post_run().map(|run| {
        let mut results = [0; RESULTS.len()];
        for id in selected {
            let status = finding_status(run, id);
            if let Some(index) = RESULTS.iter().position(|entry| entry.0 == status) {
                results[index] += 1;
            }
        }
        results
    });
    let dependencies = analysis.dependencies.len();
    let is_post = results.is_some();
    let mut block = document.vector_block(if is_post { 359.0 } else { 291.0 })?;
    block.rect([0.0, 0.0, WIDTH, 91.0], BLUE)?;
    block.label(
        [18.0, 24.0],
        WIDTH - 36.0,
        Style::new(Font::Bold, 9.5, [0xd9, 0xea, 0xff]),
        if is_post {
            "AFTER APPLYING CHANGES"
        } else {
            "BEFORE APPLYING CHANGES"
        },
    )?;
    block.label(
        [18.0, 55.0],
        WIDTH - 36.0,
        Style::new(Font::Bold, 24.0, [255; 3]),
        &single_line(&label(item)),
    )?;
    block.label(
        [18.0, 76.0],
        WIDTH - 36.0,
        Style::new(Font::Regular, 11.0, [0xd9, 0xea, 0xff]),
        "Code analysis report",
    )?;
    let fourth = results.map_or(("Dependencies", dependencies, DEPENDENCIES), |results| {
        ("Applied", results[0], RESULTS[0].2)
    });
    tiles(
        &mut block,
        [
            ("Findings", analysis.improvements.len(), HEADING),
            (
                "Critical / high",
                critical_high,
                if critical_high == 0 {
                    HEADING
                } else {
                    SEVERITIES[1].2
                },
            ),
            ("Selected", selected_count, BLUE),
            fourth,
        ],
    )?;
    severity_chart(&mut block, counts, dependencies)?;
    if let Some(results) = results.filter(|_| selected_count > 0) {
        result_chart(&mut block, results)?;
    }
    Ok(())
}

fn tiles(block: &mut PageBlock<'_>, stats: [(&str, usize, [u8; 3]); 4]) -> io::Result<()> {
    const GAP: f32 = 10.0;
    let width = (WIDTH - 3.0 * GAP) / 4.0;
    for (index, (label, count, color)) in stats.into_iter().enumerate() {
        let x = index as f32 * (width + GAP);
        block.rect([x, 105.0, width, 50.0], [0xf8, 0xfa, 0xfc])?;
        block.rect([x + 10.0, 111.0, 22.0, 3.0], color)?;
        block.label(
            [x + 10.0, 134.0],
            width - 20.0,
            Style::new(Font::Bold, 19.0, color),
            &count.to_string(),
        )?;
        block.label(
            [x + 10.0, 146.0],
            width - 20.0,
            Style::new(Font::Regular, 8.4, MUTED),
            label,
        )?;
    }
    Ok(())
}

fn severity_chart(
    block: &mut PageBlock<'_>,
    counts: [[usize; SEVERITIES.len()]; SECTIONS.len()],
    dependencies: usize,
) -> io::Result<()> {
    let title = Style::new(Font::Bold, 11.0, HEADING);
    block.label(
        [0.0, 174.0],
        WIDTH,
        title,
        "Findings by category and severity",
    )?;
    let totals = counts.map(|row| row.into_iter().sum::<usize>());
    let max = totals.into_iter().fold(dependencies, usize::max);
    let colors = SEVERITIES.map(|entry| entry.2);
    for (index, (_, label)) in SECTIONS.into_iter().enumerate() {
        let top = chart_row(block, index, label, totals[index])?;
        stacked_bar(
            block,
            [BAR_X, top, BAR_WIDTH],
            max,
            counts[index].into_iter().zip(colors),
        )?;
    }
    let top = chart_row(block, SECTIONS.len(), "Dependencies", dependencies)?;
    stacked_bar(
        block,
        [BAR_X, top, BAR_WIDTH],
        max,
        [(dependencies, DEPENDENCIES)],
    )?;
    for (index, (_, label, color)) in SEVERITIES.into_iter().enumerate() {
        legend(block, index as f32 * 82.0, 268.0, label, color)?;
    }
    Ok(())
}

fn chart_row(
    block: &mut PageBlock<'_>,
    index: usize,
    label: &str,
    count: usize,
) -> io::Result<f32> {
    let top = 185.0 + index as f32 * 18.0;
    let style = Style::new(Font::Regular, 9.4, HEADING);
    block.label([0.0, top + 8.0], BAR_X - 8.0, style, label)?;
    block.label(
        [BAR_X + BAR_WIDTH + 8.0, top + 8.0],
        30.0,
        style,
        &count.to_string(),
    )?;
    Ok(top)
}

fn result_chart(block: &mut PageBlock<'_>, results: [usize; RESULTS.len()]) -> io::Result<()> {
    let title = Style::new(Font::Bold, 11.0, HEADING);
    block.label(
        [0.0, 299.0],
        WIDTH,
        title,
        "Apply results for selected items",
    )?;
    let total = results.into_iter().sum();
    let colors = RESULTS.map(|entry| entry.2);
    stacked_bar(
        block,
        [0.0, 310.0, WIDTH],
        total,
        results.into_iter().zip(colors),
    )?;
    for (index, (count, (_, label, color))) in results
        .into_iter()
        .zip(RESULTS)
        .filter(|(count, _)| *count > 0)
        .enumerate()
    {
        legend(
            block,
            index as f32 * 116.0,
            329.0,
            &format!("{label} ({count})"),
            color,
        )?;
    }
    Ok(())
}

fn stacked_bar(
    block: &mut PageBlock<'_>,
    [left, top, width]: [f32; 3],
    max: usize,
    segments: impl IntoIterator<Item = (usize, [u8; 3])>,
) -> io::Result<()> {
    block.rect([left, top, width, BAR_HEIGHT], TRACK)?;
    let mut start = 0;
    let scale = width / max.max(1) as f32;
    for (count, color) in segments {
        if count > 0 {
            block.rect(
                [
                    left + start as f32 * scale,
                    top,
                    count as f32 * scale,
                    BAR_HEIGHT,
                ],
                color,
            )?;
            start += count;
        }
    }
    Ok(())
}

fn legend(
    block: &mut PageBlock<'_>,
    x: f32,
    y: f32,
    label: &str,
    color: [u8; 3],
) -> io::Result<()> {
    block.rect([x, y, 8.0, 8.0], color)?;
    block.label(
        [x + 11.0, y + 7.0],
        WIDTH - x - 11.0,
        Style::new(Font::Regular, 8.2, MUTED),
        label,
    )
}
