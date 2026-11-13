use std::collections::VecDeque;
use std::time::{Duration, Instant};

use async_channel::{Receiver, TryRecvError};

use crate::terminal_effects::Speed;

use super::Event;

const BULK_BYTES: usize = 4096;
const MAX_PENDING_BYTES: usize = 8192;
// A short interactive write may be paced. Sustained output cannot delay a host reply for minutes.
const MAX_PACING_MILLIS: u64 = 4000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Delay {
    Immediate,
    Character,
    Punctuation,
    LineBreak,
}

impl Delay {
    fn milliseconds(self, speed: Speed) -> u64 {
        let base = match self {
            Self::Immediate => return 0,
            Self::Character => 28.0,
            Self::Punctuation => 120.0,
            Self::LineBreak => 300.0,
        };
        (base / speed.multiplier()).round().max(1.0) as u64
    }
}

struct Span {
    bytes: usize,
    delay: Delay,
}

#[derive(Default)]
pub(super) struct Pacer {
    framing: Framing,
    bytes: VecDeque<u8>,
    spans: VecDeque<Span>,
    arrivals: VecDeque<(usize, Instant)>,
    held: Option<Event>,
    speed: Option<Speed>,
    queued_millis: u64,
    anchor: Option<Instant>,
    deadline: Option<Instant>,
}

impl Pacer {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn poll(
        &mut self,
        events: &Receiver<Event>,
        speed: Option<Speed>,
        now: Instant,
    ) -> Result<Event, TryRecvError> {
        if self.speed != speed {
            self.speed = speed;
            self.reset_clock();
            self.queued_millis = speed.map_or(0, |speed| {
                self.spans
                    .iter()
                    .map(|span| span.delay.milliseconds(speed))
                    .sum()
            });
        }
        if let Some(event) = self.held.take() {
            return Ok(event);
        }
        if let Some(output) = self.ready(now) {
            return Ok(output);
        }
        for _ in 0..32 {
            match events.try_recv()? {
                Event::Output(bytes, received) => {
                    if let Some(output) = self.offer(bytes, received, now) {
                        return Ok(output);
                    }
                }
                event if flush_before(&event) && !self.bytes.is_empty() => {
                    self.held = Some(event);
                    return Ok(self.flush());
                }
                event => return Ok(event),
            }
        }
        Err(TryRecvError::Empty)
    }

    fn offer(&mut self, bytes: Vec<u8>, received: Instant, now: Instant) -> Option<Event> {
        if bytes.is_empty() {
            return None;
        }
        let bypass = self.speed.is_none()
            || bytes.len() >= BULK_BYTES
            || self.bytes.len() + bytes.len() > MAX_PENDING_BYTES;
        if bypass || self.framing.opaque() {
            self.framing.track(&bytes);
            return Some(self.forward_unpaced(bytes, received));
        }
        let speed = self.speed.unwrap_or(Speed::DEFAULT);
        let mut immediate = false;
        for (index, &byte) in bytes.iter().enumerate() {
            // Queries must not sit behind a paced backlog awaiting their own host reply.
            immediate |= matches!(self.framing.phase, Phase::Csi)
                && matches!(byte, b'c' | b'n' | b'p' | b't' | b'x');
            let delay = self.framing.step(byte, bytes.get(index + 1).copied());
            immediate |= self.framing.opaque();
            if immediate {
                continue;
            }
            self.queued_millis += delay.milliseconds(speed);
            if delay == Delay::Immediate
                && let Some(last) = self.spans.back_mut().filter(|span| span.delay == delay)
            {
                last.bytes += 1;
            } else {
                self.spans.push_back(Span { bytes: 1, delay });
            }
        }
        // OSC/APC/DCS/SOS/PM carry clipboard, graphics and other protocol state. Process their
        // transport reads immediately, including continuations, with any earlier bytes first.
        if immediate {
            return Some(self.forward_unpaced(bytes, received));
        }
        self.arrivals.push_back((bytes.len(), received));
        self.bytes.extend(bytes);
        self.ready(now)
    }

    fn forward_unpaced(&mut self, bytes: Vec<u8>, received: Instant) -> Event {
        if self.bytes.is_empty() {
            self.spans.clear();
            self.queued_millis = 0;
            return Event::Output(bytes, received);
        }
        self.arrivals.push_back((bytes.len(), received));
        self.bytes.extend(bytes);
        self.flush()
    }

    fn ready(&mut self, now: Instant) -> Option<Event> {
        if self.bytes.is_empty() {
            return None;
        }
        let Some(speed) = self.speed else {
            return Some(self.flush());
        };
        if self.queued_millis > MAX_PACING_MILLIS {
            return Some(self.flush());
        }
        let mut count = 0;
        while let Some(span) = self.spans.front() {
            let millis = span.delay.milliseconds(speed);
            if millis != 0 {
                let deadline = *self.deadline.get_or_insert_with(|| {
                    self.anchor.unwrap_or(now) + Duration::from_millis(millis)
                });
                if deadline > now {
                    break;
                }
                self.anchor = Some(deadline);
                self.deadline = None;
            }
            self.queued_millis -= millis;
            count += self.spans.pop_front().expect("checked span").bytes;
        }
        if self.spans.is_empty() {
            self.reset_clock();
        }
        (count != 0).then(|| self.take(count))
    }

