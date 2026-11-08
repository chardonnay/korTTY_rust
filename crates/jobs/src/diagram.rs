use kortty_diagram::{
    Diagram, DiagramError, Direction, Edge, EdgeStyle, MAX_LABEL_CHARS, Node, NodeShape, NodeTone,
};

use crate::{DependencyMode, JobId, RunOutcome, Scheduler};

/// Which jobs appear in a dependency diagram.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiagramScope<'a> {
    #[default]
    All,
    Connected,
    /// The selected job, its ancestors and its descendants. A missing focus
    /// uses the connected scope.
    Neighborhood(&'a JobId),
}

impl<A> Scheduler<A> {
    /// Builds the current dependency diagram for the native scene renderer.
    /// Chain edges are solid and scheduled conditions are dashed.
    pub fn diagram(
        &self,
        direction: Direction,
        scope: DiagramScope<'_>,
    ) -> Result<Diagram, DiagramError> {
        let graph = &self.graph;
        let connected = || {
            graph
                .predecessors
                .iter()
                .zip(&graph.successors)
                .map(|(predecessors, successors)| {
                    !predecessors.is_empty() || !successors.is_empty()
                })
                .collect()
        };
        let included: Vec<bool> = match scope {
            DiagramScope::All => vec![true; graph.jobs.len()],
            DiagramScope::Neighborhood(id) => graph.index(id).map_or_else(connected, |index| {
                graph
                    .ancestors(index)
                    .into_iter()
                    .zip(graph.descendants(index))
                    .map(|(a, d)| a || d)
                    .collect()
            }),
            DiagramScope::Connected => connected(),
        };
        let mut diagram = Diagram::new(direction);
        for (index, job) in graph.jobs.iter().enumerate() {
            if !included[index] {
                continue;
            }
            let (status, tone) = if self.is_running(job.id()) {
                ("Running", NodeTone::Active)
            } else {
                match self.last_finished(job.id()).map(|run| run.outcome) {
                    Some(RunOutcome::Success) => ("Successful", NodeTone::Success),
                    Some(RunOutcome::Failed { .. }) => ("Failed", NodeTone::Failure),
                    Some(RunOutcome::Blocked) => ("Blocked", NodeTone::Blocked),
                    Some(RunOutcome::Cancelled) => ("Cancelled", NodeTone::Cancelled),
                    None => ("Never ran", NodeTone::Neutral),
                }
            };
            let (suffix, tone) = if job.enabled() {
                ("", tone)
            } else {
                (" · Disabled", NodeTone::Disabled)
            };
            let name = fit_name(
                job.name(),
                MAX_LABEL_CHARS - 1 - status.chars().count() - suffix.chars().count(),
            );
            let label = format!("{name}\n{status}{suffix}");
            diagram.add_node(
                Node::new(job.id().as_str(), label, NodeShape::Action)?.with_tone(tone),
            )?;
        }
        for (index, job) in graph.jobs.iter().enumerate() {
            if !included[index] {
                continue;
            }
            let (label, style) = match job.mode() {
                DependencyMode::Gate => ("Condition", EdgeStyle::Dashed),
                DependencyMode::Chain => ("Chain", EdgeStyle::Solid),
            };
            for &predecessor in &graph.predecessors[index] {
                if included[predecessor] {
                    let source = graph.jobs[predecessor].id().as_str();
                    diagram
                        .add_edge(Edge::new(source, job.id().as_str(), label)?.with_style(style))?;
                }
            }
        }
        Ok(diagram)
    }
}

fn fit_name(name: &str, limit: usize) -> String {
    if name.chars().count() <= limit {
        return name.to_owned();
    }
    name.chars()
        .take(limit - 1)
        .chain(std::iter::once('…'))
        .collect()
}
