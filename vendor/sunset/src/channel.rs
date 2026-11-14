use self::packets::ExitSignal;

#[allow(unused_imports)]
use {
    crate::error::{Error, Result, TrapBug},
    log::{debug, error, info, log, trace, warn},
};

use core::num::NonZeroUsize;
use core::task::Waker;

use heapless::{String, Vec};

use crate::{runner::set_waker, *};
use config::*;
use conn::DispatchEvent;
use event::{CliEventId, ServEventId};
use packets::{
    ChannelData, ChannelDataExt, ChannelOpen, ChannelOpenType, ChannelReqType,
    ChannelRequest, Packet,
};
use runner::ChanHandle;
use sshnames::*;
use sshwire::{BinString, SSHEncodeEnum, TextString};
use traffic::TrafSend;

pub(crate) struct Channels {
    ch: [Option<Channel>; config::MAX_CHANNELS],
    is_client: bool,
}

impl Channels {
    pub fn new(is_client: bool) -> Self {
        Channels { ch: Default::default(), is_client }
    }

    pub fn open<'b>(
        &mut self,
        ty: packets::ChannelOpenType<'b>,
    ) -> Result<(ChanNum, Packet<'b>)> {
        let num = self.unused_chan()?;

        let chan = Channel::new(num, (&ty).into());
        let p = packets::ChannelOpen {
            sender_num: num.0,
            initial_window: chan.recv.window as u32,
            max_packet: chan.recv.max_packet as u32,
            ty,
        }
        .into();
        let ch = &mut self.ch[num.0 as usize];
        let ch = ch.insert(chan);
        Ok((ch.num(), p))
    }

    /// Returns a `Channel` for a local number, any state including `InOpen`.
    fn get_any(&self, num: ChanNum) -> Result<&Channel> {
        self.ch
            .get(num.0 as usize)
            // out of range
            .ok_or(error::BadChannel { num }.build())?
            .as_ref()
            // unused channel
            .ok_or(error::BadChannel { num }.build())
    }

    /// Returns a `Channel` for a local number. Excludes `InOpen` or `Opening` state.
    pub(crate) fn get(&self, num: ChanNum) -> Result<&Channel> {
        let ch = self.get_any(num)?;

        match ch.state {
            ChanState::InOpen | ChanState::Opening => {
                error::BadChannel { num }.fail()
            }
            _ => Ok(ch),
        }
    }

    fn get_any_mut(&mut self, num: ChanNum) -> Result<&mut Channel> {
        self.ch
            .get_mut(num.0 as usize)
            // out of range
            .ok_or(error::BadChannel { num }.build())?
            .as_mut()
            // unused channel
            .ok_or(error::BadChannel { num }.build())
    }

    pub(crate) fn get_mut(&mut self, num: ChanNum) -> Result<&mut Channel> {
        let ch = self.get_any_mut(num)?;

        match ch.state {
            ChanState::InOpen | ChanState::Opening => {
                error::BadChannel { num }.fail()
            }
            _ => Ok(ch),
        }
    }

    pub fn by_handle_mut(&mut self, handle: &ChanHandle) -> &mut Channel {
        self.get_mut(handle.0).unwrap()
    }

    /// Must be called when an application has finished with a channel.
    pub fn done(&mut self, num: ChanNum) -> Result<()> {
        let ch = self.get_mut(num)?;
        debug_assert!(!ch.app_done);
        ch.app_done = true;
        Ok(())
    }

    fn remove_any(&mut self, num: ChanNum) -> Result<()> {
        trace!("remove_any channel {}", num);
        self.ch[num.0 as usize] = None;
        Ok(())
    }

    fn remove(&mut self, num: ChanNum) -> Result<()> {
        // TODO any checks?
        let ch = self.get_any_mut(num)?;
        if ch.app_done {
            trace!("removing channel {}", num);
            self.ch[num.0 as usize] = None;
        } else {
            ch.state = ChanState::PendingDone;
            trace!("not removing channel {}, not finished", num);
        }
        Ok(())
    }

    /// Returns the first available channel
    fn unused_chan(&self) -> Result<ChanNum> {
        self.ch
            .iter()
            .enumerate()
            .find_map(|(i, ch)| {
                if ch.as_ref().is_none() { Some(ChanNum(i as u32)) } else { None }
            })
            .ok_or(Error::NoChannels)
    }

    /// Creates a new channel in InOpen state.
    fn reserve_chan(&mut self, co: &ChannelOpen) -> Result<&mut Channel> {
        let num = self.unused_chan()?;
        let mut chan = Channel::new(num, (&co.ty).into());
        chan.send = Some(ChanDir {
            num: co.sender_num,
            max_packet: co.max_packet as usize,
            window: co.initial_window as usize,
        });
        chan.state = ChanState::InOpen;

        let ch = &mut self.ch[num.0 as usize];
        *ch = Some(chan);
        Ok(ch.as_mut().unwrap())
    }

    /// Returns the channel data packet to send.
    ///
    /// Caller has already checked valid length with send_allowed(), and
    /// validated `dt`.
    /// Don't call with zero length data.
    pub(crate) fn send_data<'b>(
        &mut self,
        num: ChanNum,
        dt: ChanData,
        data: &'b [u8],
    ) -> Result<Packet<'b>> {
        debug_assert!(!data.is_empty());

        let ch = self.get_mut(num)?;
        let send = ch.send.as_mut().trap()?;
        if data.len() > send.max_packet || data.len() > send.window {
            trace!(
                "data len {}, max {}, window {}",
                data.len(),
                send.max_packet,
                send.window
            );
            return Error::bug();
        }
        send.window -= data.len();
        trace!("send_data: new window {}", send.window);
        if send.window == 0 {
            debug!("ch {num} send window empty");
        }

        let data = BinString(data);
        let p = match dt {
            ChanData::Normal => packets::ChannelData { num: send.num, data }.into(),
            ChanData::Stderr => packets::ChannelDataExt {
                num: send.num,
                code: sshnames::SSH_EXTENDED_DATA_STDERR,
                data,
            }
            .into(),
        };

        Ok(p)
    }

    /// Returns receive credit for bytes consumed by the application.
    pub(crate) fn finished_read(
        &mut self,
        num: ChanNum,
        len: usize,
        s: &mut TrafSend,
    ) -> Result<()> {
        let ch = self.get_mut(num)?;
        ch.finished_input(len)?;
        ch.check_send_window_adjust(s);
        Ok(())
    }

    pub(crate) fn have_recv_eof(&self, num: ChanNum) -> bool {
        self.get(num).is_ok_and(|c| c.have_recv_eof())
    }

    pub(crate) fn is_closed(&self, num: ChanNum) -> bool {
        self.get(num).is_ok_and(|c| c.is_closed())
    }

    pub(crate) fn client_request_result(&self, num: ChanNum) -> Result<Option<bool>> {
        Ok(match self.get_any(num)?.request_reply {
            RequestReply::Complete { accepted, .. } => Some(accepted),
            _ => None,
        })
    }

    pub(crate) fn client_exec_after_confirmation(
        &mut self,
        num: ChanNum,
        command: &str,
        send: &mut TrafSend,
    ) -> Result<()> {
        let channel = self.get_mut(num)?;
        if !matches!(channel.request_reply, RequestReply::Complete { accepted: true, pty: true })
            || !matches!(channel.ty, ChanType::Session)
        {
            return error::BadUsage.fail();
        }
        Req::Exec(command).send_with_reply(channel, send, true)?;
        channel.request_reply = RequestReply::Pending { pty: false };
        Ok(())
    }

    pub(crate) fn track_client_session(&mut self, num: ChanNum) -> Result<()> {
        let channel = self.get_any_mut(num)?;
        channel.tracked_session = true;
        Ok(())
    }

    pub(crate) fn client_tracked_request(
        &mut self,
        num: ChanNum,
        request: Req<'_>,
        send: &mut TrafSend,
    ) -> Result<()> {
        let channel = self.get_mut(num)?;
        if !channel.tracked_session || channel.request_reply != RequestReply::Unsent {
            return error::BadUsage.fail();
        }
        let pty = matches!(request, Req::Pty(_));
        request.send_with_reply(channel, send, true)?;
        channel.request_reply = RequestReply::Pending { pty };
        Ok(())
    }

    pub(crate) fn tracked_status(&self, num: ChanNum) -> Result<DirectStatus> {
        let channel = self.get_any(num)?;
        if !matches!(channel.ty, ChanType::Tcp) && !channel.tracked_session {
            return error::BadUsage.fail();
        }
        if let Some(reason) = channel.open_failure {
            return Ok(DirectStatus::Refused(reason));
        }
        if matches!(channel.state, ChanState::Opening) {
            return Ok(DirectStatus::Opening);
        }
        Ok(DirectStatus::Open)
    }

    /// Keeps a closing channel number reserved until the peer answers.
    pub(crate) fn close_tracked(&mut self, num: ChanNum, s: &mut TrafSend) -> Result<bool> {
        match self.tracked_status(num)? {
            DirectStatus::Opening => Ok(false),
            DirectStatus::Refused(_) => {
                self.remove_any(num)?;
                Ok(true)
            }
            DirectStatus::Open => {
                let channel = self.get_mut(num)?;
                if channel.is_closed() {
                    self.remove_any(num)?;
                    Ok(true)
                } else {
                    channel.send_close(s)?;
                    channel.app_done = true;
                    Ok(false)
                }
            }
        }
    }

    pub(crate) fn discard_unopened_tracked(&mut self, num: ChanNum) -> Result<()> {
        if self.tracked_status(num)? != DirectStatus::Opening {
            return error::BadUsage.fail();
        }
        self.remove_any(num)
    }

    pub(crate) fn send_eof(&mut self, num: ChanNum, s: &mut TrafSend) -> Result<()> {
        self.get_mut(num)?.send_eof(s)
    }

    pub(crate) fn send_allowed(&self, num: ChanNum) -> Option<usize> {
        self.get(num).map_or(Some(0), |c| c.send_allowed())
    }

    pub(crate) fn valid_send(&self, num: ChanNum, dt: ChanData) -> bool {
        self.get(num).is_ok_and(|c| c.valid_send(dt))
    }

    pub fn progress(&mut self, s: &mut TrafSend) -> DispatchEvent {
        for ch in self.ch.iter_mut().filter_map(|c| c.as_mut()) {
            ch.check_send_window_adjust(s);

            if ch.open_confirmed {
                ch.open_confirmed = false;
                match ch.ty {
                    ChanType::Session => {
                        return DispatchEvent::CliEvent(CliEventId::SessionOpened(
                            ch.num(),
                        ));
                    }
                    ChanType::Tcp => {}
                }
            }
        }
        DispatchEvent::None
    }

    /// Wake the channel with a ready input data packet.
    pub fn wake_read(&mut self, num: ChanNum, dt: ChanData, is_client: bool) {
        if let Ok(ch) = self.get_mut(num) {
            ch.wake_read(dt, is_client);
        } else {
            debug_assert!(false, "wake_read bad channel");
        }
    }

    /// Wake all ready output channels
    pub fn wake_write(&mut self, is_client: bool) {
        for ch in self.ch.iter_mut().filter_map(|c| c.as_mut()) {
            ch.wake_write(is_client)
        }
    }

    pub(crate) fn term_window_change(
        &self,
        num: ChanNum,
        winch: &packets::WinChange,
        s: &mut TrafSend,
    ) -> Result<()> {
        let ch = self.get(num)?;
        match ch.ty {
            ChanType::Session => Req::WinChange(winch.clone()).send(ch, s),
            _ => error::BadChannelData.fail(),
        }
    }

    pub(crate) fn term_break(
        &self,
        num: ChanNum,
        length: u32,
        s: &mut TrafSend,
    ) -> Result<()> {
        let ch = self.get(num)?;
        let br = packets::Break {
            length: if length == 0 { 0 } else { length.clamp(500, 3000) },
        };
        match ch.ty {
            ChanType::Session => Req::Break(br).send(ch, s),
            _ => error::BadChannelData.fail(),
        }
    }

    fn dispatch_open(
        &mut self,
        p: &ChannelOpen<'_>,
        s: &mut TrafSend,
    ) -> Result<DispatchEvent> {
        match self.dispatch_open_inner(p) {
            Err(DispatchOpenError::Failure(f)) => {
                s.send(packets::ChannelOpenFailure {
                    num: p.sender_num,
                    reason: f as u32,
                    desc: "".into(),
                    lang: "",
                })?;
                Ok(DispatchEvent::None)
            }
            Err(DispatchOpenError::Error(e)) => Err(e),
            Ok(ev) => Ok(ev),
        }
    }

    // the caller will send failure messages if required
    fn dispatch_open_inner(
        &mut self,
        p: &ChannelOpen,
    ) -> Result<DispatchEvent, DispatchOpenError> {
        // Check validity before reserving a channel
        match &p.ty {
            ChannelOpenType::Unknown(u) => {
                error!("Rejecting unknown channel type '{u}'");
                return Err(ChanFail::SSH_OPEN_UNKNOWN_CHANNEL_TYPE.into());
            }
            ChannelOpenType::Session if self.is_client => {
                trace!("dispatch not server");
                return Err(error::SSHProto.build().into());
            }
            ChannelOpenType::ForwardedTcpip(_) if !self.is_client => {
                return Err(ChanFail::SSH_OPEN_UNKNOWN_CHANNEL_TYPE.into());
            }
            ChannelOpenType::DirectTcpip(_) => {
                // TODO implement it
                debug!("Rejecting direct tcp");
                return Err(ChanFail::SSH_OPEN_UNKNOWN_CHANNEL_TYPE.into());
            }
            _ => (),
        }

        // Reserve a channel
        let ch = self.reserve_chan(p)?;

        // Beware that a reserved channel must be cleaned up on failure

        match &p.ty {
            ChannelOpenType::Session => {
                Ok(DispatchEvent::ServEvent(ServEventId::OpenSession {
                    num: ch.num(),
                }))
            }
            ChannelOpenType::ForwardedTcpip(_) => {
                Ok(DispatchEvent::CliEvent(CliEventId::ForwardedTcpip(ch.num())))
            }
            _ => {
                // Checked above
                unreachable!()
            }
        }
    }

    pub fn resume_open(
        &mut self,
        c: ChanNum,
        failure: Option<ChanFail>,
        s: &mut TrafSend,
    ) -> Result<()> {
        let ch = self.get_any_mut(c)?;
        if let Some(failure) = failure {
            let sender_num = ch.send_num()?;
            self.remove_any(c)?;
            s.send(packets::ChannelOpenFailure {
                num: sender_num,
                reason: failure as u32,
                desc: "".into(),
                lang: "",
            })?;
            Ok(())
        } else {
            // Success
            s.send(ch.open_done()?)
        }
    }

    // Some returned errors will be caught by caller and returned as SSH messages
    fn dispatch_inner(
        &mut self,
        packet: Packet,
        s: &mut TrafSend,
    ) -> Result<DispatchEvent> {
        let mut ev = DispatchEvent::default();
        let is_client = self.is_client;

        match packet {
            Packet::ChannelOpen(p) => {
                ev = self.dispatch_open(&p, s)?;
            }

            Packet::ChannelOpenConfirmation(p) => {
                let ch = self.get_any_mut(ChanNum(p.num))?;
                match ch.state {
                    ChanState::Opening => {
                        debug_assert!(ch.send.is_none());

                        if ch.app_done || ch.open_failure.is_some() {
                            return Ok(DispatchEvent::None);
                        }

                        ch.send = Some(ChanDir {
                            num: p.sender_num,
                            max_packet: p.max_packet as usize,
                            window: p.initial_window as usize,
                        });

                        // A future progress() will notify the application.
                        ch.open_confirmed = !ch.tracked_session;
                        ch.state = ChanState::Normal;
                    }
                    _ => {
                        trace!("Bad channel state {:?}", ch.state);
                        return error::SSHProto.fail();
                    }
                }
            }

            Packet::ChannelOpenFailure(p) => {
                let ch = self.get_any_mut(ChanNum(p.num))?;
                if !matches!(ch.state, ChanState::Opening) || ch.open_failure.is_some() {
                    // TODO: or just warn?
                    trace!("open failure late?");
                    return error::SSHProto.fail();
                } else if matches!(ch.ty, ChanType::Tcp) || ch.tracked_session {
                    ch.open_failure = Some(p.reason);
                } else {
                    self.remove(ChanNum(p.num))?;
                    // TODO event
                }
            }
            Packet::ChannelWindowAdjust(p) => {
                let chan = self.get_mut(ChanNum(p.num))?;
                let send = chan.send.as_mut().trap()?;
                send.window = send.window.saturating_add(p.adjust as usize);
                debug!("ch {} new window +{} = {}", p.num, p.adjust, send.window);
                // Wake any writers that might have been blocked.
                chan.wake_write(is_client);
            }
            Packet::ChannelData(p) => {
                let ch = self.get_mut(ChanNum(p.num))?;
                if ch.app_done {
                    trace!("Ignoring data for done channel");
                } else if let Some(len) = NonZeroUsize::new(p.data.0.len()) {
                    ch.receive_input(len.get())?;
                    let di =
                        DataIn { num: ChanNum(p.num), dt: ChanData::Normal, len };
                    ev = DispatchEvent::Data(di);
                } else {
                    trace!("Zero length channeldata");
                }
            }
            Packet::ChannelDataExt(p) => {
                let ch = self.get_mut(ChanNum(p.num))?;
                if ch.app_done {
                    trace!("Ignoring data for done channel");
                } else if let Some(len) = NonZeroUsize::new(p.data.0.len()) {
                    ch.receive_input(len.get())?;
                    if !is_client || p.code != sshnames::SSH_EXTENDED_DATA_STDERR {
                        debug!("Ignoring unexpected dt data, code {}", p.code);
                        ch.finished_input(len.get())?;
                    } else {
                        let di = DataIn { num: ChanNum(p.num), dt: ChanData::Stderr, len };
                        ev = DispatchEvent::Data(di);
                    }
                } else {
                    trace!("Zero length channeldataext");
                }
            }
            Packet::ChannelEof(p) => {
                let ch = self.get_mut(ChanNum(p.num))?;
                ch.handle_eof(is_client);
            }
            Packet::ChannelClose(p) => {
                let is_client = self.is_client;
                let ch = self.get_mut(ChanNum(p.num))?;
                ch.handle_close(s, is_client)?;
            }
            Packet::ChannelRequest(p) => {
                let is_client = self.is_client;
                match self.get_mut(ChanNum(p.num)) {
                    Ok(ch) => {
                        ev = ch.dispatch_request(&p, s, is_client)?;
                    }
                    Err(_) => debug!("Ignoring request to unknown channel: {p:#?}"),
                }
            }
            Packet::ChannelSuccess(p) => {
                self.get_mut(ChanNum(p.num))?.complete_request(true);
            }
            Packet::ChannelFailure(p) => {
                self.get_mut(ChanNum(p.num))?.complete_request(false);
            }
            _ => Error::bug_msg("unreachable")?,
        };

        Ok(ev)
    }

    /// Incoming packet handling
    // TODO: protocol errors etc should perhaps be less fatal,
    // ssh implementations are usually imperfect.
    pub fn dispatch(
        &mut self,
        packet: Packet,
        s: &mut TrafSend,
    ) -> Result<DispatchEvent> {
        let r = self.dispatch_inner(packet, s);

        match r {
            Err(Error::BadChannel { num, .. }) => {
                warn!("Ignoring bad channel number {:?}", num);
                // warn!("Ignoring bad channel number {:?}", r.unwrap_err().backtrace());
                Ok(DispatchEvent::default())
            }
            // TODO: close channel on error? or on SSHProtoError?
            r => r,
        }
    }

    pub fn resume_chanreq(
        &self,
        p: &Packet,
        success: bool,
        s: &mut TrafSend,
    ) -> Result<()> {
        if let Packet::ChannelRequest(r) = p {
            let ch = self.get(ChanNum(r.num))?;
            if r.want_reply {
                let num = ch.send_num()?;
                if success {
                    s.send(packets::ChannelSuccess { num })
                } else {
                    s.send(packets::ChannelFailure { num })
                }
            } else {
                Ok(())
            }
        } else {
            Error::bug()
        }
    }

    pub fn fetch_servcommand<'p>(&self, p: &Packet<'p>) -> Result<TextString<'p>> {
        match p {
            Packet::ChannelRequest(ChannelRequest {
                req: ChannelReqType::Exec(packets::Exec { command }),
                ..
            })
            | Packet::ChannelRequest(ChannelRequest {
                req:
                    ChannelReqType::Subsystem(packets::Subsystem { subsystem: command }),
                ..
            }) => Ok(*command),
            _ => Error::bug(),
        }
    }

    pub fn fetch_env_name<'p>(&self, p: &Packet<'p>) -> Result<TextString<'p>> {
        match p {
            Packet::ChannelRequest(ChannelRequest {
                req: ChannelReqType::Environment(packets::Environment { name, .. }),
                ..
            }) => Ok(*name),
            _ => Error::bug(),
        }
    }

    pub fn fetch_env_value<'p>(&self, p: &Packet<'p>) -> Result<TextString<'p>> {
        match p {
            Packet::ChannelRequest(ChannelRequest {
                req:
                    ChannelReqType::Environment(packets::Environment { name: _, value }),
                ..
            }) => Ok(*value),
            _ => Error::bug(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ChanType {
    Session,
    Tcp,
}

impl From<&ChannelOpenType<'_>> for ChanType {
    fn from(c: &ChannelOpenType) -> Self {
        match c {
            ChannelOpenType::Session => ChanType::Session,
            ChannelOpenType::DirectTcpip(_) => ChanType::Tcp,
            ChannelOpenType::ForwardedTcpip(_) => ChanType::Tcp,
            ChannelOpenType::Unknown(_) => unreachable!(),
        }
    }
}

#[derive(Debug)]
pub struct ModePair {
    pub opcode: u8,
    pub arg: u32,
}

#[derive(Debug)]
pub struct Pty {
    pub term: String<MAX_TERM>,
    pub cols: u32,
    pub rows: u32,
    pub width: u32,
    pub height: u32,
    pub modes: Vec<ModePair, { termmodes::NUM_MODES }>,
}

impl TryFrom<&packets::PtyReq<'_>> for Pty {
    type Error = Error;
    fn try_from(p: &packets::PtyReq) -> Result<Self, Self::Error> {
        debug!("TODO implement pty modes");
        // SSHProto error is returned when TERM is too long. The caller will catch that.
        let term =
            p.term.to_ascii()?.try_into().map_err(|_| error::SSHProto.build())?;
        Ok(Pty {
            term,
            cols: p.cols,
            rows: p.rows,
            width: p.width,
            height: p.height,
            modes: Vec::new(),
        })
    }
}
/// Like a `packets::ChannelReqType` but with storage.
/// Lifetime-free variants have the packet part directly.
#[derive(Debug)]
pub enum Req<'a> {
    // TODO let hook impls provide a string type?
    Shell,
    Exec(&'a str),
    Subsystem(&'a str),
    Pty(Pty),
    WinChange(packets::WinChange),
    Break(packets::Break),
    // Signal,
    // ExitStatus,
    // ExitSignal,
}

impl Req<'_> {
    pub(crate) fn send(self, ch: &Channel, s: &mut TrafSend) -> Result<()> {
        self.send_with_reply(ch, s, false)
    }

    fn send_with_reply(self, ch: &Channel, s: &mut TrafSend, want_reply: bool) -> Result<()> {
        let t;
        let mut modes = [0; 5 * termmodes::NUM_MODES + 1];
        let req = match self {
            Req::Shell => ChannelReqType::Shell,
            Req::Pty(pty) => {
                t = pty.term;
                let mut length = 0;
                for mode in pty.modes {
                    modes[length] = mode.opcode;
                    modes[length + 1..length + 5].copy_from_slice(&mode.arg.to_be_bytes());
                    length += 5;
                }
                // Nonempty mode lists end with TTY_OP_END.
                if length != 0 {
                    length += 1;
                }
                ChannelReqType::Pty(packets::PtyReq {
                    term: TextString(t.as_bytes()),
                    cols: pty.cols,
                    rows: pty.rows,
                    width: pty.width,
                    height: pty.height,
                    modes: BinString(&modes[..length]),
                })
            }
            Req::Exec(cmd) => {
                ChannelReqType::Exec(packets::Exec { command: cmd.into() })
            }
            Req::Subsystem(cmd) => ChannelReqType::Subsystem(packets::Subsystem {
                subsystem: cmd.into(),
            }),
            Req::WinChange(rt) => ChannelReqType::WinChange(rt),
            Req::Break(rt) => ChannelReqType::Break(rt),
        };

        let p = ChannelRequest {
            num: ch.send_num()?,
            want_reply,
            req,
        };
        let p: Packet = p.into();
        s.send(p)
    }
}

