//! TCP (RFC 793, simplified): reliable byte streams over IPv4.
//!
//! - Connections are a state machine: SynSent / SynReceived -> Established
//!   -> (FinWait1 -> FinWait2 | Closing) -> TimeWait, or CloseWait -> LastAck,
//!   -> Closed. RST closes at once.
//! - Sending: bytes wait in `send_buf` until acknowledged. At most
//!   min(peer window, 128 segments) are in flight. If the oldest unacknowledged
//!   segment is not acknowledged within the retransmission timeout (RTO),
//!   everything from there is sent again (go-back-N) and the RTO doubles;
//!   the RTO follows the measured round trip time (one sample at a time,
//!   none from retransmitted data - Karn's rule).
//! - Receiving: only in-order data is accepted (out-of-order segments are
//!   dropped and answered with a duplicate ACK; the sender retransmits).
//!   The advertised window is the free space in `recv_buf`.
//! - Window scaling (RFC 7323): our SYN offers a shift of `WSCALE_SHIFT`;
//!   if the peer's SYN (or SYN-ACK) carries the option too, windows in all
//!   later segments are scaled - ours by our shift, the peer's by its shift.
//!   Windows in SYN segments themselves are never scaled. So the receive
//!   buffer can be larger than 64 KiB.
//! - Not implemented: SACK, Nagle, delayed ACKs, urgent data, the
//!   zero-window persist timer, simultaneous open.
//!
//! All state lives in one table behind one lock, taken with interrupts off
//! (it is read by `wait_for` conditions). Segments are collected while the
//! lock is held and sent afterwards, because sending may block for ARP.

use super::packet::*;
use super::{NetError, config, send_ipv4, wait_net};
use crate::arch::x86_64::without_interrupts;
use crate::time;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

pub type ConnId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpError {
    Refused,
    Reset,
    TimedOut,
    AddressInUse,
    /// Writing after the connection was shut down or closed.
    NotConnected,
    Interrupted,
    Net(NetError),
}

const DEFAULT_MSS: usize = 536;
const RECV_CAP: usize = 256 * 1024;
const SEND_CAP: usize = 256 * 1024;
const MAX_IN_FLIGHT_SEGMENTS: usize = 128;
/// Our window scale: advertised windows count in units of 2^3 bytes.
const WSCALE_SHIFT: u8 = 3;
const _: () = assert!(RECV_CAP >> WSCALE_SHIFT <= u16::MAX as usize);
/// Retransmissions of the same data before the connection gives up (`set_max_retries`).
static MAX_RETRIES: AtomicU32 = AtomicU32::new(8);
/// Retransmission timeouts so far (all connections).
static RETRANSMISSIONS: AtomicU64 = AtomicU64::new(0);
const RTO_INITIAL_US: u64 = 1_000_000;
const RTO_MIN_US: u64 = 200_000;
const RTO_MAX_US: u64 = 8_000_000;
const TIME_WAIT_US: u64 = 2_000_000;

struct Conn {
    state: State,
    local_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
    iss: u32,
    // sending
    snd_una: u32,
    snd_nxt: u32,
    snd_wnd: u32,
    peer_mss: usize,
    /// Window scaling: our SYN offers it (active open) or the peer's SYN did.
    offer_wscale: bool,
    /// Shift for the peer's advertised windows / for ours (0 = no scaling).
    snd_shift: u8,
    rcv_shift: u8,
    /// Sequence number of `send_buf[0]`.
    buf_seq: u32,
    send_buf: VecDeque<u8>,
    fin_queued: bool,
    /// Sequence number of our FIN once it was sent.
    fin_seq: Option<u32>,
    // receiving
    rcv_nxt: u32,
    recv_buf: VecDeque<u8>,
    peer_fin: bool,
    // timers
    rto_us: u64,
    last_tx_us: u64,
    retries: u32,
    /// (sequence number that acknowledges the timed segment, send time)
    rtt_probe: Option<(u32, u64)>,
    srtt_us: Option<u64>,
    time_wait_since: u64,
    error: Option<TcpError>,
    /// Port of the listener that created this connection (until accepted).
    listener: Option<u16>,
    /// No descriptor refers to it (any more): remove once closed.
    orphan: bool,
}

