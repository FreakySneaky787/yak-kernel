//! A small IPv4 network stack on top of the e1000 driver.
//!
//! The configuration comes from DHCP at boot (`dhcp`); without an answer
//! we fall back to QEMU's user-mode defaults ("slirp"): 10.0.2.15/24, the
//! virtual gateway 10.0.2.2 (it answers pings, and traffic to it reaches the
//! host's 127.0.0.1), DNS 10.0.2.3.
//!
//! - A kernel thread ("net") reads the card when its interrupt arrives (see
//!   `probe_irq`; at least every 10 ms anyway), handles ARP
//!   (answers requests for our address, learns replies), answers ICMP echo
//!   requests, records echo replies and queues UDP datagrams for sockets.
//! - Sending resolves the next hop (the destination on our subnet, else the
//!   gateway) through the ARP cache, asking with ARP requests if needed.
//! - Waiting (ARP, ping replies, UDP receive) uses `task::wait_for` with
//!   `Wait::Net`; the net thread notifies after every round, so timeouts are
//!   checked at least every 10 ms.
//! - Packets to our own address do not go to the card: they are queued and
//!   handled by the net thread like received ones (loopback).
//! - TCP lives in `tcp`; the net thread also drives its retransmission timer.
//!
//! The small state locks below are only taken with interrupts disabled
//! (they are read from `wait_for` conditions, which run with interrupts off).

pub mod dhcp;
pub mod packet;
pub mod tcp;

use crate::arch::x86_64::without_interrupts;
use crate::drivers::e1000::E1000;
use crate::task::{self, Wait};
use crate::time;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use packet::*;
use spin::Mutex;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub mac: Mac,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub dns: Ipv4Addr,
    /// From a DHCP server (false: the static fallback).
    pub dhcp: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// No network card.
    NoDevice,
    /// The next hop did not answer ARP.
    Unreachable,
    TimedOut,
    AddressInUse,
    TooLong,
    Interrupted,
}

/// A received UDP datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    pub src: Ipv4Addr,
    pub src_port: u16,
    pub data: Vec<u8>,
}

/// A bound UDP port; unbound when the last reference is dropped.
pub struct UdpSocket {
    pub port: u16,
    queue: Arc<Mutex<VecDeque<Datagram>>>,
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        without_interrupts(|| SOCKETS.lock().remove(&self.port));
    }
}

const PING_ID: u16 = 0x4B54; // "KT"
const SOCKET_QUEUE_MAX: usize = 64;
const ARP_TIMEOUT_MS: u64 = 1000;
pub const MAX_UDP_PAYLOAD: usize = 1472; // 1500 - IPv4 - UDP header

static NIC: Mutex<Option<E1000>> = Mutex::new(None);
/// Changes once, when DHCP is done; read with interrupts off (small lock).
static CONFIG: Mutex<Option<Config>> = Mutex::new(None);
static ARP_CACHE: Mutex<BTreeMap<Ipv4Addr, Mac>> = Mutex::new(BTreeMap::new());
/// (id, sequence) of received echo replies -> uptime in µs when they arrived
static PING_REPLIES: Mutex<BTreeMap<(u16, u16), u64>> = Mutex::new(BTreeMap::new());
static SOCKETS: Mutex<BTreeMap<u16, Arc<Mutex<VecDeque<Datagram>>>>> = Mutex::new(BTreeMap::new());
static NEXT_IP_ID: AtomicU16 = AtomicU16::new(1);
static NEXT_EPHEMERAL: AtomicU16 = AtomicU16::new(49152);
/// IPv4 packets sent to ourselves, waiting for the net thread.
static LOOPBACK: Mutex<VecDeque<Vec<u8>>> = Mutex::new(VecDeque::new());
/// The net thread, the card's I/O APIC input (0 = polling) and interrupt bookkeeping.
static NET_THREAD: AtomicU64 = AtomicU64::new(0);
static IRQ_GSI: AtomicU32 = AtomicU32::new(0);
static IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
static RX_PENDING: AtomicBool = AtomicBool::new(false);
/// Interrupt vector of the card.
pub const VECTOR: u8 = 0x51;

fn in_net_thread() -> bool {
    task::current_id() == NET_THREAD.load(Ordering::Relaxed)
}

