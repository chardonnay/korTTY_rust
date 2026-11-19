//! Resizable summary above an AI source comparison.

use iced::widget::{canvas, column, container, responsive, scrollable};
use iced::{Element, Length};

use crate::horizontal_divider::{self, Grow};

const BOUNDS: std::ops::RangeInclusive<u16> = 20..=900;

#[derive(Clone, Copy)]
pub enum Event {
    Resize(u16),
    Finish,
}

pub(crate) fn valid(parts: u16) -> bool {
    BOUNDS.contains(&parts)
}

pub(crate) fn parts(ratio: f64) -> Option<u16> {
    ratio.is_finite().then(|| {
        (ratio * 1000.0)
            .round()
            .clamp(f64::from(*BOUNDS.start()), f64::from(*BOUNDS.end())) as u16
    })
}

fn bounds(height: f32) -> (f32, f32) {
    let minimum = 24.0_f32.min(height * 0.35);
    let maximum = (height - 100.0_f32.min(height * 0.5)).max(minimum);
    (minimum, maximum)
}

fn extent(height: f32, estimate: f32, preferred: Option<u16>) -> f32 {
    let (minimum, maximum) = bounds(height);
    preferred
        .map(|parts| height * f32::from(parts) / 1000.0)
        .unwrap_or(estimate.min(height / 3.0))
        .clamp(minimum, maximum)
}

pub(crate) fn view<'a, Message: Clone + 'a>(
    preferred: Option<u16>,
    estimate: f32,
    summary: impl Fn() -> Element<'a, Message> + 'a,
    diff: impl Fn() -> Element<'a, Message> + 'a,
    event: impl Fn(Event) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    responsive(move |size| {
        let available = (size.height - horizontal_divider::WIDTH).max(0.0);
        let height = extent(available, estimate, preferred);
        let (minimum, maximum) = bounds(available);
        let divider = canvas(horizontal_divider::program(
            height,
            Grow::Down,
            move |requested| {
                let ratio = requested.clamp(minimum, maximum) / available.max(1.0);
                event(Event::Resize(
                    parts(f64::from(ratio)).unwrap_or(*BOUNDS.start()),
                ))
            },
            Some(event(Event::Finish)),
        ))
        .width(Length::Fill)
        .height(horizontal_divider::WIDTH);
        column![
            scrollable(summary()).height(height).width(Length::Fill),
            divider,
            container(diff()).height(Length::Fill).width(Length::Fill),
        ]
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    })
    .into()
}

pub(crate) fn update(drag: &mut Option<u16>, event: Event) -> Option<u16> {
    match event {
        Event::Resize(value) if valid(value) => {
            *drag = Some(value);
            None
        }
        Event::Finish => drag.take(),
        Event::Resize(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_fits_its_estimate_and_keeps_a_comparison_on_short_windows() {
        assert_eq!(extent(600.0, 32.0, None), 32.0);
        assert_eq!(extent(600.0, 500.0, None), 200.0);
        assert_eq!(extent(600.0, 32.0, Some(500)), 300.0);
        assert_eq!(extent(80.0, 32.0, Some(900)), 40.0);
        let mut drag = None;
        assert_eq!(update(&mut drag, Event::Finish), None);
        assert_eq!(update(&mut drag, Event::Resize(450)), None);
        assert_eq!(update(&mut drag, Event::Resize(1000)), None);
        assert_eq!(update(&mut drag, Event::Finish), Some(450));
        assert_eq!(update(&mut drag, Event::Finish), None);
    }
}
