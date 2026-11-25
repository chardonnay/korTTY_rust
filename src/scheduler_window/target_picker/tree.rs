use super::*;

pub(crate) struct Row {
    pub(crate) key: String,
    pub(crate) choice: Option<Choice>,
    pub(crate) label: String,
    pub(crate) detail: Option<String>,
    pub(crate) depth: usize,
    pub(crate) branch: bool,
    pub(crate) count: Option<usize>,
}

#[derive(Default)]
struct Node {
    choice: Option<Choice>,
    children: BTreeMap<String, Node>,
    connections: Vec<Choice>,
}

impl Node {
    fn count(&self) -> usize {
        self.connections.len() + self.children.values().map(Self::count).sum::<usize>()
    }
}

fn group_key(source: &str, path: &str) -> String {
    format!(
        "scheduler-group-{:x}",
        Sha256::digest(format!("{source}\0{path}").as_bytes())
    )
}

pub(super) fn rows(
    picker: &Picker,
    connections: &[ConnectionChoice],
    groups: &[ConnectionGroup],
    paths: &BTreeMap<u64, String>,
) -> Vec<Row> {
    let mut roots = BTreeMap::<String, Node>::new();
    for (choice, _) in picker.matching(connections, groups, paths) {
        let (source, path) = match &choice {
            Choice::Group(group) => {
                let (source, path) = group.picker_location();
                (source, path.to_owned())
            }
            Choice::Connection(ConnectionChoice::Saved { reference, .. }) => (
                reference
                    .shared_identity()
                    .map(|identity| identity.source().to_string())
                    .unwrap_or_default(),
                paths.get(&reference.id).cloned().unwrap_or_default(),
            ),
            _ => continue,
        };
        let mut node = roots.entry(source).or_default();
        for part in path.split('/').filter(|part| !part.is_empty()) {
            node = node.children.entry(part.to_owned()).or_default();
        }
        match choice {
            Choice::Group(_) => node.choice = Some(choice),
            _ => node.connections.push(choice),
        }
    }
    // Filtering keeps matching leaves in context without losing the identity
    // of their selectable ancestor groups.
    for group in groups {
        let (source, path) = group.picker_location();
        let mut node = roots.get_mut(&source);
        for part in path.split('/').filter(|part| !part.is_empty()) {
            node = node.and_then(|node| node.children.get_mut(part));
        }
        if let Some(node) = node {
            node.choice = Some(Choice::Group(group.clone()));
        }
    }
    let mut result = Vec::new();
    for (source, root) in roots {
        let depth = if source.is_empty() {
            0
        } else {
            let key = group_key(&source, "");
            result.push(Row {
                key: key.clone(),
                choice: None,
                label: format!("Teamwork · {source}"),
                detail: None,
                depth: 0,
                branch: true,
                count: Some(root.count()),
            });
            if picker.query.trim().is_empty() && picker.collapsed.contains(&key) {
                continue;
            }
            1
        };
        flatten(picker, &source, "", root, depth, &mut result);
    }
    result
}

fn flatten(
    picker: &Picker,
    source: &str,
    path: &str,
    node: Node,
    depth: usize,
    rows: &mut Vec<Row>,
) {
    let mut children: Vec<_> = node.children.into_iter().collect();
    children.sort_by_cached_key(|(name, _)| name.to_lowercase());
    for (name, child) in children {
        let path = if path.is_empty() {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        let key = child
            .choice
            .as_ref()
            .map(Choice::key)
            .unwrap_or_else(|| group_key(source, &path));
        rows.push(Row {
            key: key.clone(),
            choice: child.choice.clone(),
            label: name,
            detail: None,
            depth,
            branch: true,
            count: Some(child.count()),
        });
        if !picker.query.trim().is_empty() || !picker.collapsed.contains(&key) {
            flatten(picker, source, &path, child, depth + 1, rows);
        }
    }
    for choice in node.connections {
        let Choice::Connection(ConnectionChoice::Saved { name, reference }) = &choice else {
            continue;
        };
        let endpoint = reference.target.label();
        let label = match &choice {
            Choice::Connection(connection) => connection.to_string(),
            Choice::Group(_) => unreachable!(),
        };
        let prefix = format!("{name} ({endpoint})");
        let detail = format!(
            "{endpoint}{}",
            label.strip_prefix(&prefix).unwrap_or_default()
        );
        rows.push(Row {
            key: choice.key(),
            choice: Some(choice.clone()),
            label: name.clone(),
            detail: Some(detail),
            depth,
            branch: false,
            count: None,
        });
    }
}
