//! Saved opt-in policy for the verified stable llama.cpp release channel.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{Choice, PackageBackend};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    #[default]
    Off,
    Notify,
    AutomaticStable,
}

impl Mode {
    pub(crate) const ALL: [Self; 3] = [Self::Off, Self::Notify, Self::AutomaticStable];

    pub(crate) fn checks_channel(self) -> bool {
        self != Self::Off
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off (manual checks only)",
            Self::Notify => "Notify",
            Self::AutomaticStable => "Automatic Stable",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Backend {
    #[default]
    Auto,
    Cpu,
    Metal,
    Vulkan,
}

impl Backend {
    pub(crate) fn choices() -> [Self; 3] {
        [
            Self::Auto,
            Self::Cpu,
            if cfg!(target_os = "macos") {
                Self::Metal
            } else {
                Self::Vulkan
            },
        ]
    }

    fn required(self) -> Option<PackageBackend> {
        match self {
            Self::Auto => None,
            Self::Cpu => Some(PackageBackend::Cpu),
            Self::Metal => Some(PackageBackend::Metal),
            Self::Vulkan => Some(PackageBackend::Vulkan),
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.required() {
            Some(backend) => backend.fmt(formatter),
            None => formatter.write_str("Automatic"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) mode: Mode,
    pub(crate) backend: Backend,
}

impl Settings {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn decision<'a>(
        self,
        choices: &'a [Choice],
        installed: Option<Installed<'_>>,
        pinned_runtime: Option<&str>,
    ) -> Decision<'a> {
        self.decide(
            choices,
            installed,
            pinned_runtime,
            cfg!(target_os = "macos"),
        )
    }

    pub(super) fn pin_available(
        self,
        choices: &[Choice],
        installed: Option<Installed<'_>>,
        pin: &str,
    ) -> bool {
        let backend = self
            .backend
            .required()
            .or_else(|| installed.map(|current| current.backend));
        select(choices, backend, Some(pin), cfg!(target_os = "macos")).is_some()
    }

    fn decide<'a>(
        self,
        choices: &'a [Choice],
        installed: Option<Installed<'_>>,
        pinned_runtime: Option<&str>,
        macos: bool,
    ) -> Decision<'a> {
        if !self.mode.checks_channel() {
            return Decision::Disabled;
        }
        let exact = self.backend.required();
        let backend = exact.or_else(|| installed.map(|current| current.backend));
        let candidate = |pin: Option<&str>| select(choices, backend, pin, macos);
        let pinned = pinned_runtime.and_then(|pin| candidate(Some(pin)));
        let revoked = installed.is_some_and(|current| current.revoked);
        let Some(candidate) = pinned.or_else(|| candidate(None)) else {
            return if installed.is_some_and(|current| !current.revoked) {
                Decision::Current {
                    pending_first_launch: installed
                        .is_some_and(|current| current.pending_first_launch),
                }
            } else {
                Decision::NoCompatible { revoked }
            };
        };
        if let Some(current) = installed.filter(|current| !current.revoked) {
            let switches_backend = exact.is_some_and(|backend| backend != current.backend);
            let newer = build(&candidate.id)
                .zip(build(current.id))
                .is_some_and(|(candidate, current)| candidate > current);
            if current.id == candidate.id || (!switches_backend && (pinned.is_some() || !newer)) {
                return Decision::Current {
                    pending_first_launch: current.pending_first_launch,
                };
            }
        }
        match self.mode {
            Mode::Off => Decision::Disabled,
            Mode::Notify => Decision::Offer {
                package: candidate,
                revoked,
            },
            Mode::AutomaticStable => Decision::Stage {
                package: candidate,
                revoked,
            },
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Installed<'a> {
    pub(crate) id: &'a str,
    pub(crate) backend: PackageBackend,
    pub(crate) revoked: bool,
    pub(crate) pending_first_launch: bool,
}

pub(crate) enum Decision<'a> {
    Disabled,
    Current { pending_first_launch: bool },
    NoCompatible { revoked: bool },
    Offer { package: &'a Choice, revoked: bool },
    Stage { package: &'a Choice, revoked: bool },
}

pub(super) fn select<'a>(
    choices: &'a [Choice],
    backend: Option<PackageBackend>,
    pin: Option<&str>,
    macos: bool,
) -> Option<&'a Choice> {
    let best = |backend| {
        choices
            .iter()
            .filter(|choice| {
                choice.backend == backend
                    && pin.is_none_or(|pin| runtime_id(&choice.id) == Some(pin))
            })
            .filter_map(|choice| build(&choice.id).map(|build| (build, choice)))
            .max_by_key(|(build, _)| *build)
            .map(|(_, choice)| choice)
    };
    if let Some(backend) = backend {
        return best(backend);
    }
    let (preferred, fallback) = if macos {
        (PackageBackend::Metal, PackageBackend::Cpu)
    } else {
        (PackageBackend::Cpu, PackageBackend::Vulkan)
    };
    best(preferred).or_else(|| best(fallback))
}

pub(super) fn runtime_id(id: &str) -> Option<&str> {
    let (_, revision) = id.split_once("-kortty")?;
    let revision = revision.split('-').next()?;
    let length = id.find("-kortty")? + "-kortty".len() + revision.len();
    id.get(..length)
}

fn build(id: &str) -> Option<(u64, u64)> {
    let (build, revision) = runtime_id(id)?
        .strip_prefix("llama-b")?
        .split_once("-kortty")?;
    Some((build.parse().ok()?, revision.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str, backend: PackageBackend) -> Choice {
        Choice {
            id: format!("{id}-fixture"),
            backend,
            size: 1,
            url: String::new(),
            digest: "a".repeat(64),
            entrypoint: "bin/llama-server".into(),
        }
    }

    #[test]
    fn opted_in_policy_preserves_backend_and_compares_numeric_stable_builds() {
        let catalog = [
            choice("llama-b9999-kortty11", PackageBackend::Cpu),
            choice("llama-b10000-kortty1", PackageBackend::Cpu),
            choice("llama-b10001-kortty1", PackageBackend::Vulkan),
        ];
        assert!(matches!(
            Settings::default().decide(&catalog, None, None, false),
            Decision::Disabled
        ));
        let notify = Settings {
            mode: Mode::Notify,
            backend: Backend::Auto,
        };
        let Decision::Offer {
            package,
            revoked: false,
        } = notify.decide(&catalog, None, None, false)
        else {
            panic!("expected the newest CPU package");
        };
        assert_eq!(package.id, "llama-b10000-kortty1-fixture");
        let installed = Installed {
            id: "llama-b10000-kortty1-fixture",
            backend: PackageBackend::Cpu,
            revoked: false,
            pending_first_launch: true,
        };
        assert!(matches!(
            notify.decide(&catalog, Some(installed), None, false),
            Decision::Current {
                pending_first_launch: true
            }
        ));
        let automatic = Settings {
            mode: Mode::AutomaticStable,
            backend: Backend::Vulkan,
        };
        assert!(
            matches!(automatic.decide(&catalog, Some(installed), None, false), Decision::Stage { package, revoked: false } if package.backend == PackageBackend::Vulkan)
        );
    }

    #[test]
    fn stable_policy_keeps_a_pin_and_never_downgrades_a_healthy_runtime() {
        let catalog = [choice("llama-b10000-kortty1", PackageBackend::Cpu)];
        let automatic = Settings {
            mode: Mode::AutomaticStable,
            backend: Backend::Auto,
        };
        let newer = Installed {
            id: "llama-b11000-kortty1-fixture",
            backend: PackageBackend::Cpu,
            revoked: false,
            pending_first_launch: false,
        };
        assert!(matches!(
            automatic.decide(&catalog, Some(newer), None, false),
            Decision::Current {
                pending_first_launch: false
            }
        ));
        assert!(matches!(
            automatic.decide(&catalog, Some(newer), Some("llama-b10000-kortty1"), false),
            Decision::Current { .. }
        ));
        let revoked = Installed {
            revoked: true,
            ..newer
        };
        assert!(matches!(
            automatic.decide(&catalog, Some(revoked), None, false),
            Decision::Stage { revoked: true, .. }
        ));
        assert!(matches!(
            automatic.decide(&[], Some(revoked), None, false),
            Decision::NoCompatible { revoked: true }
        ));
    }

    #[test]
    fn a_compatible_pin_holds_a_release_and_can_follow_a_backend_switch() {
        let pin = "llama-b12040-kortty1";
        let mut catalog = [
            choice(pin, PackageBackend::Cpu),
            choice(pin, PackageBackend::Vulkan),
            choice("llama-b12041-kortty1", PackageBackend::Cpu),
            choice("llama-b12041-kortty1", PackageBackend::Vulkan),
        ];
        catalog[1].id.push_str("-vulkan");
        let installed = Installed {
            id: &catalog[0].id,
            backend: PackageBackend::Cpu,
            revoked: false,
            pending_first_launch: false,
        };
        let automatic = Settings {
            mode: Mode::AutomaticStable,
            backend: Backend::Auto,
        };
        assert!(automatic.pin_available(&catalog, Some(installed), pin));
        assert!(matches!(
            automatic.decide(&catalog, Some(installed), Some(pin), false),
            Decision::Current { .. }
        ));
        assert!(!automatic.pin_available(&catalog[2..], Some(installed), pin));
        assert!(
            matches!(automatic.decide(&catalog[2..], Some(installed), Some(pin), false), Decision::Stage { package, .. } if package.id == catalog[2].id)
        );
        let switched = Settings {
            backend: Backend::Vulkan,
            ..automatic
        };
        assert!(
            matches!(switched.decide(&catalog, Some(installed), Some(pin), false), Decision::Stage { package, .. } if package.backend == PackageBackend::Vulkan && runtime_id(&package.id) == Some(pin))
        );
    }
}
