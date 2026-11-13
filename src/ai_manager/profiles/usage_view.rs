//! Profile budget settings and the private ledger's current-period readout.

use std::collections::BTreeMap;
use std::sync::Arc;

use iced::widget::{button, column, progress_bar, row, text, text_input};
use iced::{Element, Length};

use crate::local_ai::{UsageBudget, UsageSnapshot, UsageWarning};

use super::{Message, Profile, ProfileId, ProfileRef};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Field {
    Limit,
    Days,
    Yellow,
    Red,
    InputPrice,
    OutputPrice,
    Currency,
}

pub(crate) struct Loaded {
    pub(crate) reference: ProfileRef,
    pub(crate) reset_days: u16,
    pub(crate) result: Result<UsageSnapshot, String>,
}

#[derive(Default)]
pub(super) struct Editor {
    invalid: BTreeMap<(ProfileId, Field), String>,
    loaded: Option<Arc<Loaded>>,
    loading: bool,
    reload: bool,
}

impl Editor {
    pub(super) fn invalid(&self) -> bool {
        !self.invalid.is_empty()
    }

    pub(super) fn forget(&mut self, id: ProfileId) {
        self.invalid.retain(|(profile, _), _| *profile != id);
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| {
                matches!(loaded.reference, ProfileRef::Numeric(saved) if ProfileId::from(saved) == id)
            })
        {
            self.loaded = None;
        }
    }

    pub(super) fn reset_for_catalog(&mut self) {
        self.invalid.clear();
        self.loaded = None;
        self.reload |= self.loading;
    }

    pub(super) fn finish(&mut self, loaded: Arc<Loaded>, current: bool) -> bool {
        if !std::mem::take(&mut self.loading) {
            return false;
        }
        let reload = std::mem::take(&mut self.reload);
        if current && !reload {
            self.loaded = Some(loaded);
        }
        reload
    }

    pub(super) fn begin(&mut self) -> bool {
        self.loaded = None;
        if self.loading {
            self.reload = true;
            return false;
        }
        self.loading = true;
        true
    }

    pub(super) fn edit(&mut self, profile: &mut Profile, field: Field, value: String) {
        if value.len() > 32 {
            return;
        }
        let key = (profile.id, field);
        if update(&mut profile.usage_budget, field, value.trim()).is_some() {
            self.invalid.remove(&key);
        } else {
            self.invalid.insert(key, value);
        }
    }

    pub(super) fn view<'a>(&'a self, profile: &'a Profile, editable: bool) -> Element<'a, Message> {
        let budget = &profile.usage_budget;
        let input = |field, label: &'static str, value: String| {
            let value = self
                .invalid
                .get(&(profile.id, field))
                .map_or(value.as_str(), String::as_str);
            column![
                text(label).size(13),
                text_input("", value)
                    .on_input_maybe(
                        editable.then_some(move |value| Message::EditUsage(field, value))
                    )
                    .width(168)
            ]
            .spacing(4)
        };
        column![
            text("Token quota and cost").size(16),
            row![
                input(Field::Limit, "Token quota (blank for unlimited)", budget.token_limit.map_or_else(String::new, |value| value.to_string())),
                input(Field::Days, "Reset period in days", budget.reset_days.to_string()),
                input(Field::Yellow, "First warning (%)", budget.yellow_percent.to_string()),
                input(Field::Red, "Second warning (%)", budget.red_percent.to_string()),
            ].spacing(8).wrap(),
            row![
                input(Field::InputPrice, "Input price per million tokens", price_text(budget.input_micros_per_million)),
                input(Field::OutputPrice, "Output price per million tokens", price_text(budget.output_micros_per_million)),
                input(Field::Currency, "Currency code", budget.currency.clone()),
            ].spacing(8).wrap(),
            text("Quota warnings use provider-reported tokens and pending output reservations. Providers can report incomplete usage. Prices calculate an estimate from reported tokens.").size(13),
            self.recorded(profile, editable),
        ].spacing(8).into()
    }

    pub(super) fn recorded<'a>(
        &'a self,
        profile: &'a Profile,
        can_load: bool,
    ) -> Element<'a, Message> {
        let budget = &profile.usage_budget;
        let reference = profile.reference();
        let snapshot = self.loaded.as_ref().filter(|loaded| {
            loaded.reference == reference && loaded.reset_days == budget.reset_days
        });
        column![
            button(if self.loading {
                "Reading recorded usage…"
            } else {
                "Refresh recorded usage"
            })
            .style(button::secondary)
            .on_press_maybe((can_load && !self.loading).then_some(Message::LoadUsage)),
            snapshot.map(|loaded| match &loaded.result {
                Ok(snapshot) => summary(budget, *snapshot),
                Err(error) => column![text(error).size(13)].into(),
            }),
        ]
        .spacing(8)
        .into()
    }
}

