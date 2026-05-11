use crate::model::terminal_effect::{
    normalize_terminal_effect_speed, TerminalEffectPluginBundle, TerminalEffectPluginEntry,
    TerminalEffectPluginManifest, TerminalEffectPluginSource,
};
use crate::persistence::xml_repository;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const DISABLED_FILE: &str = "terminal-effect-plugins.disabled";
const PLUGINS_SUBDIR: &str = "plugins/terminal-effects";
const BUNDLED_SUBDIR: &str = "bundled-plugins/terminal-effects";

const MOTHER_JS: &str = r#"export function describe() {
  return {
    id: "mother",
    name: "MU/TH/UR 6000",
    features: ["crt-overlay", "paced-output", "control-sequence-bypass", "high-volume-bypass", "line-flash"]
  };
}
"#;

const MOTHER_CSS: &str = r#".kortty-terminal-effect-mother {
  background: #000;
  filter: contrast(1.18) saturate(1.22);
}

.kortty-terminal-effect-mother .xterm {
  background: #000 !important;
  text-shadow: 0 0 2px rgba(25, 255, 76, 0.8), 0 0 8px rgba(25, 255, 76, 0.28);
}

.kortty-terminal-effect-mother .xterm-screen,
.kortty-terminal-effect-mother .xterm-viewport {
  background: #000 !important;
}

.kortty-mother-terminal-host {
  background: radial-gradient(circle at 50% 42%, rgba(25, 255, 76, 0.10), transparent 58%), #000;
}

.kortty-mother-line-flash-layer,
.kortty-mother-crt-overlay {
  pointer-events: none;
  position: absolute;
  inset: 0;
  overflow: hidden;
}

.kortty-mother-line-flash-layer {
  z-index: 18;
}

.kortty-mother-crt-overlay {
  z-index: 20;
}

.kortty-mother-crt-overlay::before {
  content: "";
  position: absolute;
  inset: 0;
  background:
    repeating-linear-gradient(0deg, rgba(0, 0, 0, 0.30) 0, rgba(0, 0, 0, 0.30) 1px, transparent 1px, transparent 4px),
    repeating-linear-gradient(0deg, transparent 0, transparent 7px, rgba(40, 255, 80, 0.055) 7px, rgba(40, 255, 80, 0.055) 8px),
    linear-gradient(90deg, rgba(255, 0, 0, 0.05), rgba(0, 255, 0, 0.018), rgba(0, 0, 255, 0.05));
  background-size: 100% 4px, 100% 8px, 4px 100%;
  mix-blend-mode: screen;
}

.kortty-mother-crt-overlay::after {
  content: "";
  position: absolute;
  inset: 0;
  background:
    linear-gradient(90deg, rgba(0, 0, 0, 0.25), transparent 4%, transparent 96%, rgba(0, 0, 0, 0.25)),
    linear-gradient(0deg, rgba(0, 0, 0, 0.26), transparent 7%, transparent 93%, rgba(0, 0, 0, 0.28)),
    radial-gradient(ellipse at center, transparent 56%, rgba(0, 0, 0, 0.18) 100%);
}

.kortty-mother-noise {
  position: absolute;
  inset: 0;
  opacity: 0.20;
  background-image:
    radial-gradient(circle at 13% 17%, rgba(180, 255, 190, 0.24) 0 1px, transparent 1px),
    radial-gradient(circle at 47% 53%, rgba(180, 255, 190, 0.16) 0 1px, transparent 1px),
    radial-gradient(circle at 79% 31%, rgba(180, 255, 190, 0.18) 0 1px, transparent 1px);
  background-size: 91px 77px, 137px 113px, 173px 149px;
  animation: kortty-mother-noise 140ms steps(2, end) infinite;
}

