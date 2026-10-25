//! DHCP client (RFC 2131): asks the network for an address at boot and
//! keeps the lease alive.
//!
//! - Getting a lease: DISCOVER (broadcast) -> OFFER from a server -> REQUEST
//!   for that address -> ACK. Sent from 0.0.0.0:68 to 255.255.255.255:67
//!   with the broadcast flag, so the server answers with broadcasts too.
//! - Keeping it (thread "dhcp", `maintain`): at T1 (half the lease) a
//!   REQUEST with our address in `ciaddr` goes to the server that gave the
//!   lease (RENEWING); without an answer it is repeated after half the time
//!   left, from T2 (7/8) on as a broadcast to any server (REBINDING). An ACK
//!   starts the lease anew; when it runs out, the stack falls back to the
//!   static QEMU configuration. `LeaseTimes::phase` is the pure timing part.

use super::packet::*;
use super::{NetError, udp_bind};
use alloc::vec::Vec;

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;
/// Never wait less than this between two renewal attempts (RFC: 60 s).
const MIN_RETRY_MS: u64 = 60_000;

/// Runs the exchange; `timeout_ms` per step. Returns the ACKed lease.
pub fn request_lease(mac: Mac, timeout_ms: u64) -> Result<DhcpReply, NetError> {
    let socket = udp_bind(CLIENT_PORT)?;
    let xid = new_xid();
    let discover: Vec<u8> = build_dhcp(DHCP_DISCOVER, xid, mac, None, None);
    socket.send_to([255; 4], SERVER_PORT, &discover)?;
    let offer = wait_reply(&socket, xid, DHCP_OFFER, timeout_ms)?;
    let request = build_dhcp(DHCP_REQUEST, xid, mac, Some(offer.your_ip), Some(offer.server_id));
    socket.send_to([255; 4], SERVER_PORT, &request)?;
    let ack = wait_reply(&socket, xid, DHCP_ACK, timeout_ms)?;
    crate::serial_println!(
        "net: DHCP lease {}.{}.{}.{} from {}.{}.{}.{} ({} s)",
        ack.your_ip[0],
        ack.your_ip[1],
        ack.your_ip[2],
        ack.your_ip[3],
        ack.server_id[0],
        ack.server_id[1],
        ack.server_id[2],
        ack.server_id[3],
        ack.lease_secs
    );
    Ok(ack)
}

fn new_xid() -> u32 {
    (crate::arch::x86_64::tsc::rdtsc() >> 3) as u32
}

/// Waits for a reply of type `want` to transaction `xid` (NAK: `Unreachable`).
fn wait_reply(socket: &super::UdpSocket, xid: u32, want: u8, timeout_ms: u64) -> Result<DhcpReply, NetError> {
    let deadline = crate::time::uptime_ms() + timeout_ms;
    loop {
        let left = deadline.saturating_sub(crate::time::uptime_ms());
        if left == 0 {
            return Err(NetError::TimedOut);
        }
        let d = socket.recv(left, || false)?;
        if let Some(reply) = parse_dhcp(&d.data, xid) {
            if reply.msg_type == want {
                return Ok(reply);
            }
            if reply.msg_type == DHCP_NAK {
                return Err(NetError::Unreachable);
            }
        }
    }
}

/// Extends the lease of `ip`: a REQUEST in the RENEWING state (unicast to
/// `server`) or, with `server` = None, in the REBINDING state (broadcast).
pub fn renew(mac: Mac, ip: Ipv4Addr, server: Option<Ipv4Addr>, timeout_ms: u64) -> Result<DhcpReply, NetError> {
    let socket = udp_bind(CLIENT_PORT)?;
    let xid = new_xid();
    let request = build_dhcp_renew(xid, mac, ip);
    socket.send_to(server.unwrap_or([255; 4]), SERVER_PORT, &request)?;
    wait_reply(&socket, xid, DHCP_ACK, timeout_ms)
}

/// When a lease was obtained and how long it lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseTimes {
    pub start_ms: u64,
    pub lease_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before T1: nothing to do until `t1`.
    Bound,
    /// Between T1 and T2: ask the leasing server.
    Renewing,
    /// Between T2 and the end: ask any server.
    Rebinding,
    Expired,
}

impl LeaseTimes {
    pub fn t1_ms(&self) -> u64 {
        self.start_ms + self.lease_secs as u64 * 1000 / 2
    }

    pub fn t2_ms(&self) -> u64 {
        self.start_ms + self.lease_secs as u64 * 1000 * 7 / 8
    }

    pub fn end_ms(&self) -> u64 {
        self.start_ms + self.lease_secs as u64 * 1000
    }

