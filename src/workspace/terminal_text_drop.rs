use iced::advanced::widget::{Id, Operation, operation::Outcome};
use iced::{Point, Rectangle, window};

use super::Workspace;
use crate::session::SessionId;

#[derive(Clone, Copy)]
pub struct PaneTarget {
    pub(super) tab: u64,
    pub(super) session: SessionId,
    pub(super) stream: u64,
}

pub(super) fn terminal_drop_id(session: SessionId) -> Id {
    Id::from(format!("terminal-native-drop-{}", session.0))
}

pub(super) struct PaneHit {
    point: Option<Point>,
    candidates: Vec<(Id, PaneTarget)>,
    target: Option<PaneTarget>,
}

impl Operation<Option<PaneTarget>> for PaneHit {
    fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn Operation<Option<PaneTarget>>)) {
        if self.target.is_none() && self.point.is_some() {
            children(self);
        }
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if self.target.is_none() && self.point.is_some_and(|point| bounds.contains(point)) {
            self.target = self
                .candidates
                .iter()
                .find_map(|(candidate, target)| (Some(candidate) == id).then_some(*target));
        }
    }

    fn finish(&self) -> Outcome<Option<PaneTarget>> {
        Outcome::Some(self.target)
    }
}

impl PaneHit {
    pub(super) fn new(point: Option<Point>, candidates: Vec<(Id, PaneTarget)>) -> Self {
        Self {
            point,
            candidates,
            target: None,
        }
    }
}

impl Workspace {
    pub(super) fn text_drop_target_current(&self, owner: window::Id, target: PaneTarget) -> bool {
        self.external_text_drop_allowed(owner, target.tab, target.session, target.stream)
            && self.owner_dialog(owner).is_none()
            && !self.reopen_busy(owner)
    }

    pub(super) fn pane_drop_candidates(
        &self,
        owner: window::Id,
        allowed: impl Fn(PaneTarget) -> bool,
    ) -> Vec<(Id, PaneTarget)> {
        if self.owner_dialog(owner).is_some() || self.reopen_busy(owner) {
            return Vec::new();
        }
        let Some(tab) = self.windows.get(&owner).and_then(super::Window::tab) else {
            return Vec::new();
        };
        tab.grid
            .iter()
            .filter_map(|(_, &session)| {
                let target = PaneTarget {
                    tab: tab.id,
                    session,
                    stream: self.sessions.get(&session)?.stream_generation,
                };
                allowed(target).then(|| (terminal_drop_id(session), target))
            })
            .collect()
    }

    pub(super) fn admit_terminal_text_drop(
        &mut self,
        owner: window::Id,
        target: PaneTarget,
        text: String,
    ) -> bool {
        if text.is_empty() || !self.text_drop_target_current(owner, target) {
            return false;
        }
        let request = self.allocate();
        let window = self.windows.get_mut(&owner).expect("validated drop window");
        let tab = window.tab_mut().expect("validated drop tab");
        let Some(pane) = tab
            .grid
            .iter()
            .find_map(|(pane, &session)| (session == target.session).then_some(*pane))
        else {
            return false;
        };
        tab.focused = pane;
        tab.remote_files.release_keyboard();
        window.cancel_paste();
        let targets = vec![target.session];
        if !self.current_targets(owner, target.tab, &targets) {
            return false;
        }
        self.review_paste(
            owner,
            request,
            target.tab,
            targets,
            Some(text),
            Some(target.stream),
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_hit_selects_the_pane_under_the_pointer() {
        let first = SessionId(1);
        let second = SessionId(2);
        let mut hit = PaneHit {
            point: Some(Point::new(102.0, 14.0)),
            candidates: [first, second]
                .into_iter()
                .map(|session| {
                    (
                        terminal_drop_id(session),
                        PaneTarget {
                            tab: 1,
                            session,
                            stream: 0,
                        },
                    )
                })
                .collect(),
            target: None,
        };
        hit.container(
            Some(&terminal_drop_id(first)),
            Rectangle {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            },
        );
        hit.container(
            None,
            Rectangle {
                x: 100.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            },
        );
        assert!(hit.target.is_none());
        hit.container(
            Some(&terminal_drop_id(second)),
            Rectangle {
                x: 100.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            },
        );
        assert_eq!(hit.target.unwrap().session, second);
    }
}
