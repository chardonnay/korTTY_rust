use std::fs;

use iced::futures::{StreamExt, executor::block_on};

use crate::connection_store::Store;
use crate::connection_window::{ConnectionWindow, Message as Profile};

use super::super::connection_manager_tests::seed;
use super::*;

fn complete(workspace: &mut Workspace, task: Task<Message>) -> Task<Message> {
    let mut actions = iced_runtime::task::into_stream(task).expect("export task");
    loop {
        if let iced_runtime::Action::Output(message) =
            block_on(actions.next()).expect("export completion")
        {
            return workspace.update(message);
        }
    }
}

#[test]
fn connection_map_pdf_uses_the_reviewed_destination_and_frozen_scope() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    seed(&mut store, "Visible host");
    seed(&mut store, "Other host");
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (manager, _) =
        workspace.install_connection_manager(owner, ConnectionWindow::open(path).unwrap());
    let _ = workspace.update_connections(manager, Profile::ExportMapPdf);
    assert!(workspace.connection_overview_for_owner(manager).is_none());
    let _ = workspace.update_connections(manager, Profile::Search("Visible".into()));
    let _ = workspace.update_connections(manager, Profile::MapView(true));
    let _ = workspace.update_connections(manager, Profile::ExportMapPdf);
    let id = workspace.connection_overview_for_owner(manager).unwrap();
    assert_eq!(workspace.placed_dialog_owner(id), Some(manager));
    assert_eq!(workspace.connection_overview_title(id), Some(MAP_TITLE));
    assert_eq!(workspace.owner_dialog(owner).map(|(id, _)| id), Some(id));

    seed(&mut store, "Visible new host");
    workspace.update_connection_store(store);
    assert_eq!(workspace.connection_overview[&id].count(), 1);
    let update = |workspace: &mut Workspace, event| workspace.update_connection_overview(id, event);
    let _ = update(&mut workspace, Event::MapTitle("Operations map".into()));
    let _ = update(&mut workspace, Event::Landscape(false));
    let _ = update(&mut workspace, Event::ChooseDestination);
    let attempt = workspace.connection_overview[&id].attempt;
    // Point the browser at the private fixture without enumerating the user profile.
    let dialog = workspace.connection_overview.get_mut(&id).unwrap();
    dialog.destination.as_mut().unwrap().browser = Browser::new("kortty-map.pdf".into(), ".pdf");
    let _ = update(
        &mut workspace,
        Event::File(
            attempt,
            FileEvent::Folder(root.to_string_lossy().into_owned()),
        ),
    );
    let list = update(&mut workspace, Event::File(attempt, FileEvent::List));
    let _ = complete(&mut workspace, list);

    let destination = root.join("kortty-map.pdf");
    fs::write(&destination, b"previous file").unwrap();
    let review = update(&mut workspace, Event::File(attempt, FileEvent::Save));
    assert!(workspace.connection_overview[&id].busy());
    let _ = workspace.close_connection_overview(id);
    let _ = complete(&mut workspace, review);
    assert!(matches!(
        workspace.connection_overview[&id]
            .destination
            .as_ref()
            .unwrap()
            .state,
        SaveState::Confirm(_)
    ));
    let _ = update(
        &mut workspace,
        Event::Replace(attempt.wrapping_add(1), true),
    );
    assert_eq!(fs::read(&destination).unwrap(), b"previous file");
    fs::write(&destination, b"replacement made after review").unwrap();
    let publish = update(&mut workspace, Event::Replace(attempt, true));
    let _ = complete(&mut workspace, publish);
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"replacement made after review"
    );
    assert!(workspace.connection_overview[&id].receipt.is_none());

    let review = update(&mut workspace, Event::File(attempt, FileEvent::Save));
    let _ = complete(&mut workspace, review);
    let publish = update(&mut workspace, Event::Replace(attempt, true));
    let _ = complete(&mut workspace, publish);
    let receipt = workspace.connection_overview[&id].receipt.as_ref().unwrap();
    assert_eq!(receipt.count, 1);
    assert!(fs::read(&destination).unwrap().starts_with(b"%PDF-"));
    let _ = workspace.close_connection_overview(id);
    assert!(workspace.connection_overview_for_owner(manager).is_none());
}
