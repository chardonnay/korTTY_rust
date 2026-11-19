//! Explicit assignment of unresolved Java Scheduler connection references.

use std::collections::{BTreeMap, BTreeSet};
use std::{fmt, io};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::import::{
    JavaImportedRoute, JavaLibrarySettings, JavaSchedulerSource, JavaTargetReuse,
    java_connection_candidate, prepare_java_connection_import_with_library,
};
use crate::connection_store::{Connection, Store};
use crate::profile_backup::{java_credential_restore, java_migration::SchedulerSource};
use crate::ssh_dialog::LoginTarget;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Target {
    pub(crate) id: Option<u64>,
    label: String,
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

struct Source {
    id: String,
    label: String,
    route: JavaSchedulerSource,
    selected: Option<u64>,
}

struct Saved {
    choice: Target,
    endpoint: LoginTarget,
    jump: Option<LoginTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GroupTarget(pub(crate) Option<String>);

impl fmt::Display for GroupTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_deref().unwrap_or("Use the archived group path"))
    }
}

struct Group {
    path: String,
    members: Vec<String>,
    choices: Vec<GroupTarget>,
    selected: Option<String>,
}

pub(crate) struct Review {
    catalogue: [u8; 32],
    sources: Vec<Source>,
    saved: Vec<Saved>,
    groups: Vec<Group>,
    routes: BTreeMap<String, JavaSchedulerSource>,
    pub(crate) open: bool,
    pub(crate) page: usize,
}

fn matching(saved: &Saved, source: &JavaSchedulerSource) -> bool {
    saved.endpoint == source.target && saved.jump == source.jump
}

fn endpoint_label(endpoint: &LoginTarget) -> String {
    format!("{}@{}:{}", endpoint.username, endpoint.host, endpoint.port)
}

fn group_bindings(
    saved: &[&Connection],
    members: &[String],
    routes: &BTreeMap<String, JavaSchedulerSource>,
    policy: &ClipboardPolicy,
) -> Option<BTreeMap<String, JavaImportedRoute>> {
    if saved.len() != members.len() {
        return None;
    }
    let mut ids = BTreeSet::new();
    members
        .iter()
        .map(|source| {
            let route = routes.get(source)?;
            let mut matches = saved
                .iter()
                .filter(|profile| profile.matches_route(&route.target, route.jump.as_ref()));
            let profile = matches.next()?;
            if matches.next().is_some()
                || !ids.insert(profile.id)
                || profile.blocked_server(policy.server_access()).is_some()
            {
                return None;
            }
            Some((source.clone(), binding(profile.id, route)))
        })
        .collect()
}

fn binding(id: u64, route: &JavaSchedulerSource) -> JavaImportedRoute {
    JavaImportedRoute {
        id,
        target: route.target.clone(),
        jump: route.jump.clone(),
        rsync_folder: route.rsync_folder.clone(),
    }
}

impl Review {
    pub(in crate::profile_backup) fn read(
        source: &SchedulerSource,
        current: &[u8],
        library: JavaLibrarySettings,
    ) -> io::Result<Option<Self>> {
        let policy = ClipboardPolicy::load();
        let store = java_connection_candidate(current)?;
        let saved: Vec<_> = store
            .connections()
            .iter()
            .filter(|connection| {
                connection.missing_username().is_none()
                    && connection.blocked_server(policy.server_access()).is_none()
            })
            .map(|connection| {
                let name = store.group_of(connection.id).map_or_else(
                    || connection.name.clone(),
                    |group| format!("{group} / {}", connection.name),
                );
                let endpoint = connection.login.endpoint.clone();
                let jump = connection.jump.as_ref().map(|jump| jump.endpoint.clone());
                let mut route = endpoint_label(&endpoint);
                if let Some(jump) = &jump {
                    route.push_str(" via ");
                    route.push_str(&endpoint_label(jump));
                }
                Saved {
                    choice: Target {
                        id: Some(connection.id),
                        label: format!("{name} · {route} · #{}", connection.id),
                    },
                    endpoint,
                    jump,
                }
            })
            .collect();
        if saved.is_empty() {
            return Ok(None);
        }
        let mut import = prepare_java_connection_import_with_library(
            &source.connections,
            Some(current),
            policy.server_access(),
            library,
        )?;
        import.reuse_targets(
            Some(current),
            JavaTargetReuse {
                toolbar: false,
                scheduler: true,
            },
        );
        let mut mentioned = BTreeSet::new();
        let mut mentioned_groups = BTreeSet::new();
        for job in &source.review.jobs {
            mentioned.extend(job.source_targets.iter().map(String::as_str));
            for group in &job.source_groups {
                mentioned_groups.insert(group.as_str());
                if let Some(members) = import.groups.member_ids(group) {
                    mentioned.extend(members);
                }
            }
        }
        let sources: Vec<_> = mentioned
            .into_iter()
            .filter(|id| !import.routes.contains_key(*id))
            .filter_map(|id| {
                let route = import.scheduler_sources.get(id)?.clone();
                saved
                    .iter()
                    .any(|candidate| matching(candidate, &route))
                    .then(|| Source {
                        id: id.into(),
                        label: format!(
                            "{} · {} · Java ID {id}",
                            route.name,
                            endpoint_label(&route.target)
                        ),
                        route,
                        selected: None,
                    })
            })
            .collect();
        let mut saved_groups = BTreeMap::<_, Vec<_>>::new();
        if !mentioned_groups.is_empty() {
            for profile in store.connections() {
                let parents = std::iter::successors(store.group_of(profile.id), |path| {
                    path.rsplit_once('/').map(|(parent, _)| parent)
                });
                for path in parents {
                    let members = saved_groups.entry(path).or_default();
                    if members.len() <= kortty_jobs::MAX_SWARM_TARGETS {
                        members.push(profile);
                    }
                }
            }
        }
        let groups: Vec<_> = mentioned_groups
            .into_iter()
            .filter_map(|path| {
                let members: Vec<_> = import
                    .groups
                    .member_ids(path)?
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                if members.len() > kortty_jobs::MAX_SWARM_TARGETS
                    || members
                        .iter()
                        .any(|id| !import.scheduler_sources.contains_key(id))
                {
                    return None;
                }
                let choices: Vec<_> = std::iter::once(GroupTarget(None))
                    .chain(
                        saved_groups
                            .iter()
                            .filter(|(candidate, saved)| {
                                **candidate != path
                                    && group_bindings(
                                        saved,
                                        &members,
                                        &import.scheduler_sources,
                                        &policy,
                                    )
                                    .is_some()
                            })
                            .map(|(&candidate, _)| GroupTarget(Some(candidate.into()))),
                    )
                    .collect();
                (choices.len() > 1).then(|| Group {
                    path: path.into(),
                    members,
                    choices,
                    selected: None,
                })
            })
            .collect();
        Ok((!sources.is_empty() || !groups.is_empty()).then_some(Self {
            catalogue: java_credential_restore::digest(current),
            sources,
            saved,
            groups,
            routes: import.scheduler_sources,
            open: false,
            page: 0,
        }))
    }