/// Called from the interrupt handler of `VECTOR`.
pub fn handle_interrupt() {
    if crate::drivers::e1000::take_interrupt_causes() != 0 {
        IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
        RX_PENDING.store(true, Ordering::SeqCst);
        let net = NET_THREAD.load(Ordering::Relaxed);
        if net != 0 {
            task::wake(net);
        }
    }
}

/// (I/O APIC input of the card if interrupts work, interrupts so far).
pub fn interrupt_stats() -> (Option<u32>, u64) {
    let gsi = IRQ_GSI.load(Ordering::Relaxed);
    ((gsi != 0).then_some(gsi), IRQ_COUNT.load(Ordering::Relaxed))
}

/// Finds the I/O APIC input the card's interrupt pin is wired to (see the
/// e1000 module docs): tries every free PCI input (GSI 16-23) in turn.
fn probe_irq(nic: &E1000) -> Option<u32> {
    use crate::arch::x86_64::{ioapic, lapic};
    let cpu = lapic::id();
    nic.enable_interrupts(Some(crate::drivers::e1000::ICR_LSC));
    let found = (16..ioapic::input_count().min(24)).find(|&gsi| {
        if !ioapic::is_masked(gsi) {
            return false; // somebody else's
        }
        let before = IRQ_COUNT.load(Ordering::Relaxed);
        ioapic::route(gsi, VECTOR, cpu, true, true);
        nic.trigger_interrupt(crate::drivers::e1000::ICR_LSC);
        let deadline = time::uptime_ms() + 5;
        while IRQ_COUNT.load(Ordering::Relaxed) == before && time::uptime_ms() < deadline {
            core::hint::spin_loop();
        }
        let hit = IRQ_COUNT.load(Ordering::Relaxed) != before;
        if !hit {
            ioapic::mask(gsi);
            crate::drivers::e1000::take_interrupt_causes(); // release the line
        }
        hit
    });
    nic.disable_interrupts();
    found
}

pub fn config() -> Option<Config> {
    without_interrupts(|| *CONFIG.lock())
}

/// (received, sent) frames and whether the link is up.
pub fn stats() -> Option<(u64, u64, bool)> {
    NIC.lock().as_ref().map(|n| (n.rx_frames, n.tx_frames, n.link_up()))
}

/// Finds the card, configures the stack and starts the net thread.
pub fn init() {
    let nic = match E1000::probe() {
        Ok(nic) => nic,
        Err(e) => {
            crate::serial_println!("net: no network card ({:?})", e);
            return;
        }
    };
    let mac = nic.mac();
    // unconfigured until DHCP is done (address 0.0.0.0)
    set_config(Config {
        mac,
        ip: [0; 4],
        netmask: [0; 4],
        gateway: [0; 4],
        dns: [0; 4],
        dhcp: false,
    });
    match probe_irq(&nic) {
        Some(gsi) => {
            nic.enable_interrupts(None);
            IRQ_GSI.store(gsi, Ordering::Relaxed);
        }
        None => crate::serial_println!("net: no interrupt from the card, polling"),
    }
    *NIC.lock() = Some(nic);
    NET_THREAD.store(task::spawn("net", poll_loop), Ordering::Relaxed);
    match dhcp::request_lease(mac, 3000) {
        Ok(lease) => {
            set_config(Config {
                mac,
                ip: lease.your_ip,
                netmask: lease.netmask,
                gateway: lease.router,
                dns: lease.dns,
                dhcp: true,
            });
            let start = time::uptime_ms();
            task::spawn("dhcp", move || {
                dhcp::maintain(mac, lease.your_ip, lease.server_id, lease.lease_secs, start)
            });
        }
        Err(e) => {
            crate::serial_println!("net: no DHCP answer ({:?}), using the static QEMU configuration", e);
            use_static_config(mac);
        }
    }
}

/// QEMU's user-mode network defaults (no DHCP answer, or the lease ran out).
pub fn use_static_config(mac: Mac) {
    set_config(Config {
        mac,
        ip: [10, 0, 2, 15],
        netmask: [255, 255, 255, 0],
        gateway: [10, 0, 2, 2],
        dns: [10, 0, 2, 3],
        dhcp: false,
    });
}

fn set_config(cfg: Config) {
    without_interrupts(|| *CONFIG.lock() = Some(cfg));
}

