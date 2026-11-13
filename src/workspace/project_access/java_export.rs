use std::io;
use std::path::PathBuf;

use crate::connection_store::Store;
use crate::profile::{self, PaneLayout, WorkspaceLayout};

use super::{Name, export};

pub(super) fn prepare(
    name: &Name,
    mut layout: WorkspaceLayout,
    destination: PathBuf,
    catalogue: Option<PathBuf>,
) -> io::Result<export::Export> {
    let mut profiles = None;
    for pane in layout
        .windows
        .iter_mut()
        .flat_map(|window| &mut window.tabs)
    {
        fill_saved_size(pane, &mut profiles)?;
    }
    export::prepare(name, &layout, destination, catalogue)
}

fn fill_saved_size(pane: &mut PaneLayout, profiles: &mut Option<Store>) -> io::Result<()> {
    match pane {
        PaneLayout::Split { first, second, .. } => {
            fill_saved_size(first, profiles)?;
            fill_saved_size(second, profiles)?;
        }
        PaneLayout::Ssh {
            target,
            term,
            initial_size,
            ..
        } if let Some(id) = target.saved_id()
            && (initial_size.columns.is_none() || initial_size.rows.is_none()) =>
        {
            let profiles = match profiles {
                Some(store) => store,
                None => profiles.insert(Store::open(profile::connections_path()?)?),
            };
            if let Some(connection) = profiles.get(id).filter(|connection| {
                !connection.incognito && target.matches(connection) && connection.emulation == *term
            }) {
                let saved = connection.terminal.initial_size();
                initial_size.columns = initial_size.columns.or(saved.columns);
                initial_size.rows = initial_size.rows.or(saved.rows);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_store::{Connection, TerminalOverrides};
    use crate::profile::InitialTerminalSize;

    #[test]
    fn java_export_uses_saved_size_for_matching_ssh_routes_only() -> io::Result<()> {
        let scratch = crate::snippets::tests::Profile::new();
        let mut store = Store::open(scratch.0.join("connections.json"))?;
        let term = kortty_terminal::TerminalProfile::Xterm.term_name();
        let mut connection: Connection = serde_json::from_value(serde_json::json!({
            "id": 0, "name": "SSH", "tag": "", "jump": null, "tunnels": [],
            "login": {
                "endpoint": {"host": "edge.example", "port": 22, "username": "operator"},
                "auth": "prompt"
            },
            "emulation": term,
        }))?;
        connection.terminal = TerminalOverrides {
            terminal_columns: Some(144),
            terminal_rows: Some(48),
            ..TerminalOverrides::default()
        };
        let id = store.save(connection)?;
        let mut profiles = Some(store);
        for (host, rows) in [("edge.example", Some(48)), ("previous.example", None)] {
            let mut pane = PaneLayout::Ssh {
                font: 14,
                focused: true,
                term: term.into(),
                target: serde_json::from_value(serde_json::json!({
                    "host": host, "port": 22, "username": "operator", "saved": id,
                }))?,
                initial_size: InitialTerminalSize {
                    columns: Some(120),
                    rows: None,
                },
            };
            fill_saved_size(&mut pane, &mut profiles)?;
            let PaneLayout::Ssh { initial_size, .. } = pane else {
                unreachable!()
            };
            assert_eq!(
                initial_size,
                InitialTerminalSize {
                    columns: Some(120),
                    rows
                }
            );
        }
        Ok(())
    }
}