/// Convenience for the types of session channels that can be opened
pub enum SessionCommand<S: AsRef<str>> {
    Shell,
    Exec(S),
    Subsystem(S),
}

impl<'a, S: AsRef<str> + 'a> From<&'a SessionCommand<S>> for Req<'a> {
    fn from(val: &'a SessionCommand<S>) -> Self {
        match val {
            SessionCommand::Shell => Req::Shell,
            SessionCommand::Exec(s) => Req::Exec(s.as_ref()),
            SessionCommand::Subsystem(s) => Req::Subsystem(s.as_ref()),
        }
    }
}

/// Per-direction channel variables
#[derive(Debug)]
struct ChanDir {
    /// `u32` rather than `ChanNum` because it can also be used
    /// for the sender-side number
    num: u32,
    max_packet: usize,
    window: usize,
}

#[derive(Debug)]
enum ChanState {
    /// An incoming channel open request that has not yet been responded to.
    ///
    /// Not to be used for normal channel messages
    InOpen,

    // TODO: perhaps .get() and .get_mut() should ignore Opening state channels?
    Opening,
    Normal,
    RecvEof,
    // TODO: recvclose state probably shouldn't be possible, we remove it straight away?
    RecvClose,
    /// The channel is unused and ready to close after a call to `done()`
    PendingDone,
}