fn poll_loop() {
    loop {
        loop {
            let frame = NIC.lock().as_mut().and_then(|n| n.receive());
            match frame {
                Some(f) => handle_frame(&f),
                None => break,
            }
        }
        // only what is queued now: answers to these wait for the next round
        let looped: Vec<Vec<u8>> = without_interrupts(|| LOOPBACK.lock().drain(..).collect());
        if let Some(cfg) = config() {
            for packet in looped {
                handle_ipv4(&cfg, cfg.mac, &packet);
            }
        }
        tcp::on_timer();
        task::notify(Wait::Net); // also lets waiters check their timeouts
        // until the card interrupts (or a packet was looped back), at most 10 ms
        if !RX_PENDING.swap(false, Ordering::SeqCst) {
            time::sleep_ms_or_abort(10, || RX_PENDING.load(Ordering::SeqCst));
        }
    }
}

fn send_frame(frame: &[u8]) -> Result<(), NetError> {
    let mut nic = NIC.lock();
    let nic = nic.as_mut().ok_or(NetError::NoDevice)?;
    nic.send(frame).map_err(|_| NetError::TooLong)
}

fn on_subnet(cfg: &Config, ip: Ipv4Addr) -> bool {
    (0..4).all(|i| ip[i] & cfg.netmask[i] == cfg.ip[i] & cfg.netmask[i])
}

fn learn(ip: Ipv4Addr, mac: Mac) {
    without_interrupts(|| ARP_CACHE.lock().insert(ip, mac));
}

fn cached_mac(ip: Ipv4Addr) -> Option<Mac> {
    without_interrupts(|| ARP_CACHE.lock().get(&ip).copied())
}

/// Handles one received Ethernet frame (called by the net thread).
pub fn handle_frame(frame: &[u8]) {
    let Some(cfg) = config() else { return };
    let Some(eth) = parse_eth(frame) else { return };
    if eth.dst != cfg.mac && eth.dst != BROADCAST_MAC {
        return;
    }
    match eth.ethertype {
        ETHERTYPE_ARP => {
            let Some(arp) = parse_arp(eth.payload) else { return };
            if arp.target_ip != cfg.ip {
                return;
            }
            learn(arp.sender_ip, arp.sender_mac);
            if arp.op == ARP_REQUEST {
                let reply = Arp {
                    op: ARP_REPLY,
                    sender_mac: cfg.mac,
                    sender_ip: cfg.ip,
                    target_mac: arp.sender_mac,
                    target_ip: arp.sender_ip,
                };
                let _ = send_frame(&build_eth(arp.sender_mac, cfg.mac, ETHERTYPE_ARP, &build_arp(&reply)));
            }
            task::notify(Wait::Net);
        }
        ETHERTYPE_IPV4 => handle_ipv4(&cfg, eth.src, eth.payload),
        _ => {}
    }
}

fn handle_ipv4(cfg: &Config, from_mac: Mac, packet: &[u8]) {
    let Some(ip) = parse_ipv4(packet) else { return };
    // without an address yet (DHCP running) everything for our MAC is ours
    if ip.dst != cfg.ip && ip.dst != [255; 4] && cfg.ip != [0; 4] {
        return;
    }
    if on_subnet(cfg, ip.src) && ip.src != cfg.ip {
        learn(ip.src, from_mac);
    }
    match ip.protocol {
        PROTO_ICMP => handle_icmp(cfg, from_mac, &ip),
        PROTO_UDP => handle_udp(&ip),
        PROTO_TCP => {
            if let Some(seg) = parse_tcp(ip.src, ip.dst, ip.payload) {
                tcp::input(ip.src, &seg);
            }
        }
        _ => {}
    }
}

fn handle_icmp(cfg: &Config, from_mac: Mac, ip: &Ipv4Packet) {
    let Some(echo) = parse_icmp_echo(ip.payload) else {
        return;
    };
    if echo.kind == ICMP_ECHO_REQUEST {
        let reply = build_icmp_echo(ICMP_ECHO_REPLY, echo.id, echo.seq, echo.data);
        let id = NEXT_IP_ID.fetch_add(1, Ordering::Relaxed);
        let packet = build_ipv4(cfg.ip, ip.src, PROTO_ICMP, id, &reply);
        let _ = send_frame(&build_eth(from_mac, cfg.mac, ETHERTYPE_IPV4, &packet));
    } else if echo.id == PING_ID {
        let now = time::uptime_us();
        without_interrupts(|| PING_REPLIES.lock().insert((echo.id, echo.seq), now));
        task::notify(Wait::Net);
    }
}

