//! Decodes Harmony frames carried in the content field by managed GPT-OSS.

use zeroize::Zeroizing;

use crate::local_ai::Error;

const START: &str = "<|start|>assistant";
const CHANNEL: &str = "<|channel|>";
const MESSAGE: &str = "<|message|>";
const END: &str = "<|end|>";
const RETURN: &str = "<|return|>";
const CONTROL: [&str; 7] = [
    END,
    RETURN,
    "<|call|>",
    "<|start|>",
    CHANNEL,
    MESSAGE,
    "<|constrain|>",
];
const INVALID: Error = Error::Invalid("invalid GPT-OSS Harmony response");

#[derive(Clone, Copy)]
enum Channel {
    Analysis,
    Commentary,
    Final,
}

enum State {
    Header { first: bool },
    Content(Channel),
    Plain,
    Done,
}

pub(super) struct Decoder {
    state: State,
    pending: Zeroizing<String>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            state: State::Header { first: true },
            pending: Zeroizing::default(),
        }
    }
}

impl Decoder {
    pub(super) fn push(
        &mut self,
        part: &str,
        mut emit: impl FnMut(&str, bool) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut buffer = std::mem::take(&mut self.pending);
        buffer.push_str(part);
        let mut input = buffer.as_str();
        while !input.is_empty() {
            match self.state {
                State::Header { first } => {
                    let header = input.strip_prefix(START).unwrap_or(input);
                    let prefixes = [
                        ("<|channel|>analysis<|message|>", Channel::Analysis),
                        ("<|channel|>commentary<|message|>", Channel::Commentary),
                        ("<|channel|>final<|message|>", Channel::Final),
                    ];
                    if let Some((remaining, channel)) =
                        prefixes.iter().find_map(|(marker, channel)| {
                            header.strip_prefix(marker).map(|tail| (tail, *channel))
                        })
                    {
                        self.state = State::Content(channel);
                        input = remaining;
                    } else if START.starts_with(input)
                        || prefixes
                            .iter()
                            .any(|(marker, _)| marker.starts_with(header))
                    {
                        self.pending.push_str(input);
                        return Ok(());
                    } else if first
                        && !input.starts_with("<|start|>")
                        && !input.starts_with(CHANNEL)
                    {
                        self.state = State::Plain;
                    } else {
                        return Err(INVALID);
                    }
                }
                State::Content(channel) => {
                    let (text, marker) = match input.find("<|") {
                        Some(index) => input.split_at(index),
                        None if input.ends_with('<') => input.split_at(input.len() - 1),
                        None => (input, ""),
                    };
                    emit_channel(channel, text, &mut emit)?;
                    if marker.is_empty() {
                        return Ok(());
                    }
                    if let Some(control) =
                        CONTROL.iter().find(|control| marker.starts_with(**control))
                    {
                        self.state = match (*control, channel) {
                            (END | RETURN, Channel::Final) => State::Done,
                            (END, _) => State::Header { first: false },
                            _ => return Err(INVALID),
                        };
                        input = &marker[control.len()..];
                    } else if CONTROL.iter().any(|control| control.starts_with(marker)) {
                        self.pending.push_str(marker);
                        return Ok(());
                    } else {
                        // An ordinary less-than sign or an unknown marker is content.
                        emit_channel(channel, "<", &mut emit)?;
                        input = &marker[1..];
                    }
                }
                State::Plain => return emit(input, false),
                State::Done if input.trim().is_empty() => return Ok(()),
                State::Done => return Err(INVALID),
            }
        }
        Ok(())
    }

    pub(super) fn finish(
        self,
        mut emit: impl FnMut(&str, bool) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self.state {
            State::Header { first: true } if !self.pending.starts_with("<|") => {
                emit(&self.pending, false)
            }
            State::Content(Channel::Final) if !self.pending.starts_with("<|") => {
                emit(&self.pending, false)
            }
            State::Plain | State::Done => Ok(()),
            _ => Err(INVALID),
        }
    }
}

fn emit_channel(
    channel: Channel,
    text: &str,
    emit: &mut impl FnMut(&str, bool) -> Result<(), Error>,
) -> Result<(), Error> {
    if text.is_empty() {
        return Ok(());
    }
    match channel {
        Channel::Analysis => emit(text, true),
        Channel::Final => emit(text, false),
        Channel::Commentary => Ok(()),
    }
}