#[derive(Debug)]
pub(crate) struct Channel {
    ty: ChanType,
    state: ChanState,
    sent_eof: bool,
    sent_close: bool,

    recv: ChanDir,
    /// populated in all states except `Opening`
    send: Option<ChanDir>,

    /// Accumulated bytes for the next window adjustment (inbound data direction)
    pending_adjust: usize,

    full_window: usize,

    /// Set when Open Confirmation is received.
    ///
    /// A subsequent `progress()` will emit a `SessionOpened` event
    /// for the application to handle.
    open_confirmed: bool,
    tracked_session: bool,
    open_failure: Option<u32>,
    request_reply: RequestReply,

    /// Set once application has called `done()`. The channel
    /// will only be removed from the list
    /// (allowing channel number re-use) if `app_done` is set
    app_done: bool,

    // Wakers for notifying readyness. Usually used for async.
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
    /// Will be a stderr read waker for a client, or stderr write waker for
    /// a server.
    ext_waker: Option<Waker>,
}

#[derive(Debug, PartialEq)]
enum RequestReply {
    Unsent,
    Pending { pty: bool },
    Complete { accepted: bool, pty: bool },
}

/// Result of a tracked outbound channel open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectStatus {
    Opening,
    Open,
    Refused(u32),
}

impl Channel {
    fn new(num: ChanNum, ty: ChanType) -> Self {
        Channel {
            ty,
            state: ChanState::Opening,
            sent_close: false,
            sent_eof: false,
            recv: ChanDir {
                num: num.0,
                // TODO these should depend on SSH rx buffer size minus overhead
                max_packet: config::DEFAULT_MAX_PACKET,
                window: config::DEFAULT_WINDOW,
            },
            send: None,
            pending_adjust: 0,
            full_window: config::DEFAULT_WINDOW,
            open_confirmed: false,
            tracked_session: false,
            open_failure: None,
            request_reply: RequestReply::Unsent,
            app_done: false,
            read_waker: None,
            write_waker: None,
            ext_waker: None,
        }
    }