.kortty-mother-line-flash {
  position: absolute;
  left: 0;
  right: 0;
  border-radius: 2px;
  background: linear-gradient(180deg, rgba(245, 255, 210, 0.12), rgba(150, 255, 175, 0.24) 45%, rgba(25, 255, 76, 0.08));
  box-shadow: 0 0 12px rgba(25, 255, 76, 0.55), inset 0 1px 0 rgba(245, 255, 210, 0.18);
  mix-blend-mode: screen;
  animation: kortty-mother-line-flash 260ms ease-out forwards;
}

@keyframes kortty-mother-line-flash {
  from { opacity: 1; transform: scaleY(1.06); }
  to { opacity: 0; transform: scaleY(1); }
}

@keyframes kortty-mother-noise {
  from { transform: translate3d(0, 0, 0); }
  to { transform: translate3d(3px, -2px, 0); }
}
"#;

pub fn list_plugins() -> Result<Vec<TerminalEffectPluginEntry>> {
    ensure_bundled_mother()?;
    let disabled = load_disabled_plugin_ids()?;
    let mut entries = Vec::new();
    collect_plugins_from_dir(
        &bundled_plugins_dir()?,
        TerminalEffectPluginSource::Bundled,
        &disabled,
        &mut entries,
    )?;
    collect_plugins_from_dir(
        &plugins_dir()?,
        TerminalEffectPluginSource::Imported,
        &disabled,
        &mut entries,
    )?;
    entries.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut seen = HashSet::new();
    Ok(entries
        .into_iter()
        .filter(|entry| seen.insert(entry.id.clone()))
        .collect())
}

pub fn load_plugin_bundle(plugin_id: &str) -> Result<TerminalEffectPluginBundle> {
    let id = normalize_plugin_id(plugin_id)?;
    let entry = list_plugins()?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| anyhow!("Terminal effect plugin not found: {id}"))?;
    if !entry.enabled {
        bail!("Terminal effect plugin is disabled: {id}");
    }

    let entry_js = fs::read_to_string(&entry.entry_path)
        .with_context(|| format!("Failed to read terminal effect entry {}", entry.entry_path))?;
    let css = entry
        .css_path
        .as_deref()
        .map(fs::read_to_string)
        .transpose()
        .with_context(|| format!("Failed to read terminal effect CSS for {}", entry.id))?;
    Ok(TerminalEffectPluginBundle {
        manifest: TerminalEffectPluginManifest {
            id: entry.id,
            name: entry.name,
            version: entry.version,
            description: entry.description,
            entry: Path::new(&entry.entry_path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "entry.js".into()),
            css: entry.css_path.as_ref().and_then(|path| {
                Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            }),
            assets: Vec::new(),
        },
        entry_js,
        css,
    })
}

pub fn set_plugin_enabled(plugin_id: &str, enabled: bool) -> Result<()> {
    let id = normalize_plugin_id(plugin_id)?;
    if !list_plugins()?.iter().any(|entry| entry.id == id) {
        bail!("Unknown terminal effect plugin id: {id}");
    }
    let mut disabled = load_disabled_plugin_ids()?;
    if enabled {
        disabled.remove(&id);
    } else {
        disabled.insert(id);
    }
    save_disabled_plugin_ids(&disabled)
}

