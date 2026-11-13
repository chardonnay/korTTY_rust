use std::io;

use crate::profile;
use crate::snippet_analysis::model::hardening::InputOption;
use crate::workspace::tab_titles::WindowSettings;

pub(super) fn save<T: Eq>(
    live: &mut WindowSettings,
    desired: T,
    get: impl Fn(&WindowSettings) -> T,
    set: impl Fn(&mut WindowSettings, T),
) -> Result<(), String> {
    let path =
        profile::window_settings_path().map_err(|_| "Could not locate the saved settings.")?;
    let mut persisted = match WindowSettings::read(&path) {
        Ok(settings) => settings,
        Err(error) if error.kind() == io::ErrorKind::NotFound => live.clone(),
        Err(_) => return Err("Could not read the saved settings.".into()),
    };
    let saved = get(&persisted);
    if saved != desired {
        if saved != get(live) {
            return Err("This preference changed in another instance.".into());
        }
        set(&mut persisted, desired);
        persisted
            .save(&path)
            .map_err(|_| "Could not save the workflow preference.")?;
    }
    set(live, get(&persisted));
    Ok(())
}

pub(super) fn input(settings: &WindowSettings) -> (bool, Vec<InputOption>, u16) {
    let hardening = &settings.snippet_hardening;
    (
        hardening.input_enabled,
        hardening.input.clone(),
        hardening.max_file_size_mib,
    )
}