struct Listener {
    backlog: VecDeque<ConnId>,
}

struct Tcp {
    conns: BTreeMap<ConnId, Conn>,
    listeners: BTreeMap<u16, Listener>,
    next_id: ConnId,
    next_port: u16,
}

static TCP: Mutex<Tcp> = Mutex::new(Tcp {
    conns: BTreeMap::new(),
    listeners: BTreeMap::new(),
    next_id: 1,
    next_port: 0,
});

fn locked<R>(f: impl FnOnce(&mut Tcp) -> R) -> R {
    without_interrupts(|| f(&mut TCP.lock()))
}

/// A segment to send once the lock is released.
struct Out {
    dst: Ipv4Addr,
    segment: Vec<u8>,
}

fn transmit(out: Vec<Out>) {
    for o in out {
        let _ = send_ipv4(o.dst, PROTO_TCP, &o.segment);
    }
}

fn new_iss() -> u32 {
    (crate::arch::x86_64::tsc::rdtsc() >> 4) as u32
}

fn now() -> u64 {
    time::uptime_us()
}

impl Conn {
    fn new(state: State, local_port: u16, remote_ip: Ipv4Addr, remote_port: u16) -> Conn {
        let iss = new_iss();
        Conn {
            state,
            local_port,
            remote_ip,
            remote_port,
            iss,
            snd_una: iss,
            snd_nxt: iss,
            snd_wnd: 0,
            peer_mss: DEFAULT_MSS,
            offer_wscale: true,
            snd_shift: 0,
            rcv_shift: 0,
            buf_seq: iss.wrapping_add(1),
            send_buf: VecDeque::new(),
            fin_queued: false,
            fin_seq: None,
            rcv_nxt: 0,
            recv_buf: VecDeque::new(),
            peer_fin: false,
            rto_us: RTO_INITIAL_US,
            last_tx_us: now(),
            retries: 0,
            rtt_probe: None,
            srtt_us: None,
            time_wait_since: 0,
            error: None,
            listener: None,
            orphan: false,
        }
    }

    /// The window to advertise: free space, scaled (never in SYN segments).
    fn window(&self, flags: u8) -> u16 {
        let free = RECV_CAP - self.recv_buf.len();
        let scaled = if flags & TCP_SYN != 0 {
            free
        } else {
            free >> self.rcv_shift
        };
        scaled.min(u16::MAX as usize) as u16
    }

    /// The peer's SYN (or SYN-ACK) told us whether it scales windows.
    fn agree_on_scaling(&mut self, peer: Option<u8>) {
        match peer {
            Some(shift) if self.offer_wscale => {
                self.snd_shift = shift;
                self.rcv_shift = WSCALE_SHIFT;
            }
            _ => {
                self.offer_wscale = false;
                self.snd_shift = 0;
                self.rcv_shift = 0;
            }
        }
    }

    fn segment(&self, flags: u8, seq: u32, payload: &[u8], out: &mut Vec<Out>) {
        let Some(cfg) = config() else { return };
        let ack = if flags & TCP_ACK != 0 { self.rcv_nxt } else { 0 };
        let wscale = self.offer_wscale.then_some(WSCALE_SHIFT);
        out.push(Out {
            dst: self.remote_ip,
            segment: build_tcp(
                cfg.ip,
                self.remote_ip,
                self.local_port,
                self.remote_port,
                seq,
                ack,
                flags,
                self.window(flags),
                wscale,
                payload,
            ),
        });
    }

    fn ack_now(&self, out: &mut Vec<Out>) {
        self.segment(TCP_ACK, self.snd_nxt, &[], out);
    }