    /// Local channel number
    pub(crate) fn num(&self) -> ChanNum {
        ChanNum(self.recv.num)
    }

    /// Remote channel number, fails if channel is in progress opening
    ///
    /// Returned as a plain `u32` since it is a different namespace than `ChanNum`.
    /// This is the channel number included in most sent packets.
    pub(crate) fn send_num(&self) -> Result<u32> {
        Ok(self.send.as_ref().trap()?.num)
    }

    pub fn set_read_waker(&mut self, dt: ChanData, is_client: bool, waker: &Waker) {
        match dt {
            ChanData::Normal => {
                set_waker(&mut self.read_waker, waker);
            }
            ChanData::Stderr => {
                if is_client {
                    set_waker(&mut self.ext_waker, waker);
                } else {
                    debug_assert!(false, "server ext read waker");
                }
            }
        }
    }

    pub fn set_write_waker(&mut self, dt: ChanData, is_client: bool, waker: &Waker) {
        match dt {
            ChanData::Normal => {
                set_waker(&mut self.write_waker, waker);
            }
            ChanData::Stderr => {
                if !is_client {
                    set_waker(&mut self.ext_waker, waker);
                } else {
                    debug_assert!(false, "client ext write waker");
                }
            }
        }
    }

    pub fn wake_read(&mut self, dt: ChanData, is_client: bool) {
        match dt {
            ChanData::Normal => {
                if let Some(w) = self.read_waker.take() {
                    w.wake()
                }
            }
            ChanData::Stderr => {
                if is_client {
                    if let Some(w) = self.ext_waker.take() {
                        w.wake()
                    }
                }
            }
        }
    }