    pub fn phase(&self, now_ms: u64) -> Phase {
        match now_ms {
            t if t < self.t1_ms() => Phase::Bound,
            t if t < self.t2_ms() => Phase::Renewing,
            t if t < self.end_ms() => Phase::Rebinding,
            _ => Phase::Expired,
        }
    }

    /// When to try (again) after an attempt at `now_ms` got no answer: half
    /// the time to the next deadline (T2 or the end), but not sooner than
    /// `MIN_RETRY_MS` - and never later than that deadline.
    pub fn retry_at(&self, now_ms: u64) -> u64 {
        let deadline = match self.phase(now_ms) {
            Phase::Bound => return self.t1_ms(),
            Phase::Renewing => self.t2_ms(),
            Phase::Rebinding | Phase::Expired => self.end_ms(),
        };
        let half = deadline.saturating_sub(now_ms) / 2;
        (now_ms + half.max(MIN_RETRY_MS)).min(deadline)
    }
}

/// The lease of the running system (for `lease_status`).
static LEASE: spin::Mutex<Option<(LeaseTimes, Ipv4Addr)>> = spin::Mutex::new(None);

/// (lease times, server) of the current lease, if DHCP configured us.
pub fn lease_status() -> Option<(LeaseTimes, Ipv4Addr)> {
    crate::arch::x86_64::without_interrupts(|| *LEASE.lock())
}

/// Thread body: keeps the lease of `ip` (obtained at `start_ms`) alive.
pub fn maintain(mac: Mac, ip: Ipv4Addr, server: Ipv4Addr, lease_secs: u32, start_ms: u64) {
    let mut times = LeaseTimes { start_ms, lease_secs };
    let mut server = server;
    let store = |t: LeaseTimes, s: Ipv4Addr| crate::arch::x86_64::without_interrupts(|| *LEASE.lock() = Some((t, s)));
    store(times, server);
    let mut next = times.t1_ms();
    loop {
        let now = crate::time::uptime_ms();
        if now < next {
            crate::time::sleep_ms((next - now).min(60_000));
            continue;
        }
        let target = match times.phase(now) {
            Phase::Bound => {
                next = times.t1_ms();
                continue;
            }
            Phase::Renewing => Some(server),
            Phase::Rebinding => None,
            Phase::Expired => {
                crate::serial_println!("net: DHCP lease expired, back to the static configuration");
                super::use_static_config(mac);
                crate::arch::x86_64::without_interrupts(|| *LEASE.lock() = None);
                return;
            }
        };
        match renew(mac, ip, target, 2000) {
            Ok(ack) if ack.your_ip == ip => {
                times = LeaseTimes {
                    start_ms: crate::time::uptime_ms(),
                    lease_secs: ack.lease_secs,
                };
                if ack.server_id != [0; 4] {
                    server = ack.server_id;
                }
                store(times, server);
                next = times.t1_ms();
            }
            _ => next = times.retry_at(crate::time::uptime_ms()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn lease_phases_and_retries() {
        let t = LeaseTimes {
            start_ms: 1000,
            lease_secs: 3600,
        };
        assert_eq!((t.t1_ms(), t.t2_ms(), t.end_ms()), (1_801_000, 3_151_000, 3_601_000));
        assert_eq!(t.phase(1000), Phase::Bound);
        assert_eq!(t.phase(1_801_000), Phase::Renewing);
        assert_eq!(t.phase(3_151_000), Phase::Rebinding);
        assert_eq!(t.phase(3_601_000), Phase::Expired);
        assert_eq!(t.retry_at(5000), t.t1_ms(), "bound: wait for T1");
        // renewing at T1: half of the 1350 s to T2
        assert_eq!(t.retry_at(1_801_000), 1_801_000 + 675_000);
        // close to T2: at least 60 s, but not past T2
        assert_eq!(t.retry_at(3_100_000), 3_151_000);
        // rebinding: half the way to the end, at least 60 s
        assert_eq!(t.retry_at(3_151_000), 3_151_000 + 225_000);
        assert_eq!(t.retry_at(3_590_000), 3_601_000);
    }

    #[test_case]
    fn the_lease_is_renewed_by_the_server() {
        let cfg = super::super::config().expect("no network");
        let (times, server) = lease_status().expect("no DHCP lease");
        assert!(times.lease_secs > 0 && server != [0; 4]);
        // RENEWING (unicast to the server) and REBINDING (broadcast) both get an ACK
        for target in [Some(server), None] {
            let ack = renew(cfg.mac, cfg.ip, target, 2000).expect("no ACK for the renewal");
            assert_eq!(ack.your_ip, cfg.ip);
            assert!(ack.lease_secs > 0);
        }
    }
}
