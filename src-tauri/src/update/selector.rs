//! Release-asset selection, a 1:1 port of Java `UpdateAssetSelector`.

use super::platform::{OperatingSystem, PlatformProfile};
use super::{UpdateAsset, UpdateRelease};

/// Picks the best installable asset for the given platform, or `None` when no
/// candidate matches (mirrors Java `UpdateAssetSelector.select`).
pub fn select(release: &UpdateRelease, profile: &PlatformProfile) -> Option<UpdateAsset> {
    let mut assets: Vec<&UpdateAsset> = release
        .assets
        .iter()
        .filter(|asset| is_installable_asset(asset))
        .collect();
    assets.sort_by(|a, b| a.name.cmp(&b.name));
    let selected = match profile.operating_system {
        OperatingSystem::Windows => select_windows_asset(&assets, profile),
        OperatingSystem::Macos => select_mac_asset(&assets, profile),
        OperatingSystem::Linux => select_linux_asset(&assets, profile),
        OperatingSystem::Other => select_java_zip(&assets),
    };
    selected.cloned()
}

fn select_windows_asset<'a>(
    assets: &[&'a UpdateAsset],
    profile: &PlatformProfile,
) -> Option<&'a UpdateAsset> {
    find_asset(assets, profile, "windows", ".msi")
        .or_else(|| find_asset(assets, profile, "windows", ".zip"))
        .or_else(|| select_java_zip(assets))
}

fn select_mac_asset<'a>(
    assets: &[&'a UpdateAsset],
    profile: &PlatformProfile,
) -> Option<&'a UpdateAsset> {
    find_asset(assets, profile, "macos", ".dmg")
        .or_else(|| find_asset(assets, profile, "macos", ".zip"))
        .or_else(|| select_java_zip(assets))
}

fn select_linux_asset<'a>(
    assets: &[&'a UpdateAsset],
    profile: &PlatformProfile,
) -> Option<&'a UpdateAsset> {
    let preferred_extension = preferred_linux_extension(profile);
    // Exact fallback chain from Java: preferred → .deb → .rpm → .pkg.tar.zst
    // → .tar.gz → .zip → kortty-java-*.zip (skipping the preferred extension
    // where it would repeat).
    find_linux_asset(assets, profile, preferred_extension)
        .or_else(|| {
            if preferred_extension == ".deb" {
                None
            } else {
                find_linux_asset(assets, profile, ".deb")
            }
        })
        .or_else(|| {
            if preferred_extension == ".rpm" {
                None
            } else {
                find_linux_asset(assets, profile, ".rpm")
            }
        })
        .or_else(|| {
            if preferred_extension == ".pkg.tar.zst" {
                None
            } else {
                find_linux_asset(assets, profile, ".pkg.tar.zst")
            }
        })
        .or_else(|| {
            if preferred_extension == ".tar.gz" {
                None
            } else {
                find_linux_asset(assets, profile, ".tar.gz")
            }
        })
        .or_else(|| find_linux_asset(assets, profile, ".zip"))
        .or_else(|| select_java_zip(assets))
}

fn preferred_linux_extension(profile: &PlatformProfile) -> &'static str {
    if profile.linux_matches(&["arch", "manjaro"]) {
        return ".pkg.tar.zst";
    }
    if profile.linux_matches(&["debian", "ubuntu", "linuxmint", "pop"]) {
        return ".deb";
    }
    if profile.linux_matches(&[
        "fedora", "rhel", "centos", "rocky", "alma", "opensuse", "suse",
    ]) {
        return ".rpm";
    }
    ".tar.gz"
}

fn find_asset<'a>(
    assets: &[&'a UpdateAsset],
    profile: &PlatformProfile,
    platform_token: &str,
    extension: &str,
) -> Option<&'a UpdateAsset> {
    assets
        .iter()
        .find(|asset| {
            let name = normalized_name(asset);
            name.contains(platform_token)
                && name.ends_with(extension)
                && matches_architecture(&name, profile)
        })
        .copied()
}

fn find_linux_asset<'a>(
    assets: &[&'a UpdateAsset],
    profile: &PlatformProfile,
    extension: &str,
) -> Option<&'a UpdateAsset> {
    assets
        .iter()
        .find(|asset| {
            let name = normalized_name(asset);
            is_linux_asset_name(&name)
                && name.ends_with(extension)
                && matches_architecture(&name, profile)
        })
        .copied()
}

fn select_java_zip<'a>(assets: &[&'a UpdateAsset]) -> Option<&'a UpdateAsset> {
    assets
        .iter()
        .find(|asset| {
            let name = normalized_name(asset);
            name.starts_with("kortty-java-") && name.ends_with(".zip")
        })
        .copied()
}

fn is_installable_asset(asset: &UpdateAsset) -> bool {
    let name = normalized_name(asset);
    !name.ends_with(".sig")
        && !name.contains("-debug-")
        && !name.contains("docs-diagrams")
        && !name.contains("signing-public")
}

fn is_linux_asset_name(name: &str) -> bool {
    name.starts_with("kortty-linux-") || name.ends_with(".pkg.tar.zst")
}

fn matches_architecture(name: &str, profile: &PlatformProfile) -> bool {
    profile
        .architecture_tokens()
        .iter()
        .any(|token| name.contains(token))
}