    pub fn wake_write(&mut self, is_client: bool) {
        if let Some(w) = self.write_waker.take() {
            w.wake()
        }
        if !is_client {
            if let Some(w) = self.ext_waker.take() {
                w.wake()
            }
        }
    }

    /// Returns an open confirmation reply packet to send.
    /// Must be called with state of `InOpen`.
    fn open_done<'p>(&mut self) -> Result<Packet<'p>> {
        debug_assert!(matches!(self.state, ChanState::InOpen));

        self.state = ChanState::Normal;
        let p = packets::ChannelOpenConfirmation {
            num: self.send_num()?,
            sender_num: self.recv.num,
            initial_window: self.recv.window as u32,
            max_packet: self.recv.max_packet as u32,
        }
        .into();
        Ok(p)
    }

    fn dispatch_request(
        &mut self,
        p: &packets::ChannelRequest,
        s: &mut TrafSend,
        is_client: bool,
    ) -> Result<DispatchEvent> {
        let r = match (is_client, self.app_done) {
            // Reject requests if the application has closed
            // the channel. ChannelEOF is arbitrary.
            (_, true) => Err(Error::ChannelEOF),
            (true, _) => self.dispatch_client_request(p, s),
            (false, _) => self.dispatch_server_request(p, s),
        };

        match r {
            Ok(_) | Err(Error::Bug) => r,
            Err(_) => {
                // Most errors just send an error response, no failure.
                if p.want_reply {
                    let num = self.send_num();
                    debug_assert!(num.is_ok());
                    if let Ok(num) = num {
                        let _ = s.send(packets::ChannelFailure { num });
                    }
                }
                Ok(DispatchEvent::None)
            }
        }
    }

