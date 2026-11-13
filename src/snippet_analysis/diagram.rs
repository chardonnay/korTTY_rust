//! Saved native flowcharts are bound to the immutable analysis source.

use std::collections::HashMap;

use kortty_diagram::{
    Diagram, DiagramKind, Direction, MAX_NODES, MAX_SOURCE_BYTES, SourceRange, Theme, parse_mermaid,
};
use serde::{Deserialize, Serialize};

use crate::diagram_window::Chart;

use super::model::now_millis;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnalysisDiagram {
    pub(crate) mermaid: String,
    pub(crate) fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fallback_reason: Option<String>,
    generated_at: u64,
    references: Vec<CodeReference>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeReference {
    node: usize,
    start_line: u32,
    end_line: u32,
}

impl AnalysisDiagram {
    pub(crate) const FALLBACK_NOTICE: &str =
        "AI diagram unavailable. Showing the locally generated flowchart.";

    pub(crate) fn capture(chart: &Chart, line_count: usize) -> Result<Self, String> {
        let mermaid = chart.diagram().to_mermaid();
        let references = chart
            .diagram()
            .nodes()
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let range = chart.source_range(node)?;
                (range.end() as usize <= line_count).then_some(CodeReference {
                    node: index,
                    start_line: range.start(),
                    end_line: range.end(),
                })
            })
            .collect();
        let saved = Self {
            mermaid,
            fallback: chart.is_fallback(),
            fallback_reason: chart.fallback_reason().map(str::to_owned),
            generated_at: now_millis(),
            references,
        };
        saved.restore(line_count)?;
        Ok(saved)
    }

    pub(crate) fn parse(&self) -> Result<Diagram, String> {
        if self.mermaid.len() > MAX_SOURCE_BYTES
            || self.references.len() > MAX_NODES
            || self
                .fallback_reason
                .as_ref()
                .is_some_and(|reason| reason.len() > 4096)
        {
            return Err("The saved flowchart exceeds its size limit.".into());
        }
        let diagram = parse_mermaid(&self.mermaid).map_err(|error| error.to_string())?;
        if diagram.kind() != DiagramKind::Flowchart || diagram.edges().is_empty() {
            return Err("The saved diagram is not a connected flowchart.".into());
        }
        Ok(diagram)
    }

    pub(crate) fn chart(&self, line_count: usize, source_retained: bool) -> Result<Chart, String> {
        let (diagram, mut links) = self.restore(line_count)?;
        if !source_retained {
            links.clear();
        }
        Ok(Chart::stored(
            diagram,
            links,
            self.fallback,
            self.fallback_reason.clone(),
        ))
    }

    pub(crate) fn report_diagram(&self) -> Result<Diagram, String> {
        let mut diagram = self.parse()?;
        diagram.set_direction(Direction::TopToBottom);
        Ok(diagram)
    }

    pub(crate) fn report_png(&self) -> Result<Vec<u8>, String> {
        self.report_diagram()?
            .layout()
            .to_png(Theme::Light, 2)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn validate(&self, line_count: usize) -> Result<(), String> {
        self.restore(line_count).map(|_| ())
    }

    fn restore(
        &self,
        line_count: usize,
    ) -> Result<(Diagram, HashMap<String, SourceRange>), String> {
        let diagram = self.parse()?;
        let mut links = HashMap::with_capacity(self.references.len());
        for link in &self.references {
            let range = SourceRange::new(link.start_line, link.end_line)
                .filter(|range| range.end() as usize <= line_count)
                .ok_or("The saved flowchart has an invalid source range.")?;
            let node = diagram
                .nodes()
                .get(link.node)
                .ok_or("The saved flowchart references an unknown node.")?;
            if links.insert(node.id().into(), range).is_some() {
                return Err("The saved flowchart contains duplicate source references.".into());
            }
        }
        Ok((diagram, links))
    }
}