    pub(crate) fn len(&self) -> usize {
        self.sources.len() + self.groups.len()
    }

    pub(crate) fn assignment_count(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| source.selected.is_some())
            .count()
            + self
                .groups
                .iter()
                .filter(|group| group.selected.is_some())
                .count()
    }

    pub(crate) fn row(&self, index: usize) -> (&str, Vec<Target>, Option<Target>) {
        let source = &self.sources[index];
        let choices = std::iter::once(Target {
            id: None,
            label: "Leave unresolved".into(),
        })
        .chain(
            self.saved
                .iter()
                .filter(|saved| matching(saved, &source.route))
                .map(|saved| saved.choice.clone()),
        )
        .collect::<Vec<_>>();
        let selected = choices
            .iter()
            .find(|choice| choice.id == source.selected)
            .cloned();
        (&source.label, choices, selected)
    }

    pub(crate) fn select(&mut self, index: usize, id: Option<u64>) -> io::Result<()> {
        let source = self
            .sources
            .get_mut(index)
            .ok_or_else(|| io::Error::other("The Java Scheduler source changed."))?;
        if id.is_some_and(|id| {
            !self
                .saved
                .iter()
                .any(|saved| saved.choice.id == Some(id) && matching(saved, &source.route))
        }) {
            return Err(io::Error::other(
                "Choose a saved SSH profile with the same main and jump routes.",
            ));
        }
        source.selected = id;
        Ok(())
    }

    pub(crate) fn group_row(
        &self,
        index: usize,
    ) -> Option<(&str, &[GroupTarget], Option<&GroupTarget>)> {
        let group = self.groups.get(index.checked_sub(self.sources.len())?)?;
        let selected = group
            .choices
            .iter()
            .find(|choice| choice.0 == group.selected);
        Some((&group.path, &group.choices, selected))
    }

    pub(crate) fn select_group(&mut self, index: usize, path: Option<String>) -> io::Result<()> {
        let group = index
            .checked_sub(self.sources.len())
            .and_then(|index| self.groups.get_mut(index))
            .ok_or_else(|| io::Error::other("The Java Scheduler group changed."))?;
        if !group.choices.iter().any(|choice| choice.0 == path) {
            return Err(io::Error::other(
                "Choose a saved group with the same complete SSH membership.",
            ));
        }
        group.selected = path;
        Ok(())
    }

    pub(in crate::profile_backup) fn group_path<'a>(&'a self, source: &'a str) -> &'a str {
        self.groups
            .iter()
            .find(|group| group.path == source)
            .and_then(|group| group.selected.as_deref())
            .unwrap_or(source)
    }

    pub(in crate::profile_backup) fn bindings(
        &self,
        before: Option<[u8; 32]>,
        candidate: &Store,
        policy: &ClipboardPolicy,
    ) -> io::Result<BTreeMap<String, JavaImportedRoute>> {
        if self.assignment_count() != 0 && before != Some(self.catalogue) {
            return Err(io::Error::other(
                "Saved connections changed. Open the backup again to review Scheduler targets.",
            ));
        }
        let mut bindings = self
            .sources
            .iter()
            .filter_map(|source| source.selected.map(|id| (source, id)))
            .map(|(source, id)| {
                let route = &source.route;
                let profile = candidate
                    .get(id)
                    .filter(|profile| {
                        profile.matches_route(&route.target, route.jump.as_ref())
                            && profile.blocked_server(policy.server_access()).is_none()
                    })
                    .ok_or_else(|| {
                        io::Error::other("A reviewed Scheduler target is unavailable.")
                    })?;
                Ok((source.id.clone(), binding(profile.id, route)))
            })
            .collect::<io::Result<BTreeMap<_, _>>>()?;
        for group in &self.groups {
            let Some(path) = &group.selected else {
                continue;
            };
            let saved: Vec<_> = candidate
                .in_group(path)
                .take(group.members.len() + 1)
                .collect();
            let members =
                group_bindings(&saved, &group.members, &self.routes, policy).ok_or_else(|| {
                    io::Error::other("A reviewed Scheduler group changed or is unavailable.")
                })?;
            for (source, route) in members {
                if bindings
                    .insert(source, route.clone())
                    .is_some_and(|previous| previous != route)
                {
                    return Err(io::Error::other(
                        "The reviewed Scheduler targets assign different profiles to the same Java connection.",
                    ));
                }
            }
        }
        Ok(bindings)
    }
}