    fn dispatch_server_request(
        &self,
        p: &packets::ChannelRequest,
        _s: &mut TrafSend,
    ) -> Result<DispatchEvent> {
        if !matches!(self.ty, ChanType::Session) {
            return Err(Error::SSHProtoUnsupported);
        }

        let num = self.num();
        match &p.req {
            ChannelReqType::Shell => {
                Ok(DispatchEvent::ServEvent(ServEventId::SessionShell { num }))
            }
            ChannelReqType::Exec(_) => {
                Ok(DispatchEvent::ServEvent(ServEventId::SessionExec { num }))
            }
            ChannelReqType::Subsystem(_) => {
                Ok(DispatchEvent::ServEvent(ServEventId::SessionSubsystem { num }))
            }
            ChannelReqType::Pty(_) => {
                Ok(DispatchEvent::ServEvent(ServEventId::SessionPty { num }))
            }
            ChannelReqType::Environment(_) => {
                Ok(DispatchEvent::ServEvent(ServEventId::Environment { num }))
            }
            _ => {
                if let ChannelReqType::Unknown(u) = &p.req {
                    warn!("Unknown channel req type \"{}\"", u)
                } else {
                    // OK unwrap: tested for Unknown
                    warn!(
                        "Unhandled channel req \"{}\"",
                        p.req.variant_name().unwrap()
                    )
                };
                Err(Error::SSHProtoUnsupported)
            }
        }
    }

