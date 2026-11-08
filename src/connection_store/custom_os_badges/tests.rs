use std::fs;

use super::*;

struct TestProfile(PathBuf);

impl TestProfile {
    fn new() -> Self {
        let directory = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = directory.join(format!("kortty-custom-os-badges-{}", nonce().unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn png(&self) -> PathBuf {
        let path = self.0.join("source.png");
        image::RgbaImage::from_pixel(48, 48, image::Rgba([24, 112, 205, 255]))
            .save(&path)
            .unwrap();
        path
    }
}

impl Drop for TestProfile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn stored_symbol_is_private_and_rename_keeps_reference_across_reopen() {
    let profile = TestProfile::new();
    let mut catalog = Catalog::load(&profile.0).unwrap();
    assert!(catalog.entries().is_empty());
    let reference = catalog.add(" Router ", " Custom ", &profile.png()).unwrap();
    let mut reopened = Catalog::load(&profile.0).unwrap();
    let badge = reopened.find(&reference).unwrap();
    let filename = badge.record.filename();
    assert_eq!((badge.name(), badge.family()), ("Router", "Custom"));
    let image = image::load_from_memory(badge.png()).unwrap().into_rgba8();
    assert_eq!(image.dimensions(), (128, 128));
    assert_eq!(image.get_pixel(64, 64).0, [24, 112, 205, 255]);
    assert_eq!(image.get_pixel(0, 0).0[3], 0);
    let directory = profile.0.join(DIRECTORY);
    let stored = directory.join(filename);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        for (path, mode) in [
            (directory.clone(), 0o700),
            (directory.join(MANIFEST), 0o600),
            (stored.clone(), 0o600),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
    }
    reopened.rename(&reference, "Router 2", "Network").unwrap();
    assert!(catalog.remove(&reference).is_err());
    let mut renamed = Catalog::load(&profile.0).unwrap();
    let badge = renamed.find(&reference).unwrap();
    assert_eq!((badge.name(), badge.family()), ("Router 2", "Network"));

    renamed.remove(&reference).unwrap();
    assert!(!stored.exists());
    assert!(
        Catalog::load(&profile.0)
            .unwrap()
            .find(&reference)
            .is_none()
    );
}

#[test]
fn missing_or_corrupt_png_is_unavailable_but_its_record_is_editable() {
    let profile = TestProfile::new();
    let mut catalog = Catalog::load(&profile.0).unwrap();
    let missing = catalog.add("Missing", "Custom", &profile.png()).unwrap();
    let corrupt = catalog.add("Corrupt", "Custom", &profile.png()).unwrap();
    let directory = profile.0.join(DIRECTORY);
    fs::remove_file(directory.join(catalog.find(&missing).unwrap().record.filename())).unwrap();
    fs::write(
        directory.join(catalog.find(&corrupt).unwrap().record.filename()),
        b"invalid PNG",
    )
    .unwrap();

    let mut reopened = Catalog::load(&profile.0).unwrap();
    assert_eq!(reopened.entries().len(), 2);
    assert!(reopened.find(&missing).is_none());
    assert!(reopened.find(&corrupt).is_none());
    reopened
        .rename(&missing, "Still missing", "Custom")
        .unwrap();
    reopened.remove(&corrupt).unwrap();
    reopened.remove(&missing).unwrap();
    assert!(Catalog::load(&profile.0).unwrap().entries().is_empty());
}

#[test]
fn static_svg_rasterizes_and_external_or_active_content_is_rejected() {
    let profile = TestProfile::new();
    let mut catalog = Catalog::load(&profile.0).unwrap();
    let svg = profile.0.join("source.svg");
    fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 32">
            <defs><linearGradient id="fill">
                <stop stop-color="#12a652"/><stop offset="1" stop-color="#12a652"/>
            </linearGradient></defs>
            <rect width="64" height="32" fill="url(#fill)"/>
        </svg>"##,
    )
    .unwrap();
    let reference = catalog.add("Native SVG", "Custom", &svg).unwrap();
    let image = image::load_from_memory(catalog.find(&reference).unwrap().png())
        .unwrap()
        .into_rgba8();
    assert_eq!(image.get_pixel(64, 64).0, [18, 166, 82, 255]);
    assert_eq!(image.get_pixel(64, 10).0[3], 0);

    for unsafe_svg in [
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" onload="alert(1)"/>"#,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><image href="file:///private/symbol.png"/></svg>"#,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><path fill="url(https://example.test/icon.svg)" d="M0 0L64 64Z"/></svg>"#,
        r#"<!DOCTYPE svg [<!ENTITY ext SYSTEM "file:///private/symbol.png">]><svg xmlns="http://www.w3.org/2000/svg">&ext;</svg>"#,
    ] {
        fs::write(&svg, unsafe_svg).unwrap();
        assert!(catalog.add("Rejected", "Custom", &svg).is_err());
    }
    assert_eq!(catalog.entries().len(), 1);
}