fn normalized_name(asset: &UpdateAsset) -> String {
    asset.name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(asset_names: &[&str]) -> UpdateRelease {
        UpdateRelease {
            tag_name: "v2.3.0".to_string(),
            name: "korTTY v2.3.0".to_string(),
            html_uri: Some("https://example.test/releases/v2.3.0".to_string()),
            published_at: Some("2026-05-20T10:00:00Z".to_string()),
            draft: false,
            prerelease: false,
            assets: asset_names.iter().map(|name| asset(name)).collect(),
        }
    }

    fn asset(name: &str) -> UpdateAsset {
        UpdateAsset {
            name: name.to_string(),
            download_uri: format!("https://example.test/downloads/{name}"),
            size: 12,
            digest: Some(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            ),
        }
    }

    #[test]
    fn selects_windows_msi_for_matching_architecture() {
        let selected = select(
            &release(&[
                "korTTY-Windows-2.3.0-x86_64.zip",
                "korTTY-Windows-2.3.0-x86_64.msi",
                "korTTY-Java-2.3.0.zip",
            ]),
            &PlatformProfile::new(OperatingSystem::Windows, "amd64", None, &[]),
        )
        .unwrap();

        assert_eq!(selected.name, "korTTY-Windows-2.3.0-x86_64.msi");
    }

    #[test]
    fn selects_mac_dmg_for_arm_architecture() {
        let selected = select(
            &release(&[
                "korTTY-macOS-2.3.0-aarch64.zip",
                "korTTY-macOS-2.3.0-aarch64.dmg",
            ]),
            &PlatformProfile::new(OperatingSystem::Macos, "arm64", None, &[]),
        )
        .unwrap();

        assert_eq!(selected.name, "korTTY-macOS-2.3.0-aarch64.dmg");
    }

    #[test]
    fn selects_deb_for_debian_like_linux() {
        let selected = select(
            &release(&[
                "kortty-Linux-2.3.0-x86_64.tar.gz",
                "kortty-Linux-2.3.0-x86_64.deb",
            ]),
            &PlatformProfile::new(
                OperatingSystem::Linux,
                "x86_64",
                Some("ubuntu"),
                &["debian"],
            ),
        )
        .unwrap();

        assert_eq!(selected.name, "kortty-Linux-2.3.0-x86_64.deb");
    }

    #[test]
    fn selects_rpm_for_fedora_like_linux() {
        let selected = select(
            &release(&[
                "kortty-Linux-2.3.0-x86_64.tar.gz",
                "kortty-Linux-2.3.0-x86_64.rpm",
            ]),
            &PlatformProfile::new(OperatingSystem::Linux, "x86_64", Some("fedora"), &[]),
        )
        .unwrap();

        assert_eq!(selected.name, "kortty-Linux-2.3.0-x86_64.rpm");
    }

    #[test]
    fn selects_arch_package_for_arch_linux() {
        let selected = select(
            &release(&[
                "kortty-Linux-2.3.0-x86_64.tar.gz",
                "kortty-2.3.0-1-x86_64.pkg.tar.zst",
            ]),
            &PlatformProfile::new(OperatingSystem::Linux, "x86_64", Some("arch"), &[]),
        )
        .unwrap();

        assert_eq!(selected.name, "kortty-2.3.0-1-x86_64.pkg.tar.zst");
    }

    #[test]
    fn falls_back_to_java_zip_when_no_native_asset_matches() {
        let selected = select(
            &release(&["korTTY-Java-2.3.0.zip"]),
            &PlatformProfile::new(OperatingSystem::Macos, "x86_64", None, &[]),
        )
        .unwrap();

        assert_eq!(selected.name, "korTTY-Java-2.3.0.zip");
    }

    #[test]
    fn filters_signatures_debug_and_doc_assets() {
        let selected = select(
            &release(&[
                "korTTY-Windows-2.3.0-x86_64.msi.sig",
                "korTTY-Windows-debug-2.3.0-x86_64.msi",
                "kortty-docs-diagrams-2.3.0.zip",
                "kortty-signing-public-key.zip",
                "korTTY-Java-2.3.0.zip",
            ]),
            &PlatformProfile::new(OperatingSystem::Windows, "x86_64", None, &[]),
        )
        .unwrap();

        assert_eq!(selected.name, "korTTY-Java-2.3.0.zip");
    }

    #[test]
    fn unknown_linux_distro_prefers_tar_gz() {
        let selected = select(
            &release(&[
                "kortty-Linux-2.3.0-x86_64.deb",
                "kortty-Linux-2.3.0-x86_64.tar.gz",
            ]),
            &PlatformProfile::new(OperatingSystem::Linux, "x86_64", Some("gentoo"), &[]),
        )
        .unwrap();

        assert_eq!(selected.name, "kortty-Linux-2.3.0-x86_64.tar.gz");
    }

    #[test]
    fn skips_assets_for_other_architectures() {
        let result = select(
            &release(&["korTTY-Windows-2.3.0-aarch64.msi"]),
            &PlatformProfile::new(OperatingSystem::Windows, "x86_64", None, &[]),
        );

        assert!(result.is_none());
    }
}
