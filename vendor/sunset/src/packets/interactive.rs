//! RFC 4256 messages, bounded before exposing any server-controlled prompts.

use core::fmt;

#[cfg(feature = "arbitrary")]
use arbitrary::Arbitrary;
use sunset_sshwire_derive::{SSHDecode, SSHEncode};

use crate::sshwire::{
    SSHDecode, SSHEncode, SSHSink, SSHSource, TextString, WireError, WireResult,
};

const MAX_PROMPTS: usize = 16;
const MAX_TEXT: usize = 4096;
const MAX_TOTAL_TEXT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, SSHDecode, SSHEncode)]
#[cfg_attr(feature = "arbitrary", derive(Arbitrary))]
pub struct InteractivePrompt<'a> {
    pub text: TextString<'a>,
    pub echo: bool,
}

#[derive(Debug)]
#[cfg_attr(feature = "arbitrary", derive(Arbitrary))]
pub struct UserauthInfoRequest<'a> {
    pub name: TextString<'a>,
    pub instruction: TextString<'a>,
    pub language: TextString<'a>,
    prompts: [Option<InteractivePrompt<'a>>; MAX_PROMPTS],
}

impl<'a> UserauthInfoRequest<'a> {
    pub fn prompts(&self) -> impl Iterator<Item = &InteractivePrompt<'a>> {
        self.prompts.iter().flatten()
    }
}

impl<'de: 'a, 'a> SSHDecode<'de> for UserauthInfoRequest<'a> {
    fn dec<S: SSHSource<'de>>(source: &mut S) -> WireResult<Self> {
        let mut budget = MAX_TOTAL_TEXT;
        let name = checked_text(source, &mut budget)?;
        let instruction = checked_text(source, &mut budget)?;
        let language = checked_text(source, &mut budget)?;
        let count = read_count(source)?;
        let mut prompts = [None; MAX_PROMPTS];
        for slot in &mut prompts[..count] {
            let text = checked_text(source, &mut budget)?;
            *slot = Some(InteractivePrompt {
                text,
                echo: bool::dec(source)?,
            });
        }
        Ok(Self {
            name,
            instruction,
            language,
            prompts,
        })
    }
}

impl SSHEncode for UserauthInfoRequest<'_> {
    fn enc(&self, sink: &mut dyn SSHSink) -> WireResult<()> {
        self.name.enc(sink)?;
        self.instruction.enc(sink)?;
        self.language.enc(sink)?;
        (self.prompts().count() as u32).enc(sink)?;
        for prompt in self.prompts() {
            prompt.enc(sink)?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "arbitrary", derive(Arbitrary))]
pub struct UserauthInfoResponse<'a> {
    responses: [Option<TextString<'a>>; MAX_PROMPTS],
}

impl<'a> UserauthInfoResponse<'a> {
    pub(crate) fn new(responses: &[&'a str]) -> WireResult<Self> {
        if responses.len() > MAX_PROMPTS {
            return Err(WireError::BadNumber);
        }
        let mut budget = MAX_TOTAL_TEXT;
        let mut result = Self {
            responses: [None; MAX_PROMPTS],
        };
        for (slot, response) in result.responses.iter_mut().zip(responses) {
            let text = TextString::from(*response);
            validate_text(text, &mut budget)?;
            *slot = Some(text);
        }
        Ok(result)
    }
}

impl fmt::Debug for UserauthInfoResponse<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserauthInfoResponse")
            .finish_non_exhaustive()
    }
}

impl<'de: 'a, 'a> SSHDecode<'de> for UserauthInfoResponse<'a> {
    fn dec<S: SSHSource<'de>>(source: &mut S) -> WireResult<Self> {
        let mut budget = MAX_TOTAL_TEXT;
        let count = read_count(source)?;
        let mut responses = [None; MAX_PROMPTS];
        for slot in &mut responses[..count] {
            *slot = Some(checked_text(source, &mut budget)?);
        }
        Ok(Self { responses })
    }
}

impl SSHEncode for UserauthInfoResponse<'_> {
    fn enc(&self, sink: &mut dyn SSHSink) -> WireResult<()> {
        (self.responses.iter().flatten().count() as u32).enc(sink)?;
        for response in self.responses.iter().flatten() {
            response.enc(sink)?;
        }
        Ok(())
    }
}

fn read_count<'de>(source: &mut impl SSHSource<'de>) -> WireResult<usize> {
    let count =
        usize::try_from(u32::dec(source)?).map_err(|_| WireError::BadNumber)?;
    if count > MAX_PROMPTS {
        return Err(WireError::BadNumber);
    }
    Ok(count)
}

fn checked_text<'de>(
    source: &mut impl SSHSource<'de>,
    budget: &mut usize,
) -> WireResult<TextString<'de>> {
    let text = TextString::dec(source)?;
    validate_text(text, budget)?;
    Ok(text)
}

fn validate_text(text: TextString<'_>, budget: &mut usize) -> WireResult<()> {
    let size = text.as_ref().len();
    if size > MAX_TEXT || size > *budget || text.to_str().is_err() {
        return Err(WireError::BadString);
    }
    *budget -= size;
    Ok(())
}
