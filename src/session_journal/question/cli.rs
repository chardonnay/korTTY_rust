//! One explicit CLI answer for the exact reviewed journal evidence.

use std::io;
use std::time::Instant;

use zeroize::Zeroizing;

use crate::local_ai::{Cancel, Error, Profile, Session, cli::Request};

use super::{
    ANSWER_OUTPUT_LIMIT, Answer, CALL_TIMEOUT, Review, check_cancel, encode, invalid, parse_answer,
};

const NEEDS_MASKING: &str = "The rendered CLI request still contains text that needs masking. Choose a different profile or prepare the question again.";
const CHANGED: &str =
    "The reviewed CLI journal evidence or profile changed. Prepare the question again.";

impl Review {
    pub(crate) fn prepare_cli_answer(
        &mut self,
        profile: &Profile,
        selection_limit: usize,
        mask: &impl Fn(&str) -> io::Result<String>,
    ) -> io::Result<Request> {
        if !profile.is_cli() {
            return Err(invalid("Choose a saved CLI profile for this review."));
        }
        self.mask_outbound(profile, selection_limit, mask)?;
        let [system, input] = self.answer_messages();
        let request = profile
            .prepare_cli_chat(system, &[], input)
            .map_err(|error| io::Error::other(error.message()))?;
        let command = Zeroizing::new(request.command_review());
        let fields = [system, input, &command, request.primary().1]
            .into_iter()
            .chain(request.secondary())
            .chain(request.reviewed_arguments());
        for field in fields {
            if Zeroizing::new(mask(field)?).as_str() != field {
                return Err(invalid(NEEDS_MASKING));
            }
        }
        Ok(request)
    }
}

pub(crate) struct CliAnswering {
    profile: Profile,
    session: Session,
    request: Request,
}

impl CliAnswering {
    pub(crate) fn begin(profile: Profile, request: Request) -> io::Result<(Self, Cancel)> {
        if !profile.is_cli() {
            return Err(invalid(CHANGED));
        }
        let (session, cancel) = Session::begin(
            profile.clone().with_output_limit(ANSWER_OUTPUT_LIMIT),
        )
        .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "Local AI workers are busy"))?;
        Ok((
            Self {
                profile,
                session,
                request,
            },
            cancel,
        ))
    }

    pub(crate) fn answer(
        self,
        review: &Review,
        cancelled: impl Fn() -> bool,
        authorize: impl Fn() -> Result<(), String>,
        mask: impl Fn(&str) -> io::Result<String>,
    ) -> io::Result<Answer> {
        let client = self.session.client();
        let cancelled = || cancelled() || !client.active();
        if review.outbound_profile.as_ref() != Some(&self.profile)
            || *encode(&review.question, &review.citations)? != *review.prompt
        {
            return Err(invalid(CHANGED));
        }
        if review.citations.is_empty() {
            return Ok(review.fallback());
        }
        let guard = || {
            review
                .verify(&cancelled)
                .map_err(|error| error.to_string())?;
            authorize()?;
            check_cancel(&cancelled).map_err(|error| error.to_string())
        };
        guard().map_err(io::Error::other)?;
        let reply = client.complete_cli_request_before_guarded(
            self.request,
            Instant::now() + CALL_TIMEOUT,
            guard,
        );
        check_cancel(&cancelled)?;
        guard().map_err(io::Error::other)?;
        let reply = match reply {
            Ok(reply) => Zeroizing::new(reply),
            Err(Error::Invalid(_)) => return Ok(review.fallback()),
            Err(error) => return Err(io::Error::other(error.message())),
        };
        let answer = parse_answer(&reply, &review.citations).filter(|answer| {
            answer.findings.iter().all(|finding| {
                mask(&finding.quote)
                    .is_ok_and(|masked| Zeroizing::new(masked).as_str() == finding.quote.as_str())
            })
        });
        Ok(answer.unwrap_or_else(|| review.fallback()))
    }
}