    fn in_flight(&self) -> bool {
        self.snd_una != self.snd_nxt
    }

    /// Sends whatever the state and the windows allow.
    fn output(&mut self, out: &mut Vec<Out>) {
        let t = now();
        match self.state {
            State::SynSent if self.snd_nxt == self.iss => {
                self.segment(TCP_SYN, self.iss, &[], out);
                self.snd_nxt = self.iss.wrapping_add(1);
                self.last_tx_us = t;
            }
            State::SynReceived if self.snd_nxt == self.iss => {
                self.segment(TCP_SYN | TCP_ACK, self.iss, &[], out);
                self.snd_nxt = self.iss.wrapping_add(1);
                self.last_tx_us = t;
            }
            State::Established | State::CloseWait | State::FinWait1 | State::Closing | State::LastAck => {
                let mut sent = self.snd_nxt.wrapping_sub(self.buf_seq) as usize;
                let limit = (self.snd_wnd as usize).min(MAX_IN_FLIGHT_SEGMENTS * self.peer_mss);
                while self.fin_seq.is_none() && sent < self.send_buf.len() {
                    let flight = self.snd_nxt.wrapping_sub(self.snd_una) as usize;
                    if flight >= limit {
                        break;
                    }
                    let n = (limit - flight).min(self.peer_mss).min(self.send_buf.len() - sent);
                    let payload: Vec<u8> = self.send_buf.range(sent..sent + n).copied().collect();
                    if !self.in_flight() {
                        self.last_tx_us = t; // the retransmission timer starts with the first byte
                    }
                    if self.rtt_probe.is_none() {
                        self.rtt_probe = Some((self.snd_nxt.wrapping_add(n as u32), t));
                    }
                    self.segment(TCP_ACK | TCP_PSH, self.snd_nxt, &payload, out);
                    self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
                    sent += n;
                }
                if self.fin_queued && self.fin_seq.is_none() && sent == self.send_buf.len() {
                    if !self.in_flight() {
                        self.last_tx_us = t;
                    }
                    self.segment(TCP_FIN | TCP_ACK, self.snd_nxt, &[], out);
                    self.fin_seq = Some(self.snd_nxt);
                    self.snd_nxt = self.snd_nxt.wrapping_add(1);
                    self.state = match self.state {
                        State::Established => State::FinWait1,
                        State::CloseWait => State::LastAck,
                        other => other, // a retransmitted FIN
                    };
                }
            }
            _ => {}
        }
    }

    /// Retransmission timer (called every 10 ms by the net thread).
    fn on_timer(&mut self, t: u64, out: &mut Vec<Out>) {
        if self.state == State::TimeWait && t - self.time_wait_since >= TIME_WAIT_US {
            self.state = State::Closed;
            return;
        }
        if !self.in_flight() || t - self.last_tx_us < self.rto_us {
            return;
        }
        self.retries += 1;
        RETRANSMISSIONS.fetch_add(1, Ordering::Relaxed);
        if self.retries > MAX_RETRIES.load(Ordering::Relaxed) {
            self.segment(TCP_RST, self.snd_nxt, &[], out);
            self.error = Some(TcpError::TimedOut);
            self.state = State::Closed;
            return;
        }
        self.rto_us = (self.rto_us * 2).min(RTO_MAX_US);
        self.rtt_probe = None; // Karn: no RTT samples from retransmissions
        // go back to the oldest unacknowledged byte and send everything again
        self.snd_nxt = self.snd_una;
        if matches!(self.state, State::SynSent | State::SynReceived) {
            self.snd_nxt = self.iss;
        } else if self.fin_seq.is_some_and(|f| seq_le(self.snd_una, f)) {
            self.fin_seq = None;
        }
        self.output(out);
        self.last_tx_us = t;
    }

