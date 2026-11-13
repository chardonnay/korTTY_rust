//! Read-only usage projections for reviewed AI requests.

use std::time::{Duration, Instant};

use iced::Task;
use zeroize::Zeroizing;

use crate::local_ai;

const FRESH: Duration = Duration::from_secs(15);
#[cfg(test)]
pub(crate) fn estimate_input<'a>(parts: impl IntoIterator<Item = &'a str>) -> u64 {
    local_ai::Tokenizer::Estimate.count_messages(parts)
}

#[derive(Default)]
pub(crate) enum State {
    #[default]
    Omitted,
    Loading(u64),
    Ready(Instant, String),
    Stale,
}

impl State {
    pub(crate) fn start_messages<'a, T: Send + 'static>(
        &mut self,
        token: u64,
        transport: &local_ai::Profile,
        parts: impl IntoIterator<Item = &'a str>,
        finish: impl FnOnce(String) -> T + Send + 'static,
    ) -> Task<T> {
        if !transport.has_quota() {
            *self = Self::Omitted;
            return Task::none();
        }
        let parts: Vec<_> = parts
            .into_iter()
            .map(|part| Zeroizing::new(part.to_owned()))
            .collect();
        self.start_forecast_with(
            token,
            transport,
            move |transport| {
                transport
                    .quota_forecast(transport.count_input(parts.iter().map(|part| part.as_str())))
            },
            finish,
        )
    }

    pub(crate) fn start_forecast_with<T: Send + 'static>(
        &mut self,
        token: u64,
        transport: &local_ai::Profile,
        forecast: impl FnOnce(&local_ai::Profile) -> Result<String, String> + Send + 'static,
        finish: impl FnOnce(String) -> T + Send + 'static,
    ) -> Task<T> {
        if !transport.has_quota() {
            *self = Self::Omitted;
            return Task::none();
        }
        *self = Self::Loading(token);
        let transport = transport.clone();
        Task::perform(
            async move { forecast(&transport).unwrap_or_else(|error| error) },
            finish,
        )
    }

    pub(crate) fn complete(&mut self, token: u64, forecast: String) {
        if matches!(self, Self::Loading(current) if *current == token) {
            *self = Self::Ready(Instant::now(), forecast);
        }
    }

    pub(crate) fn loading(&self) -> bool {
        matches!(self, Self::Loading(_))
    }

    pub(crate) fn stale(&self) -> bool {
        matches!(self, Self::Stale) || matches!(self, Self::Ready(at, _) if at.elapsed() >= FRESH)
    }

    pub(crate) fn expire(&mut self) {
        if matches!(self, Self::Ready(..)) {
            *self = Self::Stale;
        }
    }

    pub(crate) fn label(&self) -> Option<&str> {
        match self {
            Self::Omitted | Self::Stale => None,
            Self::Loading(_) => Some("Reading quota forecast…"),
            Self::Ready(_, forecast) => Some(forecast),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn java_default_estimate_counts_characters_words_and_message_overhead() {
        assert_eq!(
            super::estimate_input(["界界界界界界界界界界界界界", "a b c d"]),
            22
        );
        assert_eq!(super::estimate_input([" \n\t ", ""]), 12);
    }
}