    fn flush(&mut self) -> Event {
        self.spans.clear();
        self.queued_millis = 0;
        self.reset_clock();
        self.take(self.bytes.len())
    }

    fn reset_clock(&mut self) {
        self.anchor = None;
        self.deadline = None;
    }

    fn take(&mut self, count: usize) -> Event {
        let mut remaining = count;
        let mut received = self
            .arrivals
            .front()
            .expect("queued bytes have an arrival time")
            .1;
        while remaining != 0 {
            let (bytes, at) = self
                .arrivals
                .front_mut()
                .expect("queued byte count is exact");
            let consumed = remaining.min(*bytes);
            *bytes -= consumed;
            remaining -= consumed;
            received = *at;
            if *bytes == 0 {
                self.arrivals.pop_front();
            }
        }
        Event::Output(self.bytes.drain(..count).collect(), received)
    }
}

fn flush_before(event: &Event) -> bool {
    matches!(
        event,
        Event::Recording(_)
            | Event::Journal(_)
            | Event::TerminalCommand(..)
            | Event::CaptureReconnectHistory(_)
            | Event::PrependReconnectHistory(..)
            | Event::DisconnectSerial(_)
            | Event::ReadClosed
            | Event::IoError(_)
    )
}

#[derive(Clone, Copy, Default)]
enum Phase {
    #[default]
    Ground,
    Escape,
    Csi,
    Opaque {
        osc: bool,
        escaped: bool,
    },
}

#[derive(Default)]
struct Framing {
    phase: Phase,
    utf8_left: u8,
    possible_c1: bool,
    crlf: bool,
}

impl Framing {
    fn track(&mut self, bytes: &[u8]) {
        if matches!(self.phase, Phase::Ground)
            && self.utf8_left == 0
            && !self.crlf
            && bytes.is_ascii()
            && !bytes.contains(&0x1b)
        {
            return;
        }
        let mut index = 0;
        while index < bytes.len() {
            if matches!(self.phase, Phase::Ground) && self.utf8_left == 0 && !self.crlf {
                let Some(next_control) = bytes[index..]
                    .iter()
                    .position(|&byte| byte == 0x1b || !byte.is_ascii())
                else {
                    return;
                };
                index += next_control;
            }
            self.step(bytes[index], bytes.get(index + 1).copied());
            index += 1;
        }
    }

    fn opaque(&self) -> bool {
        matches!(self.phase, Phase::Opaque { .. })
    }

    fn step(&mut self, byte: u8, next: Option<u8>) -> Delay {
        let lf_after_cr = std::mem::take(&mut self.crlf) && byte == b'\n';
        if self.utf8_left != 0 && (0x80..=0xbf).contains(&byte) {
            self.utf8_left -= 1;
            if std::mem::take(&mut self.possible_c1) {
                if byte <= 0x9f {
                    self.control(byte);
                } else if self.opaque() {
                    self.control(0xc2);
                }
            }
            return Delay::Immediate;
        }
        if self.possible_c1 && self.opaque() {
            self.control(0xc2);
        }
        self.utf8_left = match byte {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => 0,
        };
        self.possible_c1 = byte == 0xc2;
        if self.utf8_left != 0 {
            if !self.possible_c1 && self.opaque() {
                self.control(byte);
            }
            return if matches!(self.phase, Phase::Ground)
                && !(self.possible_c1 && next.is_none_or(|byte| (0x80..=0x9f).contains(&byte)))
            {
                Delay::Character
            } else {
                Delay::Immediate
            };
        }
        if !matches!(self.phase, Phase::Ground) || !byte.is_ascii_graphic() && byte != b' ' {
            let ground = matches!(self.phase, Phase::Ground);
            self.control(byte);
            if ground && byte == b'\r' && next == Some(b'\n') {
                self.crlf = true;
                return Delay::LineBreak;
            }
            return if ground && byte == b'\n' && !lf_after_cr {
                Delay::LineBreak
            } else {
                Delay::Immediate
            };
        }
        if matches!(byte, b':' | b';' | b',' | b'.' | b'?' | b'!') {
            Delay::Punctuation
        } else {
            Delay::Character
        }
    }

    fn control(&mut self, byte: u8) {
        self.phase = match self.phase {
            Phase::Opaque { osc, escaped } => {
                if byte == 0x9c
                    || matches!(byte, 0x18 | 0x1a)
                    || (osc && byte == 0x07)
                    || (escaped && byte == b'\\')
                {
                    Phase::Ground
                } else {
                    Phase::Opaque {
                        osc,
                        escaped: byte == 0x1b && !escaped,
                    }
                }
            }
            _ if byte == 0x1b => Phase::Escape,
            _ if matches!(byte, 0x90 | 0x98 | 0x9d..=0x9f) => Phase::Opaque {
                osc: byte == 0x9d,
                escaped: false,
            },
            Phase::Escape => match byte {
                b'[' => Phase::Csi,
                b']' | b'P' | b'X' | b'^' | b'_' => Phase::Opaque {
                    osc: byte == b']',
                    escaped: false,
                },
                0x20..=0x2f => Phase::Escape,
                _ => Phase::Ground,
            },
            Phase::Csi if !matches!(byte, 0x18 | 0x1a | 0x40..=0x7e) => Phase::Csi,
            _ => Phase::Ground,
        };
    }
}