pub fn import_plugin_package(source_path: &Path) -> Result<TerminalEffectPluginEntry> {
    if source_path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
    {
        bail!(
            "Java terminal effect JARs are not compatible with KorTTY Rust. Import a trusted Tauri-native JS effect bundle instead."
        );
    }
    ensure_plugins_dir()?;

    let staging = xml_repository::config_dir()?.join(format!(
        "terminal-effect-import-{}",
        uuid::Uuid::new_v4().simple()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    if source_path.is_dir() {
        copy_dir_contents(source_path, &staging)?;
    } else if source_path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        extract_zip(source_path, &staging)?;
    } else {
        let _ = fs::remove_dir_all(&staging);
        bail!("Terminal effect imports must be a directory or .zip bundle");
    }

    let manifest = read_manifest(&staging)?;
    validate_manifest(&manifest, &staging)?;
    let target = plugins_dir()?.join(&manifest.id);
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    if fs::rename(&staging, &target).is_err() {
        copy_dir_contents(&staging, &target)?;
        fs::remove_dir_all(&staging)?;
    }

    list_plugins()?
        .into_iter()
        .find(|entry| entry.id == manifest.id)
        .ok_or_else(|| anyhow!("Imported terminal effect plugin was not loaded"))
}

pub fn export_plugin_package(plugin_id: &str, target_path: &Path) -> Result<()> {
    let id = normalize_plugin_id(plugin_id)?;
    let entry = list_plugins()?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| anyhow!("Terminal effect plugin not found: {id}"))?;
    let package_path = PathBuf::from(entry.package_path);
    if target_path.extension().is_some_and(|ext| ext == "zip") {
        zip_dir(&package_path, target_path)?;
    } else {
        if target_path.exists() {
            fs::remove_dir_all(target_path)?;
        }
        copy_dir_contents(&package_path, target_path)?;
    }
    Ok(())
}

pub fn normalize_speed(speed: f64) -> f64 {
    normalize_terminal_effect_speed(speed)
}

fn collect_plugins_from_dir(
    dir: &Path,
    source: TerminalEffectPluginSource,
    disabled: &HashSet<String>,
    entries: &mut Vec<TerminalEffectPluginEntry>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let package_path = entry.path();
        if !package_path.is_dir() {
            continue;
        }
        let manifest = match read_manifest(&package_path) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!(path = %package_path.display(), error = %error, "Skipping invalid terminal effect plugin");
                continue;
            }
        };
        validate_manifest(&manifest, &package_path)?;
        let enabled = !disabled.contains(&manifest.id);
        let entry_path = package_path.join(&manifest.entry);
        let css_path = manifest.css.as_ref().map(|css| package_path.join(css));
        entries.push(TerminalEffectPluginEntry {
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            entry_path: entry_path.to_string_lossy().into_owned(),
            css_path: css_path.map(|path| path.to_string_lossy().into_owned()),
            package_path: package_path.to_string_lossy().into_owned(),
            bundled: matches!(source, TerminalEffectPluginSource::Bundled),
            source: source.clone(),
            enabled,
        });
    }
    Ok(())
}

fn ensure_bundled_mother() -> Result<()> {
    let dir = bundled_plugins_dir()?.join("mother");
    fs::create_dir_all(&dir)?;
    let manifest = TerminalEffectPluginManifest::bundled_mother();
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    fs::write(dir.join("mother.js"), MOTHER_JS)?;
    fs::write(dir.join("mother.css"), MOTHER_CSS)?;
    Ok(())
}

fn read_manifest(package_path: &Path) -> Result<TerminalEffectPluginManifest> {
    let text = fs::read_to_string(package_path.join("manifest.json"))
        .with_context(|| format!("Missing manifest.json in {}", package_path.display()))?;
    serde_json::from_str(&text).with_context(|| {
        format!(
            "Invalid terminal effect manifest {}",
            package_path.join("manifest.json").display()
        )
    })
}

pub fn validate_manifest(
    manifest: &TerminalEffectPluginManifest,
    package_path: &Path,
) -> Result<()> {
    let id = normalize_plugin_id(&manifest.id)?;
    if id != manifest.id {
        bail!("Terminal effect plugin id must be lowercase and trimmed");
    }
    if manifest.name.trim().is_empty() {
        bail!("Terminal effect plugin name is required");
    }
    validate_relative_asset_path(&manifest.entry, "entry")?;
    if !package_path.join(&manifest.entry).is_file() {
        bail!("Terminal effect entry file is missing: {}", manifest.entry);
    }
    if let Some(css) = manifest.css.as_deref() {
        validate_relative_asset_path(css, "css")?;
        if !package_path.join(css).is_file() {
            bail!("Terminal effect CSS file is missing: {css}");
        }
    }
    for asset in &manifest.assets {
        validate_relative_asset_path(asset, "asset")?;
    }
    Ok(())
}