fn handle_udp(ip: &Ipv4Packet) {
    let Some(udp) = parse_udp(ip.src, ip.dst, ip.payload) else { return };
    let queue = without_interrupts(|| SOCKETS.lock().get(&udp.dst_port).cloned());
    if let Some(queue) = queue {
        without_interrupts(|| {
            let mut q = queue.lock();
            if q.len() < SOCKET_QUEUE_MAX {
                q.push_back(Datagram {
                    src: ip.src,
                    src_port: udp.src_port,
                    data: udp.payload.to_vec(),
                });
            }
        });
        task::notify(Wait::Net);
    }
}

/// Waits (as `Wait::Net`) until `done()` or `timeout_ms` passed. Returns `done()`.
fn wait_net(timeout_ms: u64, done: impl Fn() -> bool) -> bool {
    let deadline = time::uptime_ms() + timeout_ms;
    task::wait_for(Wait::Net, || done() || time::uptime_ms() >= deadline);
    done()
}

/// The MAC address of `ip` (a host on our subnet), asking with ARP if needed.
pub fn resolve(ip: Ipv4Addr) -> Result<Mac, NetError> {
    let cfg = config().ok_or(NetError::NoDevice)?;
    if ip == [255; 4] {
        return Ok(BROADCAST_MAC);
    }
    for _ in 0..3 {
        if let Some(mac) = cached_mac(ip) {
            return Ok(mac);
        }
        let request = Arp {
            op: ARP_REQUEST,
            sender_mac: cfg.mac,
            sender_ip: cfg.ip,
            target_mac: [0; 6],
            target_ip: ip,
        };
        send_frame(&build_eth(BROADCAST_MAC, cfg.mac, ETHERTYPE_ARP, &build_arp(&request)))?;
        if wait_net(ARP_TIMEOUT_MS / 3, || cached_mac(ip).is_some()) {
            return Ok(cached_mac(ip).unwrap());
        }
    }
    Err(NetError::Unreachable)
}

/// Sends an IPv4 packet to `dst` (through the gateway if it is not local).
pub fn send_ipv4(dst: Ipv4Addr, protocol: u8, payload: &[u8]) -> Result<(), NetError> {
    let cfg = config().ok_or(NetError::NoDevice)?;
    if dst == cfg.ip {
        let id = NEXT_IP_ID.fetch_add(1, Ordering::Relaxed);
        let packet = build_ipv4(cfg.ip, cfg.ip, protocol, id, payload);
        without_interrupts(|| LOOPBACK.lock().push_back(packet));
        // the net thread handles it right away, not at its next 10 ms round
        if !in_net_thread() {
            RX_PENDING.store(true, Ordering::SeqCst);
            task::wake(NET_THREAD.load(Ordering::Relaxed));
        }
        return Ok(());
    }
    let hop = if on_subnet(&cfg, dst) || dst == [255; 4] {
        dst
    } else {
        cfg.gateway
    };
    let mac = resolve(hop)?;
    let id = NEXT_IP_ID.fetch_add(1, Ordering::Relaxed);
    let packet = build_ipv4(cfg.ip, dst, protocol, id, payload);
    send_frame(&build_eth(mac, cfg.mac, ETHERTYPE_IPV4, &packet))
}

/// Sends an ICMP echo request and waits for the reply. Returns the round
/// trip time in microseconds (receive time = when the net thread polled it).
pub fn ping(dst: Ipv4Addr, seq: u16, timeout_ms: u64) -> Result<u64, NetError> {
    let start = time::uptime_us();
    let data: Vec<u8> = (0..32u8).collect();
    send_ipv4(
        dst,
        PROTO_ICMP,
        &build_icmp_echo(ICMP_ECHO_REQUEST, PING_ID, seq, &data),
    )?;
    let got = || without_interrupts(|| PING_REPLIES.lock().contains_key(&(PING_ID, seq)));
    if !wait_net(timeout_ms, got) {
        return Err(NetError::TimedOut);
    }
    let at = without_interrupts(|| PING_REPLIES.lock().remove(&(PING_ID, seq))).unwrap_or(start);
    Ok(at.saturating_sub(start))
}

/// Binds a UDP port (0 = pick a free one).
pub fn udp_bind(port: u16) -> Result<Arc<UdpSocket>, NetError> {
    without_interrupts(|| {
        let mut sockets = SOCKETS.lock();
        let port = if port == 0 {
            (0..16384)
                .map(|_| NEXT_EPHEMERAL.fetch_add(1, Ordering::Relaxed) | 0xC000)
                .find(|p| !sockets.contains_key(p))
                .ok_or(NetError::AddressInUse)?
        } else {
            port
        };
        if sockets.contains_key(&port) {
            return Err(NetError::AddressInUse);
        }
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        sockets.insert(port, queue.clone());
        Ok(Arc::new(UdpSocket { port, queue }))
    })
}

