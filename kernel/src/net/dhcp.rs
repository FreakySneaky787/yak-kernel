//! DHCP client (RFC 2131): asks the network for an address at boot.
//!
//! DISCOVER (broadcast) -> OFFER from a server -> REQUEST for that address
//! -> ACK. Sent from 0.0.0.0:68 to 255.255.255.255:67 with the broadcast
//! flag, so the server answers with broadcasts too. The lease is not
//! renewed (QEMU's leases last a day, far longer than a test run).

use super::packet::*;
use super::{NetError, udp_bind};
use alloc::vec::Vec;

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;

/// Runs the exchange; `timeout_ms` per step. Returns the ACKed lease.
pub fn request_lease(mac: Mac, timeout_ms: u64) -> Result<DhcpReply, NetError> {
    let socket = udp_bind(CLIENT_PORT)?;
    let xid = (crate::arch::x86_64::tsc::rdtsc() >> 3) as u32;
    let wait_for = |want: u8| -> Result<DhcpReply, NetError> {
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
    };
    let discover: Vec<u8> = build_dhcp(DHCP_DISCOVER, xid, mac, None, None);
    socket.send_to([255; 4], SERVER_PORT, &discover)?;
    let offer = wait_for(DHCP_OFFER)?;
    let request = build_dhcp(DHCP_REQUEST, xid, mac, Some(offer.your_ip), Some(offer.server_id));
    socket.send_to([255; 4], SERVER_PORT, &request)?;
    let ack = wait_for(DHCP_ACK)?;
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
