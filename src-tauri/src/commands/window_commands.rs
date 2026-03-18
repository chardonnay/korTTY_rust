use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Stores pending tab transfer payloads for windows that may not have registered the event listener yet.
pub struct PendingTransferStore(pub Mutex<HashMap<String, String>>);

#[tauri::command]
pub fn store_pending_transfer(
    app: AppHandle,
    target_label: String,
    payload_json: String,
) -> Result<(), String> {
    let store = app
        .try_state::<PendingTransferStore>()
        .ok_or_else(|| "PendingTransferStore not initialized".to_string())?;
    store
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(target_label, payload_json);
    Ok(())
}

#[tauri::command]
pub fn take_pending_transfer(
    app: AppHandle,
    window_label: String,
) -> Result<Option<String>, String> {
    let store = app
        .try_state::<PendingTransferStore>()
        .ok_or_else(|| "PendingTransferStore not initialized".to_string())?;
    let payload = store
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&window_label);
    Ok(payload)
}

#[tauri::command]
pub fn create_workspace_window(app: AppHandle, label: String, title: String) -> Result<(), String> {
    if app.get_webview_window(&label).is_some() {
        return Ok(());
    }

    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
        .title(title)
        .inner_size(1200.0, 800.0)
        .min_inner_size(800.0, 600.0)
        .center()
        .resizable(true)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