    /// Returns Ok(want_reply: bool) on success
    fn dispatch_client_request(
        &mut self,
        p: &packets::ChannelRequest,
        _s: &mut TrafSend,
    ) -> Result<DispatchEvent> {
        if !matches!(self.ty, ChanType::Session) {
            return Err(Error::SSHProtoUnsupported);
        }

        match &p.req {
            ChannelReqType::ExitStatus(_) => {
                Ok(DispatchEvent::CliEvent(CliEventId::SessionExit))
            }
            ChannelReqType::ExitSignal(_sig) => {
                Ok(DispatchEvent::CliEvent(CliEventId::SessionExit))
            }
            _ => {
                if let ChannelReqType::Unknown(u) = &p.req {
                    warn!("Unknown channel req type \"{}\"", u)
                } else {
                    // OK unwrap: tested for Unknown
                    warn!(
                        "Unhandled channel req \"{}\"",
                        p.req.variant_name().unwrap()
                    )
                };
                Err(Error::SSHProtoUnsupported)
            }
        }
    }

    fn send_eof(&mut self, s: &mut TrafSend) -> Result<()> {
        if self.is_closed() {
            return Err(Error::ChannelEOF);
        }
        if !self.sent_eof {
            s.send(packets::ChannelEof { num: self.send_num()? })?;
            self.sent_eof = true;
        }
        Ok(())
    }

    fn handle_eof(&mut self, is_client: bool) {
        if self.is_closed() {
            return;
        }

        // Wake readers on EOF
        self.wake_read(ChanData::Normal, is_client);
        if is_client {
            self.wake_read(ChanData::Stderr, is_client);
        }

        self.state = ChanState::RecvEof;
    }

    fn complete_request(&mut self, accepted: bool) {
        if let RequestReply::Pending { pty } = self.request_reply {
            self.request_reply = RequestReply::Complete { accepted, pty };
        }
    }

    fn handle_close(&mut self, s: &mut TrafSend, is_client: bool) -> Result<()> {
        self.send_close(s)?;

        // Wake readers and writers on EOF
        self.wake_read(ChanData::Normal, is_client);
        if is_client {
            self.wake_read(ChanData::Stderr, is_client);
        }
        self.wake_write(is_client);

        self.state = ChanState::RecvClose;
        Ok(())
    }

    fn send_close(&mut self, s: &mut TrafSend) -> Result<()> {
        if !self.sent_close {
            s.send(packets::ChannelClose { num: self.send_num()? })?;
            self.sent_close = true;
        }
        Ok(())
    }

    fn receive_input(&mut self, len: usize) -> Result<()> {
        if self.have_recv_eof() || len > self.recv.max_packet || len > self.recv.window {
            return error::SSHProto.fail();
        }
        self.recv.window -= len;
        Ok(())
    }

    fn finished_input(&mut self, len: usize) -> Result<()> {
        if len > self.full_window - self.recv.window - self.pending_adjust {
            return error::BadUsage.fail();
        }
        self.pending_adjust += len;
        Ok(())
    }

    fn have_recv_eof(&self) -> bool {
        matches!(self.state, ChanState::RecvEof | ChanState::RecvClose)
    }

    fn is_closed(&self) -> bool {
        matches!(self.state, ChanState::RecvClose)
    }

    // None on close
    fn send_allowed(&self) -> Option<usize> {
        if self.sent_eof || self.sent_close || self.is_closed() {
            return None;
        }
        let r = self.send.as_ref().map(|s| usize::min(s.window, s.max_packet));
        trace!("send_allowed {r:?}");
        r
    }

    pub(crate) fn valid_send(&self, _dt: ChanData) -> bool {
        // TODO: later we should only allow non-pty "session" channels
        // to have dt, for stderr only.
        true
    }