    /// Processes an acknowledgement number from the peer.
    fn on_ack(&mut self, ack: u32, window: u16) {
        if seq_lt(self.snd_una, ack) && seq_le(ack, self.snd_nxt) {
            let data_end = self.buf_seq.wrapping_add(self.send_buf.len() as u32);
            let acked_end = if seq_lt(data_end, ack) { data_end } else { ack };
            let acked = acked_end.wrapping_sub(self.buf_seq) as usize;
            if seq_lt(self.buf_seq, acked_end) {
                self.send_buf.drain(..acked.min(self.send_buf.len()));
                self.buf_seq = acked_end;
            }
            self.snd_una = ack;
            self.retries = 0;
            let t = now();
            self.last_tx_us = t;
            if let Some((end, sent)) = self.rtt_probe {
                if seq_le(end, ack) {
                    let sample = t - sent;
                    let srtt = self.srtt_us.map_or(sample, |s| (7 * s + sample) / 8);
                    self.srtt_us = Some(srtt);
                    self.rto_us = (2 * srtt).clamp(RTO_MIN_US, RTO_MAX_US);
                    self.rtt_probe = None;
                }
            }
            if self.fin_seq.is_some_and(|f| seq_lt(f, ack)) {
                self.state = match self.state {
                    State::FinWait1 => State::FinWait2,
                    State::Closing => {
                        self.time_wait_since = t;
                        State::TimeWait
                    }
                    State::LastAck => State::Closed,
                    other => other,
                };
            }
        }
        if seq_le(self.snd_una, ack) {
            self.snd_wnd = (window as u32) << self.snd_shift;
        }
    }

    /// Processes data and FIN of an incoming segment (synchronised states).
    fn on_data(&mut self, seg: &TcpSegment, out: &mut Vec<Out>) {
        let mut need_ack = false;
        let mut accepted_all = true;
        if !seg.payload.is_empty() {
            need_ack = true;
            if seg.seq == self.rcv_nxt && matches!(self.state, State::Established | State::FinWait1 | State::FinWait2) {
                let n = (RECV_CAP - self.recv_buf.len()).min(seg.payload.len());
                self.recv_buf.extend(&seg.payload[..n]);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
                accepted_all = n == seg.payload.len();
            } else {
                accepted_all = false;
            }
        }
        if seg.flags & TCP_FIN != 0 {
            need_ack = true;
            let fin_seq = seg.seq.wrapping_add(seg.payload.len() as u32);
            if accepted_all && fin_seq == self.rcv_nxt && !self.peer_fin {
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                self.peer_fin = true;
                self.state = match self.state {
                    State::Established => State::CloseWait,
                    State::FinWait1 => State::Closing,
                    State::FinWait2 => {
                        self.time_wait_since = now();
                        State::TimeWait
                    }
                    other => other,
                };
            }
        }
        if need_ack {
            self.ack_now(out);
        }
    }
}

