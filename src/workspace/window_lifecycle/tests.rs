use super::*;

#[test]
fn standalone_startup_uses_fixed_then_remembered_geometry_and_dashboard_independently() {
    let mut lifecycle = Lifecycle::default();
    let previous = WindowPlacement {
        width: 870.0,
        height: 525.0,
        position: SavedPosition::new(25.0, 35.0),
    };
    lifecycle.last.set_placement(previous, true);
    lifecycle.last.dashboard_visible = true;
    let mut preferences = StartupPreferences::default();
    assert_eq!(lifecycle.startup(preferences), (previous, true, true));

    preferences.use_fixed_geometry = true;
    assert_eq!(
        lifecycle.startup(preferences),
        (preferences.fixed.placement(), false, true)
    );

    preferences.use_fixed_geometry = false;
    preferences.remember_geometry = false;
    preferences.remember_dashboard = false;
    let (placement, maximized, dashboard) = lifecycle.startup(preferences);
    assert_eq!(
        (placement.width, placement.height),
        (DEFAULT_WINDOW.width, DEFAULT_WINDOW.height)
    );
    assert_eq!(placement.position, None);
    assert!(!maximized && !dashboard);
}

#[test]
fn standalone_close_remembers_dashboard_without_replacing_bounds_in_fixed_or_fullscreen_mode() {
    let mut workspace = Workspace::default();
    let (id, _) = workspace.open_window(false);
    let _ = workspace.set_dashboard_visible(id, true);
    let window = workspace.windows.get_mut(&id).unwrap();
    let prior = WindowPlacement {
        width: 930.0,
        height: 680.0,
        position: None,
    };
    workspace.lifecycle.last.set_placement(prior, true);

    window.geometry.recordable = true;
    let preferences = StartupPreferences {
        use_fixed_geometry: true,
        ..StartupPreferences::default()
    };
    workspace.lifecycle.capture(window, preferences, true);
    assert_eq!(workspace.lifecycle.last.placement(), Some(prior));
    assert!(workspace.lifecycle.last.maximized);
    assert!(workspace.lifecycle.last.dashboard_visible);

    let revision = window
        .geometry
        .observe(Some(Size::new(1440.0, 900.0)), None)
        .unwrap();
    let mode = kortty_windowing::WindowMode {
        fullscreen: true,
        ..Default::default()
    };
    assert_eq!(window.geometry.confirm(revision, None, mode), None);
    workspace
        .lifecycle
        .capture(window, StartupPreferences::default(), true);
    assert_eq!(workspace.lifecycle.last.placement(), Some(prior));
}

#[test]
fn saved_window_geometry_does_not_replace_global_last_window_bounds() {
    let mut workspace = Workspace::default();
    let (id, _) = workspace.open_window(false);
    let prior = WindowPlacement {
        width: 930.0,
        height: 680.0,
        position: None,
    };
    workspace.lifecycle.last.set_placement(prior, true);

    let window = workspace.windows.get_mut(&id).unwrap();
    window.geometry.recordable = true;
    window.geometry.mark_connection_specific();
    workspace
        .lifecycle
        .capture(window, StartupPreferences::default(), false);
    workspace
        .lifecycle
        .capture(window, StartupPreferences::default(), true);
    assert_eq!(workspace.lifecycle.last.placement(), Some(prior));
    assert!(workspace.lifecycle.last.maximized);
}

const PRIMARY: WorkArea = WorkArea {
    origin: Point::new(0.0, 24.0),
    size: Size::new(1440.0, 852.0),
};
const SECONDARY: WorkArea = WorkArea {
    origin: Point::new(-1280.0, -90.0),
    size: Size::new(1280.0, 990.0),
};

#[test]
fn dialog_defaults_fit_a_small_work_area_before_the_first_open() {
    let minimum = Size::new(1000.0, 700.0);
    let mut settings = window::Settings {
        size: Size::new(1120.0, 960.0),
        min_size: Some(minimum),
        position: window::Position::Centered,
        ..window::Settings::default()
    };
    let screen = WorkArea {
        origin: Point::new(0.0, 24.0),
        size: Size::new(900.0, 660.0),
    };
    fit_dialog_on_screens(&mut settings, minimum, None, &[screen]);
    assert_eq!(settings.size, Size::new(900.0, 628.0));
    assert_eq!(settings.min_size, Some(settings.size));
    assert!(matches!(settings.position, window::Position::Centered));
}