    /// Send a window adjust packet if required.
    fn check_send_window_adjust(&mut self, s: &mut TrafSend) {
        if !self.have_recv_eof() && self.pending_adjust > self.full_window / 2 {
            let adjust = self.pending_adjust as u32;
            let Some(sdir) = self.send.as_mut() else {
                return;
            };
            let num = sdir.num;
            let p = packets::ChannelWindowAdjust { num, adjust };
            match s.send(p) {
                Ok(()) => {
                    self.recv.window += self.pending_adjust;
                    self.pending_adjust = 0;
                }
                Err(Error::BusySend { .. }) => {
                    // Do nothing, the adjustment will be sent later.
                }
                Err(e) => debug_assert!(false, "Window adjust send failed {e:?}"),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DataIn {
    pub num: ChanNum,
    pub dt: ChanData,
    // Zero length data does nothing.
    pub len: NonZeroUsize,
}

/// The result of a channel open request.
pub enum ChanOpened {
    Success,
    /// A channel open response will be sent later (for eg TCP open)
    Defer,
    /// A SSH failure code, as well as returning the passed channel handle
    Failure((ChanFail, ChanHandle)),
}

/// A SSH protocol local channel number
///
/// The number will always be in the range `0 <= num < MAX_CHANNELS`
/// and can be used as an index by applications.
/// Most external application API methods take a `ChanHandle` instead.
#[derive(Debug, PartialEq, Clone, Copy, Eq, Hash, Ord, PartialOrd)]
pub struct ChanNum(pub u32);

impl core::fmt::Display for ChanNum {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

/// Channel data type, normal or stderr
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum ChanData {
    /// `SSH_MSG_CHANNEL_DATA`
    Normal,
    /// `SSH_MSG_CHANNEL_EXTENDED_DATA`. Only `Stderr` is implemented by Sunset,
    /// other types are not widely used.
    Stderr,
    // Future API:
    // Other(u32),
}

impl ChanData {
    pub(crate) fn validate_send(&self, is_client: bool) -> Result<()> {
        if matches!(self, ChanData::Stderr) && is_client {
            error::BadChannelData.fail()
        } else {
            Ok(())
        }
    }

    pub(crate) fn validate_receive(&self, is_client: bool) -> Result<()> {
        if matches!(self, ChanData::Stderr) && !is_client {
            error::BadChannelData.fail()
        } else {
            Ok(())
        }
    }

    pub(crate) fn packet_offset(&self) -> usize {
        match self {
            ChanData::Normal => ChannelData::DATA_OFFSET,
            ChanData::Stderr => ChannelDataExt::DATA_OFFSET,
        }
    }
}

// for dispatch_open_inner()
enum DispatchOpenError {
    /// A program error
    Error(Error),
    /// A SSH failure response
    Failure(ChanFail),
}

impl From<Error> for DispatchOpenError {
    fn from(e: Error) -> Self {
        match e {
            Error::NoChannels => Self::Failure(ChanFail::SSH_OPEN_RESOURCE_SHORTAGE),
            e => Self::Error(e),
        }
    }
}

impl From<ChanFail> for DispatchOpenError {
    fn from(f: ChanFail) -> Self {
        Self::Failure(f)
    }
}

// constructed from runner::cli_session_opener()
/// Sends shell, command, or other requests to a newly opened session channel
pub struct CliSessionOpener<'g, 'a> {
    pub(crate) ch: &'g mut Channel,
    pub(crate) s: TrafSend<'g, 'a>,
}

impl<'g, 'a> CliSessionOpener<'g, 'a> {
    /// Returns the channel number associated with this session.
    pub fn channel(&self) -> ChanNum {
        self.ch.num()
    }

    /// Requests a Pseudo-TTY for the channel.
    ///
    /// This must be sent prior to requesting a shell or command.
    /// Shells using a PTY will only receive data on the stdin FD, not stderr.
    // TODO: set a flag in the channel so that it drops data on stderr, to
    // avoid waiting forever for a consumer?
    pub fn pty(&mut self, pty: channel::Pty) -> Result<()> {
        self.send(Req::Pty(pty))
    }

    /// Requests a PTY and tracks whether the server granted it.
    pub fn pty_confirmed(&mut self, pty: channel::Pty) -> Result<()> {
        self.send_confirmed(Req::Pty(pty))
    }

    /// Requests a particular command or shell for a channel
    pub fn cmd<S: AsRef<str>>(&mut self, cmd: &SessionCommand<S>) -> Result<()> {
        self.send(cmd.into())
    }

    /// Requests the channel operation and tracks its single confirmation.
    ///
    /// Poll `Runner::client_request_result` while driving connection progress.
    pub fn cmd_confirmed<S: AsRef<str>>(&mut self, cmd: &SessionCommand<S>) -> Result<()> {
        self.send_confirmed(Req::from(cmd))
    }

    fn send_confirmed(&mut self, request: Req<'_>) -> Result<()> {
        if self.ch.request_reply != RequestReply::Unsent {
            return error::BadUsage.fail();
        }
        let pty = matches!(request, Req::Pty(_));
        request.send_with_reply(self.ch, &mut self.s, true)?;
        self.ch.request_reply = RequestReply::Pending { pty };
        Ok(())
    }

    pub fn shell(&mut self) -> Result<()> {
        self.send(Req::Shell)
    }

    pub fn exec(&mut self, cmd: impl AsRef<str>) -> Result<()> {
        self.send(Req::Exec(cmd.as_ref()))
    }

    pub fn subsystem(&mut self, cmd: impl AsRef<str>) -> Result<()> {
        self.send(Req::Subsystem(cmd.as_ref()))
    }

    fn send(&mut self, req: Req) -> Result<()> {
        req.send(self.ch, &mut self.s)
    }
}

impl core::fmt::Debug for CliSessionOpener<'_, '_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CliSessionOpener").finish()
    }
}

#[derive(Debug)]
pub enum CliSessionExit<'g> {
    /// Remote process exited with an exit status code
    Status { channel: ChanNum, status: u32 },
    /// Remote process exited by signal
    Signal { channel: ChanNum, signal: ExitSignal<'g> },
}

impl<'g> CliSessionExit<'g> {
    pub fn new(p: &Packet<'g>) -> Result<Self> {
        match p {
            Packet::ChannelRequest(ChannelRequest {
                num,
                req: ChannelReqType::ExitStatus(e),
                ..
            }) => Ok(Self::Status { channel: ChanNum(*num), status: e.status }),
            Packet::ChannelRequest(ChannelRequest {
                num,
                req: ChannelReqType::ExitSignal(e),
                ..
            }) => Ok(Self::Signal { channel: ChanNum(*num), signal: e.clone() }),
            _ => Error::bug(),
        }
    }
}