fn update(budget: &mut UsageBudget, field: Field, value: &str) -> Option<()> {
    match field {
        Field::Limit => {
            budget.token_limit = if value.is_empty() {
                None
            } else {
                Some(value.parse::<u64>().ok().filter(|value| *value > 0)?)
            }
        }
        Field::Days => {
            budget.reset_days = value
                .parse::<u16>()
                .ok()
                .filter(|value| (1..=3660).contains(value))?
        }
        Field::Yellow => {
            budget.yellow_percent = value.parse::<u8>().ok().filter(|value| *value <= 100)?
        }
        Field::Red => {
            budget.red_percent = value.parse::<u8>().ok().filter(|value| *value <= 100)?
        }
        Field::InputPrice => budget.input_micros_per_million = price(value)?,
        Field::OutputPrice => budget.output_micros_per_million = price(value)?,
        Field::Currency
            if value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_alphabetic()) =>
        {
            budget.currency = value.to_ascii_uppercase()
        }
        Field::Currency => return None,
    }
    Some(())
}

fn price(value: &str) -> Option<Option<u64>> {
    if value.is_empty() {
        return Some(None);
    }
    let (whole, decimals) = value.split_once(['.', ',']).unwrap_or((value, ""));
    if whole.is_empty()
        || decimals.len() > 6
        || !whole
            .bytes()
            .chain(decimals.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.parse::<u64>().ok()?.checked_mul(1_000_000)?;
    let fraction = if decimals.is_empty() {
        0
    } else {
        decimals.parse::<u64>().ok()? * 10u64.pow(6 - decimals.len() as u32)
    };
    whole
        .checked_add(fraction)
        .filter(|value| *value <= 1_000_000_000_000)
        .map(Some)
}

fn price_text(micros: Option<u64>) -> String {
    let Some(micros) = micros else {
        return String::new();
    };
    let decimal = micros % 1_000_000;
    if decimal == 0 {
        return (micros / 1_000_000).to_string();
    }
    format!("{}.{decimal:06}", micros / 1_000_000)
        .trim_end_matches('0')
        .to_owned()
}

fn summary(budget: &UsageBudget, snapshot: UsageSnapshot) -> Element<'_, Message> {
    let counts = snapshot.counts;
    let period = |days| {
        jiff::civil::date(1970, 1, 1)
            .checked_add(jiff::Span::new().days(days))
            .map_or_else(|_| "Unavailable".into(), |date| date.to_string())
    };
    column![
        text(format!("Period starts {} · Next reset {}", period(snapshot.start_day), period(snapshot.next_day))).size(13),
        text(format!("Reported tokens · {} input · {} output", counts.input_tokens, counts.output_tokens)).size(13),
        text(format!("{} request attempts · {} interrupted · {} missing input usage · {} missing output usage", counts.requests, counts.interrupted, counts.missing_input, counts.missing_output)).size(13),
        snapshot.recording_issue.then(|| text("Some attempts could not confirm usage recording during this app run.").size(13)),
        (snapshot.in_flight != 0).then(|| text(format!("{} in flight · {} output tokens reserved", snapshot.in_flight, snapshot.reserved_output)).size(13)),
        quota_meter(budget, snapshot),
        budget.estimated_cost(counts).map(|cost| {
            let cents = cost.numerator / 10_000_000_000;
            let amount = if cost.numerator != 0 && cents == 0 { "< 0.01".into() } else { format!("{}.{:02}", cents / 100, cents % 100) };
            let label = if cost.partial || snapshot.recording_issue { "Partial calculated cost" } else { "Calculated cost" };
            text(format!("{label} · {} {amount}", budget.currency)).size(13)
        }),
    ].spacing(4).into()
}