fn validate_relative_asset_path(path: &str, label: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("Terminal effect {label} path is required");
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        bail!("Terminal effect {label} path must be relative");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("Terminal effect {label} path must stay inside the plugin package");
    }
    Ok(())
}

fn normalize_plugin_id(plugin_id: &str) -> Result<String> {
    let id = plugin_id.trim();
    if !is_valid_plugin_id(id) {
        bail!("Invalid terminal effect plugin id: {plugin_id}");
    }
    Ok(id.to_string())
}

fn is_valid_plugin_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if value.len() > 64 || !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|ch| {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_' || ch == '-'
    })
}

fn load_disabled_plugin_ids() -> Result<HashSet<String>> {
    let path = xml_repository::config_dir()?.join(DISABLED_FILE);
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let content = fs::read_to_string(path)?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

fn save_disabled_plugin_ids(disabled: &HashSet<String>) -> Result<()> {
    let mut ids = disabled.iter().cloned().collect::<Vec<_>>();
    ids.sort();
    fs::write(
        xml_repository::config_dir()?.join(DISABLED_FILE),
        ids.join("\n"),
    )?;
    Ok(())
}

fn ensure_plugins_dir() -> Result<()> {
    fs::create_dir_all(plugins_dir()?)?;
    Ok(())
}

fn plugins_dir() -> Result<PathBuf> {
    Ok(xml_repository::config_dir()?.join(PLUGINS_SUBDIR))
}

fn bundled_plugins_dir() -> Result<PathBuf> {
    Ok(xml_repository::config_dir()?.join(BUNDLED_SUBDIR))
}

fn copy_dir_contents(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_contents(&source_path, &target_path)?;
        } else if source_path.is_file() {
            fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn extract_zip(source_zip: &Path, target: &Path) -> Result<()> {
    let file = fs::File::open(source_zip)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let Some(enclosed_name) = file.enclosed_name() else {
            bail!("Terminal effect zip contains an unsafe path");
        };
        let target_path = target.join(enclosed_name);
        if file.is_dir() {
            fs::create_dir_all(&target_path)?;
        } else {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = fs::File::create(&target_path)?;
            std::io::copy(&mut file, &mut output)?;
        }
    }
    Ok(())
}

fn zip_dir(source: &Path, target_zip: &Path) -> Result<()> {
    if let Some(parent) = target_zip.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(target_zip)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    add_dir_to_zip(source, source, &mut zip, options)?;
    zip.finish()?;
    Ok(())
}

fn add_dir_to_zip<W: Write + std::io::Seek>(
    base: &Path,
    current: &Path,
    zip: &mut zip::ZipWriter<W>,
    options: zip::write::SimpleFileOptions,
) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base)?
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            zip.add_directory(format!("{rel}/"), options)?;
            add_dir_to_zip(base, &path, zip, options)?;
        } else if path.is_file() {
            zip.start_file(rel, options)?;
            let mut file = fs::File::open(path)?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_plugin_ids() {
        assert!(is_valid_plugin_id("mother"));
        assert!(is_valid_plugin_id("a.b-c_1"));
        assert!(!is_valid_plugin_id(""));
        assert!(!is_valid_plugin_id("Mother"));
        assert!(!is_valid_plugin_id("-bad"));
        assert!(!is_valid_plugin_id(&"a".repeat(65)));
    }

    #[test]
    fn normalizes_animation_speed_like_java() {
        assert_eq!(normalize_speed(0.0), 1.0);
        assert_eq!(normalize_speed(f64::NAN), 1.0);
        assert_eq!(normalize_speed(99.0), 99.0);
        assert_eq!(normalize_speed(120.0), 99.0);
    }
}