/// Handles one incoming TCP segment (called by the net thread).
pub fn input(src: Ipv4Addr, seg: &TcpSegment) {
    let mut out = Vec::new();
    locked(|tcp| {
        let found = tcp
            .conns
            .iter()
            .find(|(_, c)| {
                c.state != State::Closed
                    && c.local_port == seg.dst_port
                    && c.remote_ip == src
                    && c.remote_port == seg.src_port
            })
            .map(|(&id, _)| id);
        let Some(id) = found else {
            no_connection(tcp, src, seg, &mut out);
            return;
        };
        let c = tcp.conns.get_mut(&id).unwrap();
        if seg.flags & TCP_RST != 0 {
            c.error = Some(if c.state == State::SynSent {
                TcpError::Refused
            } else {
                TcpError::Reset
            });
            c.state = State::Closed;
            return;
        }
        match c.state {
            State::SynSent => {
                if seg.flags & (TCP_SYN | TCP_ACK) == TCP_SYN | TCP_ACK && seg.ack == c.snd_nxt {
                    c.rcv_nxt = seg.seq.wrapping_add(1);
                    c.peer_mss = seg.mss.map_or(DEFAULT_MSS, |m| m as usize).min(1460);
                    c.on_ack(seg.ack, seg.window); // the SYN-ACK's window is not scaled
                    c.agree_on_scaling(seg.wscale);
                    c.state = State::Established;
                    c.ack_now(&mut out);
                    c.output(&mut out);
                }
            }
            State::SynReceived => {
                if seg.flags & TCP_SYN != 0 {
                    c.snd_nxt = c.iss; // the peer did not get our SYN-ACK: send it again
                    c.output(&mut out);
                } else if seg.flags & TCP_ACK != 0 && seg.ack == c.snd_nxt {
                    c.on_ack(seg.ack, seg.window);
                    c.state = State::Established;
                    c.on_data(seg, &mut out);
                    if let Some(listener) = c.listener.and_then(|p| tcp.listeners.get_mut(&p)) {
                        listener.backlog.push_back(id);
                    }
                }
            }
            _ => {
                if seg.flags & TCP_ACK != 0 {
                    c.on_ack(seg.ack, seg.window);
                }
                c.on_data(seg, &mut out);
                c.output(&mut out);
            }
        }
    });
    transmit(out);
    crate::task::notify(crate::task::Wait::Net);
}

/// A segment for no known connection: a SYN for a listener opens one,
/// everything else is answered with RST.
fn no_connection(tcp: &mut Tcp, src: Ipv4Addr, seg: &TcpSegment, out: &mut Vec<Out>) {
    if seg.flags & TCP_RST != 0 {
        return;
    }
    if seg.flags & (TCP_SYN | TCP_ACK) == TCP_SYN && tcp.listeners.contains_key(&seg.dst_port) {
        let mut c = Conn::new(State::SynReceived, seg.dst_port, src, seg.src_port);
        c.rcv_nxt = seg.seq.wrapping_add(1);
        c.snd_wnd = seg.window as u32; // not scaled in a SYN
        c.peer_mss = seg.mss.map_or(DEFAULT_MSS, |m| m as usize).min(1460);
        c.agree_on_scaling(seg.wscale); // our SYN-ACK only offers scaling if the SYN did
        c.listener = Some(seg.dst_port);
        c.orphan = true; // until accepted
        c.output(out);
        let id = tcp.next_id;
        tcp.next_id += 1;
        tcp.conns.insert(id, c);
        return;
    }
    let Some(cfg) = config() else { return };
    let (seq, ack, flags) = if seg.flags & TCP_ACK != 0 {
        (seg.ack, 0, TCP_RST)
    } else {
        let len = seg.payload.len() as u32 + (seg.flags & (TCP_SYN | TCP_FIN) != 0) as u32;
        (0, seg.seq.wrapping_add(len), TCP_RST | TCP_ACK)
    };
    out.push(Out {
        dst: src,
        segment: build_tcp(cfg.ip, src, seg.dst_port, seg.src_port, seq, ack, flags, 0, None, &[]),
    });
}

/// Retransmissions and clean-up; called every 10 ms by the net thread.
pub fn on_timer() {
    let mut out = Vec::new();
    locked(|tcp| {
        let t = now();
        for c in tcp.conns.values_mut() {
            c.on_timer(t, &mut out);
        }
        tcp.conns.retain(|_, c| !(c.state == State::Closed && c.orphan));
    });
    transmit(out);
}

fn ephemeral_port(tcp: &mut Tcp) -> u16 {
    loop {
        tcp.next_port = tcp.next_port.wrapping_add(1);
        let port = 0xC000 | (tcp.next_port & 0x3FFF);
        if !tcp.conns.values().any(|c| c.local_port == port) && !tcp.listeners.contains_key(&port) {
            return port;
        }
    }
}