fn quota_meter(budget: &UsageBudget, snapshot: UsageSnapshot) -> Option<Element<'static, Message>> {
    let limit = u128::from(budget.token_limit?);
    let reported =
        u128::from(snapshot.counts.input_tokens) + u128::from(snapshot.counts.output_tokens);
    let reserved = u128::from(snapshot.reserved_output);
    let used = reported + reserved;
    let fraction = (used.min(limit) as f64 / limit as f64) as f32;
    let (warning, style): (_, fn(&iced::Theme) -> progress_bar::Style) =
        match budget.warning(snapshot, 0) {
            Some(UsageWarning::Red) => ("Second quota warning", progress_bar::danger),
            Some(UsageWarning::Yellow) => ("First quota warning", progress_bar::warning),
            None => ("Below quota warnings", progress_bar::primary),
        };
    let usage = if reserved == 0 {
        format!("{reported} of {limit} tokens")
    } else {
        format!("{reported} reported + {reserved} reserved of {limit} tokens")
    };
    Some(
        column![
            progress_bar(0.0..=1.0, fraction)
                .length(Length::Fill)
                .girth(8)
                .style(style),
            text(format!("{warning} · {usage}"))
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph),
        ]
        .spacing(4)
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage_fixture() -> UsageSnapshot {
        let mut snapshot = UsageSnapshot {
            counts: Default::default(),
            start_day: 20_000,
            next_day: 20_030,
            in_flight: 1,
            reserved_output: 20,
            recording_issue: false,
        };
        snapshot.counts.input_tokens = 41;
        snapshot.counts.output_tokens = 34;
        snapshot
    }

    #[test]
    fn quota_usage_drops_a_read_made_before_catalog_replacement() {
        let mut editor = Editor::default();
        let loaded = |reset_days| {
            Arc::new(Loaded {
                reference: ProfileRef::Numeric(1),
                reset_days,
                result: Ok(usage_fixture()),
            })
        };
        assert!(editor.begin());
        editor.reset_for_catalog();
        assert!(!editor.begin());
        assert!(editor.finish(loaded(30), true));
        assert!(editor.loaded.is_none());
        assert!(editor.begin());
        assert!(!editor.finish(loaded(7), true));
        assert_eq!(editor.loaded.as_ref().unwrap().reset_days, 7);
    }

    #[test]
    fn profile_quota_meter_fills_narrow_and_wide_rendered_rows() {
        use iced::advanced::{Layout, layout, widget::Tree};
        use iced::{Font, Renderer, Size};

        let budget = UsageBudget {
            token_limit: Some(100),
            ..UsageBudget::default()
        };
        let snapshot = usage_fixture();
        assert_eq!(budget.warning(snapshot, 0), Some(UsageWarning::Red));
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        for width in [150.0, 480.0] {
            let size = Size::new(width, 180.0);
            let mut view = quota_meter(&budget, snapshot).unwrap();
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let layout = Layout::new(&node);
            let mut children = layout.children();
            let meter = children.next().unwrap().bounds();
            let description = children.next().unwrap().bounds();
            assert_eq!(meter.width, width);
            assert_eq!(meter.height, 8.0);
            assert!(description.width <= width);
            assert!(description.y >= meter.y + meter.height);
            assert!(description.height > 0.0 && description.y + description.height <= size.height);
        }
        assert!(quota_meter(&UsageBudget::default(), snapshot).is_none());
    }

    #[test]
    fn prices_preserve_zero_and_exact_fractional_units() {
        for (value, expected) in [
            ("", None),
            ("0", Some(0)),
            ("1,005", Some(1_005_000)),
            ("2.000001", Some(2_000_001)),
        ] {
            assert_eq!(price(value), Some(expected));
            assert_eq!(price(&price_text(expected)), Some(expected));
        }
        assert_eq!(price("-1"), None);
        assert_eq!(price("0.0000001"), None);
    }
}