#[test]
fn restores_coordinates_on_the_same_monitor_and_keeps_the_title_bar_reachable() {
    let window = Size::new(900.0, 650.0);
    let screens = [PRIMARY, SECONDARY];
    let saved = SavedPosition::new(-1000.0, -20.0).unwrap();
    assert_eq!(
        place_on_screens(window, MIN_SIZE, saved, &screens),
        Some((window, Point::new(-1000.0, -20.0)))
    );
    let near_edge = SavedPosition::new(-5.0, 890.0).unwrap();
    assert_eq!(
        place_on_screens(window, MIN_SIZE, near_edge, &screens),
        Some((window, Point::new(-900.0, 218.0)))
    );
}

#[test]
fn removed_monitors_recenter_on_the_primary_work_area() {
    let saved = SavedPosition::new(-5000.0, 190.0).unwrap();
    assert_eq!(
        place_on_screens(Size::new(800.0, 600.0), MIN_SIZE, saved, &[PRIMARY]),
        Some((Size::new(800.0, 600.0), Point::new(320.0, 134.0)))
    );
    assert_eq!(
        place_on_screens(Size::new(1800.0, 1000.0), MIN_SIZE, saved, &[PRIMARY]),
        Some((Size::new(1440.0, 820.0), Point::new(0.0, 24.0)))
    );
    assert_eq!(place_on_screens(Size::ZERO, MIN_SIZE, saved, &[]), None);
}

#[test]
fn journal_dialog_geometry_uses_the_dialog_minimum_on_the_remaining_monitor() {
    let remaining = WorkArea {
        origin: Point::new(0.0, 24.0),
        size: Size::new(510.0, 370.0),
    };
    let disconnected = SavedPosition::new(-1400.0, 200.0).unwrap();
    assert_eq!(
        place_on_screens(
            Size::new(1000.0, 780.0),
            Size::new(360.0, 300.0),
            disconnected,
            &[remaining]
        ),
        Some((Size::new(510.0, 338.0), Point::new(0.0, 24.0)))
    );
}

#[test]
fn converts_signed_logical_coordinates_using_the_capture_scale() {
    let saved = SavedPosition::new(-640.5, 53.3).unwrap();
    assert_eq!(saved.physical_pixels(1.5), Some((-961, 80)));
    assert_eq!(saved.physical_pixels(f32::MAX), None);
    assert_eq!(saved.physical_pixels(0.0), None);
}

#[test]
fn fullscreen_toggle_uses_the_native_mode() {
    assert_eq!(
        fullscreen_target(window::Mode::Windowed),
        Some(window::Mode::Fullscreen)
    );
    assert_eq!(
        fullscreen_target(window::Mode::Fullscreen),
        Some(window::Mode::Windowed)
    );
    assert_eq!(fullscreen_target(window::Mode::Hidden), None);
}

fn normal_mode() -> kortty_windowing::WindowMode {
    kortty_windowing::WindowMode {
        windowed: true,
        normal_geometry: true,
        maximized: Some(false),
        ..kortty_windowing::WindowMode::default()
    }
}

#[test]
fn native_fullscreen_bounds_do_not_replace_normal_geometry() {
    let size = Size::new(940.0, 620.0);
    let initial_position = SavedPosition::new(60.0, 100.0);
    let mut geometry = Geometry::new(size, initial_position);
    let full_size = Size::new(1920.0, 1080.0);
    let revision = geometry
        .observe(Some(full_size), Some(Some(Point::ORIGIN)))
        .unwrap();
    let fullscreen = kortty_windowing::WindowMode {
        fullscreen: true,
        ..kortty_windowing::WindowMode::default()
    };
    assert_eq!(geometry.confirm(revision, None, fullscreen), None);
    assert!(geometry.fullscreen);
    assert_eq!(geometry.normal.width, size.width);
    assert_eq!(geometry.normal.height, size.height);
    assert_eq!(geometry.normal.position, initial_position);

    let new_size = Size::new(830.0, 570.0);
    let new_position = Point::new(300.0, 70.0);
    let revision = geometry
        .observe(Some(new_size), Some(Some(new_position)))
        .unwrap();
    assert_eq!(geometry.confirm(revision, Some(1.5), normal_mode()), None);
    assert!(!geometry.fullscreen);
    assert_eq!(geometry.normal.width, new_size.width);
    assert_eq!(geometry.normal.height, new_size.height);
    assert_eq!(geometry.normal.position, saved_position(new_position, 1.5));
}