impl UdpSocket {
    pub fn send_to(&self, dst: Ipv4Addr, dst_port: u16, data: &[u8]) -> Result<(), NetError> {
        if data.len() > MAX_UDP_PAYLOAD {
            return Err(NetError::TooLong);
        }
        let cfg = config().ok_or(NetError::NoDevice)?;
        send_ipv4(dst, PROTO_UDP, &build_udp(cfg.ip, dst, self.port, dst_port, data))
    }

    /// The next datagram, waiting up to `timeout_ms`; `abort` stops the wait (kill).
    pub fn recv(&self, timeout_ms: u64, abort: impl Fn() -> bool) -> Result<Datagram, NetError> {
        let has_data = || without_interrupts(|| !self.queue.lock().is_empty());
        wait_net(timeout_ms, || has_data() || abort());
        if let Some(d) = without_interrupts(|| self.queue.lock().pop_front()) {
            return Ok(d);
        }
        Err(if abort() {
            NetError::Interrupted
        } else {
            NetError::TimedOut
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn card_is_configured() {
        let cfg = config().expect("no network card (QEMU runs with -device e1000)");
        assert_eq!(cfg.ip, [10, 0, 2, 15]);
        assert!(cfg.dhcp, "QEMU's DHCP server did not answer");
        assert_eq!(
            (cfg.gateway, cfg.dns, cfg.netmask),
            ([10, 0, 2, 2], [10, 0, 2, 3], [255, 255, 255, 0])
        );
        assert_eq!(cfg.mac[0] & 1, 0, "MAC is a multicast address");
        assert!(stats().unwrap().2, "link is down");
    }

    #[test_case]
    fn gateway_answers_arp_and_ping() {
        let mac = resolve([10, 0, 2, 2]).expect("gateway did not answer ARP");
        assert_ne!(mac, [0; 6]);
        let rtt = ping([10, 0, 2, 2], 1, 2000).expect("gateway did not answer the ping");
        assert!(rtt < 2_000_000);
    }

    #[test_case]
    fn received_frames_arrive_by_interrupt() {
        let (gsi, before) = interrupt_stats();
        let gsi = gsi.expect("the card's interrupt was not found");
        assert!((16..24).contains(&gsi), "PCI interrupts are GSI 16-23, got {}", gsi);
        // pings: answered within far less than the 10 ms polling period
        let mut rtts: Vec<u64> = (10..17).filter_map(|seq| ping([10, 0, 2, 2], seq, 2000).ok()).collect();
        assert_eq!(rtts.len(), 7, "pings lost");
        rtts.sort();
        assert!(
            rtts[3] < 3000,
            "median round trip {} µs: polled, not interrupt driven?",
            rtts[3]
        );
        assert!(interrupt_stats().1 > before, "no interrupts counted");
    }

    #[test_case]
    fn udp_ports_are_exclusive() {
        let a = udp_bind(7000).unwrap();
        assert_eq!(udp_bind(7000).err(), Some(NetError::AddressInUse));
        drop(a);
        assert!(udp_bind(7000).is_ok(), "port not released on drop");
        let e = udp_bind(0).unwrap();
        assert!(e.port >= 0xC000);
        assert_eq!(e.recv(30, || false).err(), Some(NetError::TimedOut));
    }

    #[test_case]
    fn udp_to_ourselves_through_the_stack() {
        // a datagram handed to the receive path lands in the socket queue
        let s = udp_bind(7001).unwrap();
        let cfg = config().unwrap();
        let udp = build_udp([10, 0, 2, 2], cfg.ip, 5555, 7001, b"loop");
        let ip = build_ipv4([10, 0, 2, 2], cfg.ip, PROTO_UDP, 1, &udp);
        handle_frame(&build_eth(cfg.mac, [0x52, 0x55, 10, 0, 2, 2], ETHERTYPE_IPV4, &ip));
        let d = s.recv(100, || false).unwrap();
        assert_eq!(
            (d.src, d.src_port, d.data.as_slice()),
            ([10, 0, 2, 2], 5555, &b"loop"[..])
        );
    }
}