fn state_of(id: ConnId) -> Option<(State, Option<TcpError>)> {
    locked(|tcp| tcp.conns.get(&id).map(|c| (c.state, c.error)))
}

/// Opens a connection and waits until it is established.
pub fn connect(ip: Ipv4Addr, port: u16, timeout_ms: u64, abort: impl Fn() -> bool) -> Result<ConnId, TcpError> {
    config().ok_or(TcpError::Net(NetError::NoDevice))?;
    let mut out = Vec::new();
    let id = locked(|tcp| {
        let local = ephemeral_port(tcp);
        let mut c = Conn::new(State::SynSent, local, ip, port);
        c.output(&mut out);
        let id = tcp.next_id;
        tcp.next_id += 1;
        tcp.conns.insert(id, c);
        id
    });
    transmit(out);
    wait_net(timeout_ms, || {
        state_of(id).is_none_or(|(s, _)| s != State::SynSent) || abort()
    });
    match state_of(id) {
        Some((State::Established, _)) | Some((State::CloseWait, _)) => Ok(id),
        Some((_, err)) => {
            let err = err.unwrap_or(if abort() {
                TcpError::Interrupted
            } else {
                TcpError::TimedOut
            });
            abort_conn(id);
            Err(err)
        }
        None => Err(TcpError::Reset),
    }
}

/// Starts accepting connections on `port`.
pub fn listen(port: u16) -> Result<(), TcpError> {
    locked(|tcp| {
        if tcp.listeners.contains_key(&port) {
            return Err(TcpError::AddressInUse);
        }
        tcp.listeners.insert(
            port,
            Listener {
                backlog: VecDeque::new(),
            },
        );
        Ok(())
    })
}

/// Stops listening; connections nobody accepted yet are reset.
pub fn unlisten(port: u16) {
    let mut out = Vec::new();
    locked(|tcp| {
        tcp.listeners.remove(&port);
        for c in tcp.conns.values_mut() {
            if c.listener == Some(port) && c.orphan && c.state != State::Closed {
                c.segment(TCP_RST, c.snd_nxt, &[], &mut out);
                c.state = State::Closed;
            }
        }
    });
    transmit(out);
}

/// Waits for an established connection on `port`. Returns it with the peer address.
pub fn accept(port: u16, abort: impl Fn() -> bool) -> Result<(ConnId, Ipv4Addr, u16), TcpError> {
    let take = || {
        locked(|tcp| {
            let id = tcp.listeners.get_mut(&port)?.backlog.pop_front()?;
            let c = tcp.conns.get_mut(&id)?;
            c.orphan = false;
            c.listener = None;
            Some((id, c.remote_ip, c.remote_port))
        })
    };
    let ready = || locked(|tcp| tcp.listeners.get(&port).is_none_or(|l| !l.backlog.is_empty()));
    loop {
        if let Some(conn) = take() {
            return Ok(conn);
        }
        if !locked(|tcp| tcp.listeners.contains_key(&port)) {
            return Err(TcpError::NotConnected);
        }
        if abort() {
            return Err(TcpError::Interrupted);
        }
        wait_net(1000, || ready() || abort());
    }
}

/// Queues `data` for sending; blocks while the send buffer is full.
/// Returns how much was taken (everything, unless interrupted).
pub fn send(id: ConnId, data: &[u8], abort: impl Fn() -> bool) -> Result<usize, TcpError> {
    let mut done = 0;
    while done < data.len() {
        let mut out = Vec::new();
        let step = locked(|tcp| {
            let c = tcp.conns.get_mut(&id).ok_or(TcpError::NotConnected)?;
            if let Some(e) = c.error {
                return Err(e);
            }
            if c.fin_queued || !matches!(c.state, State::Established | State::CloseWait) {
                return Err(TcpError::NotConnected);
            }
            let n = (SEND_CAP - c.send_buf.len()).min(data.len() - done);
            c.send_buf.extend(&data[done..done + n]);
            c.output(&mut out);
            Ok(n)
        });
        transmit(out);
        match step {
            Ok(0) => {
                if abort() {
                    return if done > 0 { Ok(done) } else { Err(TcpError::Interrupted) };
                }
                let room = || {
                    locked(|tcp| {
                        tcp.conns
                            .get(&id)
                            .is_none_or(|c| c.send_buf.len() < SEND_CAP || c.error.is_some())
                    })
                };
                wait_net(1000, || room() || abort());
            }
            Ok(n) => done += n,
            Err(e) => return if done > 0 { Ok(done) } else { Err(e) },
        }
    }
    Ok(done)
}