#[test]
fn an_inflight_fullscreen_probe_does_not_leak_bounds_after_exit() {
    let size = Size::new(940.0, 620.0);
    let mut geometry = Geometry::new(size, None);
    let revision = geometry
        .observe(Some(Size::new(1920.0, 1080.0)), None)
        .unwrap();
    let windowed_position = Point::new(60.0, 100.0);
    assert_eq!(geometry.observe(None, Some(Some(windowed_position))), None);
    let latest = geometry
        .confirm(revision, None, kortty_windowing::WindowMode::default())
        .unwrap();
    assert_eq!(geometry.normal.position, None);
    assert_eq!(geometry.confirm(latest, Some(2.0), normal_mode()), None);
    assert_eq!(geometry.normal.width, size.width);
    assert_eq!(geometry.normal.height, size.height);
    assert_eq!(
        geometry.normal.position,
        saved_position(windowed_position, 2.0)
    );
}

#[test]
fn records_maximized_state_without_replacing_normal_bounds() {
    let size = Size::new(840.0, 580.0);
    let mut geometry = Geometry::new(size, None);
    let revision = geometry
        .observe(Some(Size::new(1440.0, 850.0)), None)
        .unwrap();
    let maximized = kortty_windowing::WindowMode {
        windowed: true,
        maximized: Some(true),
        ..kortty_windowing::WindowMode::default()
    };
    assert_eq!(geometry.confirm(revision, None, maximized), None);
    assert!(geometry.maximized);
    assert_eq!(geometry.normal.width, size.width);
    let revision = geometry.observe(None, None).unwrap();
    assert_eq!(geometry.confirm(revision, None, Default::default()), None);
    assert!(geometry.maximized);
    let revision = geometry.observe(Some(size), None).unwrap();
    assert_eq!(geometry.confirm(revision, Some(1.0), normal_mode()), None);
    assert!(!geometry.maximized);
}

#[test]
fn reused_window_waits_for_normal_mode_before_restoring_bounds() {
    let mut geometry = Geometry::new(Size::new(840.0, 580.0), None);
    let placement = WindowPlacement {
        width: 740.0,
        height: 520.0,
        position: SavedPosition::new(40.0, 80.0),
    };
    let deadline = Instant::now() + PLACEMENT_DEADLINE;
    let (cancel, cancelled) = mpsc::channel();
    let request = geometry.restore_when_unmaximized(placement, true, deadline, cancel);
    // Supply the window manager replies as test doubles, without resize or move events.
    let revision = geometry.placement_probe(request).unwrap();
    assert_eq!(geometry.placement_probe(request), None);
    let maximized = kortty_windowing::WindowMode {
        windowed: true,
        maximized: Some(true),
        ..kortty_windowing::WindowMode::default()
    };
    assert_eq!(geometry.confirm(revision, None, maximized), None);
    assert!(geometry.take_pending_placement(maximized).is_none());
    assert!(
        geometry
            .take_pending_placement(Default::default())
            .is_none()
    );
    assert_eq!(geometry.placement_probe(request), Some(revision));
    assert_eq!(geometry.confirm(revision, Some(1.0), normal_mode()), None);
    let pending = geometry.take_pending_placement(normal_mode()).unwrap();
    assert_eq!(pending.placement, placement);
    assert!(pending.maximize);
    drop(pending);
    assert_eq!(cancelled.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    assert_eq!(geometry.placement_probe(request), None);
    assert!(geometry.take_pending_placement(normal_mode()).is_none());

    let request_two =
        geometry.restore_when_unmaximized(placement, false, deadline, mpsc::channel().0);
    assert_eq!(geometry.placement_probe(request), None);
    let fullscreen = kortty_windowing::WindowMode {
        fullscreen: true,
        ..kortty_windowing::WindowMode::default()
    };
    assert!(geometry.take_pending_placement(fullscreen).is_none());
    assert_eq!(geometry.placement_probe(request_two), None);
    assert!(geometry.take_pending_placement(normal_mode()).is_none());

    let request_three =
        geometry.restore_when_unmaximized(placement, false, Instant::now(), mpsc::channel().0);
    assert_eq!(geometry.placement_probe(request_three), None);
    assert!(geometry.pending_placement.is_none());
}