#[cfg(test)]
mod tests {
    use async_channel::{Sender, bounded};

    use super::*;

    fn send(events: &Sender<Event>, bytes: &[u8], at: Instant) {
        assert!(events.try_send(Event::Output(bytes.to_vec(), at)).is_ok());
    }

    fn output(event: Result<Event, TryRecvError>) -> Vec<u8> {
        match event {
            Ok(Event::Output(bytes, _)) => bytes,
            _ => panic!("expected output"),
        }
    }

    #[test]
    fn typewriter_rhythm_keeps_multibyte_characters_and_crlf_together() {
        let (send_events, events) = bounded(8);
        let mut pacer = Pacer::default();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        send(&send_events, "Aé!\r\n".as_bytes(), start);
        assert!(matches!(
            pacer.poll(&events, Some(Speed::DEFAULT), at(0)),
            Err(TryRecvError::Empty)
        ));
        assert_eq!(pacer.deadline(), Some(at(28)));
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), at(28))),
            b"A"
        );
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), at(56))),
            "é".as_bytes()
        );
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), at(176))),
            b"!"
        );
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), at(476))),
            b"\r\n"
        );
        assert_eq!(pacer.deadline(), None);
        assert_eq!(
            Delay::Character.milliseconds(Speed::try_from(99.0).unwrap()),
            1
        );
        assert_eq!(
            Delay::LineBreak.milliseconds(Speed::try_from(10.0).unwrap()),
            30
        );
    }

    #[test]
    fn typewriter_paces_output_at_the_fractional_effect_multiplier() {
        let (send_events, events) = bounded(8);
        let mut pacer = Pacer::default();
        let speed = Some(Speed::try_from(2.5).unwrap());
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        send(&send_events, b"A!\n", start);
        assert!(matches!(
            pacer.poll(&events, speed, at(0)),
            Err(TryRecvError::Empty)
        ));
        assert_eq!(pacer.deadline(), Some(at(11)));
        assert_eq!(output(pacer.poll(&events, speed, at(11))), b"A");
        assert_eq!(output(pacer.poll(&events, speed, at(59))), b"!");
        assert_eq!(output(pacer.poll(&events, speed, at(179))), b"\n");
    }

    #[test]
    fn opaque_control_reads_bypass_pacing_across_chunks_and_enablement() {
        let (send_events, events) = bounded(8);
        let mut pacer = Pacer::default();
        let now = Instant::now();
        send(&send_events, b"\x1b[3", now);
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"\x1b[3"
        );
        send(&send_events, b"1mX", now);
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"1m"
        );
        send(&send_events, b"\x1b]52;c;opaque", now);
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"X\x1b]52;c;opaque"
        );
        send(&send_events, b"\x1b[invalid escape, still opaque\x1b", now);
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"\x1b[invalid escape, still opaque\x1b"
        );
        send(&send_events, b"\\", now);
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"\\"
        );

        send(&send_events, b"\xc2", now);
        assert_eq!(output(pacer.poll(&events, None, now)), b"\xc2");
        send(&send_events, b"\x9fG,APC\xc2", now);
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"\x9fG,APC\xc2"
        );
        send(&send_events, b"\x9c", now);
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"\x9c"
        );
        send(&send_events, b"T", now);
        assert!(matches!(
            pacer.poll(&events, Some(Speed::DEFAULT), now),
            Err(TryRecvError::Empty)
        ));
        assert_eq!(output(pacer.poll(&events, None, now)), b"T");
    }

    #[test]
    fn pending_output_yields_to_interaction_and_flushes_before_exit_or_bulk() {
        let (send_events, events) = bounded(8);
        let mut pacer = Pacer::default();
        let now = Instant::now();
        send(&send_events, b"slow", now);
        assert!(send_events.try_send(Event::ViewChanged).is_ok());
        assert!(matches!(
            pacer.poll(&events, Some(Speed::DEFAULT), now),
            Ok(Event::ViewChanged)
        ));
        assert!(send_events.try_send(Event::ReadClosed).is_ok());
        assert_eq!(
            output(pacer.poll(&events, Some(Speed::DEFAULT), now)),
            b"slow"
        );
        assert!(matches!(
            pacer.poll(&events, Some(Speed::DEFAULT), now),
            Ok(Event::ReadClosed)
        ));

        send(&send_events, b"slow", now);
        send(&send_events, &vec![b'x'; BULK_BYTES], now);
        let bulk = output(pacer.poll(&events, Some(Speed::DEFAULT), now));
        assert_eq!(bulk.len(), BULK_BYTES + 4);
        assert_eq!(&bulk[..4], b"slow");
        assert_eq!(pacer.deadline(), None);
    }
}