/// Receives up to `max` bytes; blocks until data arrives. An empty result
/// means the peer closed its side (end of stream).
pub fn recv(id: ConnId, max: usize, abort: impl Fn() -> bool) -> Result<Vec<u8>, TcpError> {
    loop {
        let mut out = Vec::new();
        let r = locked(|tcp| {
            let c = tcp.conns.get_mut(&id).ok_or(TcpError::NotConnected)?;
            if !c.recv_buf.is_empty() {
                let was_small = RECV_CAP - c.recv_buf.len() < 2 * c.peer_mss;
                let n = max.min(c.recv_buf.len());
                let data: Vec<u8> = c.recv_buf.drain(..n).collect();
                if was_small && c.state != State::Closed {
                    c.ack_now(&mut out); // window update
                }
                return Ok(Some(data));
            }
            if c.peer_fin {
                return Ok(Some(Vec::new()));
            }
            if let Some(e) = c.error {
                return Err(e);
            }
            if c.state == State::Closed {
                return Ok(Some(Vec::new()));
            }
            Ok(None)
        });
        transmit(out);
        match r? {
            Some(data) => return Ok(data),
            None if abort() => return Err(TcpError::Interrupted),
            None => {
                let ready = || {
                    locked(|tcp| {
                        tcp.conns.get(&id).is_none_or(|c| {
                            !c.recv_buf.is_empty() || c.peer_fin || c.error.is_some() || c.state == State::Closed
                        })
                    })
                };
                wait_net(1000, || ready() || abort());
            }
        }
    }
}

/// Sends FIN after the queued data (we will not write any more); reading goes on.
pub fn shutdown(id: ConnId) {
    let mut out = Vec::new();
    locked(|tcp| {
        if let Some(c) = tcp.conns.get_mut(&id) {
            if matches!(c.state, State::Established | State::CloseWait) {
                c.fin_queued = true;
                c.output(&mut out);
            }
        }
    });
    transmit(out);
}

/// The last descriptor is gone: close gracefully (FIN), free when done.
pub fn close(id: ConnId) {
    let mut out = Vec::new();
    locked(|tcp| {
        if let Some(c) = tcp.conns.get_mut(&id) {
            c.orphan = true;
            match c.state {
                State::Established | State::CloseWait => {
                    c.fin_queued = true;
                    c.output(&mut out);
                }
                State::SynSent | State::SynReceived => c.state = State::Closed,
                _ => {}
            }
        }
    });
    transmit(out);
}

/// Drops a connection at once (RST if it got anywhere).
fn abort_conn(id: ConnId) {
    let mut out = Vec::new();
    locked(|tcp| {
        if let Some(c) = tcp.conns.remove(&id) {
            if !matches!(c.state, State::Closed | State::SynSent) {
                c.segment(TCP_RST, c.snd_nxt, &[], &mut out);
            }
        }
    });
    transmit(out);
}

/// (state, local port, remote ip, remote port) of every connection, for `netstat`.
/// (peer's window in bytes, our receive window shift) of a connection.
pub fn windows(id: ConnId) -> Option<(u32, u8)> {
    locked(|tcp| tcp.conns.get(&id).map(|c| (c.snd_wnd, c.rcv_shift)))
}

pub fn connections() -> Vec<(State, u16, Ipv4Addr, u16)> {
    locked(|tcp| {
        tcp.conns
            .values()
            .map(|c| (c.state, c.local_port, c.remote_ip, c.remote_port))
            .collect()
    })
}

/// Ports with a listener.
/// How many retransmission timeouts have expired (all connections, since boot).
pub fn retransmissions() -> u64 {
    RETRANSMISSIONS.load(Ordering::Relaxed)
}

/// For tests: how often unacknowledged data is sent again before the
/// connection is reset with `TimedOut` (default 8: tens of seconds of silence, the timeout doubles).
pub fn set_max_retries(n: u32) {
    MAX_RETRIES.store(n, Ordering::Relaxed);
}

pub fn listening_ports() -> Vec<u16> {
    locked(|tcp| tcp.listeners.keys().copied().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task;

    const ME: Ipv4Addr = [10, 0, 2, 15];

    #[test_case]
    fn connect_to_a_closed_port_is_refused() {
        assert_eq!(connect(ME, 7999, 2000, || false), Err(TcpError::Refused));
    }

    #[test_case]
    fn windows_beyond_64k_with_scaling() {
        listen(7201).unwrap();
        let server = task::spawn("tcp-sink", || {
            let (id, _, _) = accept(7201, || false).unwrap();
            let mut total = 0usize;
            let mut sum = 0u64;
            loop {
                let data = recv(id, 65536, || false).unwrap();
                if data.is_empty() {
                    break;
                }
                total += data.len();
                sum += data.iter().map(|&b| b as u64).sum::<u64>();
            }
            send(id, alloc::format!("{} {}", total, sum).as_bytes(), || false).unwrap();
            close(id);
        });
        let id = connect(ME, 7201, 3000, || false).expect("connect over loopback");
        assert_eq!(windows(id).map(|w| w.1), Some(WSCALE_SHIFT), "scaling not agreed");
        let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(send(id, &data, || false), Ok(data.len()));
        shutdown(id);
        let mut reply = Vec::new();
        loop {
            let chunk = recv(id, 100, || false).unwrap();
            if chunk.is_empty() {
                break;
            }
            reply.extend_from_slice(&chunk);
        }
        // the window in the server's last segments counts in units of 2^3
        let (peer_window, _) = windows(id).unwrap();
        assert!(peer_window > 65535, "peer window {} not scaled", peer_window);
        task::join(server);
        close(id);
        unlisten(7201);
        let sum: u64 = data.iter().map(|&b| b as u64).sum();
        assert_eq!(core::str::from_utf8(&reply).unwrap(), alloc::format!("1000000 {}", sum));
    }

    #[test_case]
    fn loopback_transfer_both_ways() {
        listen(7200).unwrap();
        assert_eq!(listen(7200), Err(TcpError::AddressInUse));
        // server: echo everything back, then close
        let server = task::spawn("tcp-echo", || {
            let (id, peer, _) = accept(7200, || false).unwrap();
            assert_eq!(peer, ME);
            loop {
                let data = recv(id, 4096, || false).unwrap();
                if data.is_empty() {
                    break;
                }
                send(id, &data, || false).unwrap();
            }
            close(id);
        });
        let id = connect(ME, 7200, 3000, || false).expect("connect over loopback");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
        let sender = {
            let data = data.clone();
            task::spawn("tcp-send", move || {
                assert_eq!(send(id, &data, || false), Ok(data.len()));
                shutdown(id);
            })
        };
        let mut got = Vec::new();
        loop {
            let chunk = recv(id, 8192, || false).unwrap();
            if chunk.is_empty() {
                break;
            }
            got.extend_from_slice(&chunk);
        }
        task::join(sender);
        task::join(server);
        close(id);
        unlisten(7200);
        assert_eq!(got.len(), data.len());
        assert!(got == data, "echoed data differs");
    }
}
