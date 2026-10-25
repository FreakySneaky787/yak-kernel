//! Building and parsing the packets we speak: Ethernet II, ARP, IPv4, ICMP
//! echo and UDP. Pure functions on byte slices (network byte order = big
//! endian), unit-tested without a network card.

use alloc::vec::Vec;

pub type Mac = [u8; 6];
pub type Ipv4Addr = [u8; 4];

pub const BROADCAST_MAC: Mac = [0xFF; 6];
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_UDP: u8 = 17;

const ETH_HEADER: usize = 14;
const IPV4_HEADER: usize = 20;
const UDP_HEADER: usize = 8;
const ICMP_HEADER: usize = 8;

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

/// The Internet checksum (RFC 1071): one's complement of the one's
/// complement sum of all 16-bit words.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], 0])
        };
        sum += word as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

// ---- Ethernet ----

#[derive(Debug, PartialEq, Eq)]
pub struct EthFrame<'a> {
    pub dst: Mac,
    pub src: Mac,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

pub fn parse_eth(frame: &[u8]) -> Option<EthFrame<'_>> {
    if frame.len() < ETH_HEADER {
        return None;
    }
    Some(EthFrame {
        dst: frame[0..6].try_into().unwrap(),
        src: frame[6..12].try_into().unwrap(),
        ethertype: be16(frame, 12),
        payload: &frame[ETH_HEADER..],
    })
}

pub fn build_eth(dst: Mac, src: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(ETH_HEADER + payload.len());
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

// ---- ARP (IPv4 over Ethernet only) ----

pub const ARP_REQUEST: u16 = 1;
pub const ARP_REPLY: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arp {
    pub op: u16,
    pub sender_mac: Mac,
    pub sender_ip: Ipv4Addr,
    pub target_mac: Mac,
    pub target_ip: Ipv4Addr,
}

pub fn parse_arp(p: &[u8]) -> Option<Arp> {
    // hardware type 1 (Ethernet), protocol 0x0800, lengths 6 and 4
    if p.len() < 28 || be16(p, 0) != 1 || be16(p, 2) != ETHERTYPE_IPV4 || p[4] != 6 || p[5] != 4 {
        return None;
    }
    Some(Arp {
        op: be16(p, 6),
        sender_mac: p[8..14].try_into().unwrap(),
        sender_ip: p[14..18].try_into().unwrap(),
        target_mac: p[18..24].try_into().unwrap(),
        target_ip: p[24..28].try_into().unwrap(),
    })
}

pub fn build_arp(a: &Arp) -> Vec<u8> {
    let mut p = Vec::with_capacity(28);
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    p.extend_from_slice(&[6, 4]);
    p.extend_from_slice(&a.op.to_be_bytes());
    p.extend_from_slice(&a.sender_mac);
    p.extend_from_slice(&a.sender_ip);
    p.extend_from_slice(&a.target_mac);
    p.extend_from_slice(&a.target_ip);
    p
}

// ---- IPv4 ----

#[derive(Debug, PartialEq, Eq)]
pub struct Ipv4Packet<'a> {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub protocol: u8,
    pub ttl: u8,
    pub payload: &'a [u8],
}

/// Parses an IPv4 packet; checks version, header length, total length and
/// the header checksum. Fragments are not supported (dropped).
pub fn parse_ipv4(p: &[u8]) -> Option<Ipv4Packet<'_>> {
    if p.len() < IPV4_HEADER || p[0] >> 4 != 4 {
        return None;
    }
    let ihl = (p[0] & 0x0F) as usize * 4;
    let total = be16(p, 2) as usize;
    if ihl < IPV4_HEADER || total < ihl || total > p.len() || checksum(&p[..ihl]) != 0 {
        return None;
    }
    let flags_fragment = be16(p, 6);
    if flags_fragment & 0x3FFF != 0 {
        return None; // more fragments or a fragment offset
    }
    Some(Ipv4Packet {
        src: p[12..16].try_into().unwrap(),
        dst: p[16..20].try_into().unwrap(),
        protocol: p[9],
        ttl: p[8],
        payload: &p[ihl..total],
    })
}

pub fn build_ipv4(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, id: u16, payload: &[u8]) -> Vec<u8> {
    let total = (IPV4_HEADER + payload.len()) as u16;
    let mut p = Vec::with_capacity(total as usize);
    p.extend_from_slice(&[0x45, 0]); // version 4, 5 words header, no TOS
    p.extend_from_slice(&total.to_be_bytes());
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0x4000u16.to_be_bytes()); // don't fragment
    p.extend_from_slice(&[64, protocol, 0, 0]); // TTL, protocol, checksum placeholder
    p.extend_from_slice(&src);
    p.extend_from_slice(&dst);
    let sum = checksum(&p[..IPV4_HEADER]);
    p[10..12].copy_from_slice(&sum.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

// ---- ICMP echo ----

pub const ICMP_ECHO_REPLY: u8 = 0;
pub const ICMP_ECHO_REQUEST: u8 = 8;

#[derive(Debug, PartialEq, Eq)]
pub struct IcmpEcho<'a> {
    pub kind: u8,
    pub id: u16,
    pub seq: u16,
    pub data: &'a [u8],
}

pub fn parse_icmp_echo(p: &[u8]) -> Option<IcmpEcho<'_>> {
    if p.len() < ICMP_HEADER || checksum(p) != 0 || p[1] != 0 {
        return None;
    }
    if p[0] != ICMP_ECHO_REQUEST && p[0] != ICMP_ECHO_REPLY {
        return None;
    }
    Some(IcmpEcho {
        kind: p[0],
        id: be16(p, 4),
        seq: be16(p, 6),
        data: &p[ICMP_HEADER..],
    })
}

pub fn build_icmp_echo(kind: u8, id: u16, seq: u16, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(ICMP_HEADER + data.len());
    p.extend_from_slice(&[kind, 0, 0, 0]);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(data);
    let sum = checksum(&p);
    p[2..4].copy_from_slice(&sum.to_be_bytes());
    p
}

// ---- UDP ----

#[derive(Debug, PartialEq, Eq)]
pub struct UdpDatagram<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: &'a [u8],
}

pub fn parse_udp(p: &[u8]) -> Option<UdpDatagram<'_>> {
    if p.len() < UDP_HEADER {
        return None;
    }
    let len = be16(p, 4) as usize;
    if len < UDP_HEADER || len > p.len() {
        return None;
    }
    Some(UdpDatagram {
        src_port: be16(p, 0),
        dst_port: be16(p, 2),
        payload: &p[UDP_HEADER..len],
    })
}

/// A UDP datagram with checksum, which covers a pseudo header with the IP
/// addresses (RFC 768).
pub fn build_udp(src: Ipv4Addr, dst: Ipv4Addr, src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let len = (UDP_HEADER + payload.len()) as u16;
    let mut p = Vec::with_capacity(len as usize);
    p.extend_from_slice(&src_port.to_be_bytes());
    p.extend_from_slice(&dst_port.to_be_bytes());
    p.extend_from_slice(&len.to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(payload);
    let mut pseudo = Vec::with_capacity(12 + p.len());
    pseudo.extend_from_slice(&src);
    pseudo.extend_from_slice(&dst);
    pseudo.extend_from_slice(&[0, PROTO_UDP]);
    pseudo.extend_from_slice(&len.to_be_bytes());
    pseudo.extend_from_slice(&p);
    let sum = match checksum(&pseudo) {
        0 => 0xFFFF, // 0 means "no checksum" in UDP
        s => s,
    };
    p[6..8].copy_from_slice(&sum.to_be_bytes());
    p
}

// ---- TCP ----

pub const PROTO_TCP: u8 = 6;
pub const TCP_FIN: u8 = 1 << 0;
pub const TCP_SYN: u8 = 1 << 1;
pub const TCP_RST: u8 = 1 << 2;
pub const TCP_PSH: u8 = 1 << 3;
pub const TCP_ACK: u8 = 1 << 4;
const TCP_HEADER: usize = 20;

#[derive(Debug, PartialEq, Eq)]
pub struct TcpSegment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    /// From the MSS option (SYN segments), if present.
    pub mss: Option<u16>,
    /// From the window scale option (SYN segments, RFC 7323), if present.
    pub wscale: Option<u8>,
    pub payload: &'a [u8],
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}

fn transport_checksum(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, segment: &[u8]) -> u16 {
    let mut pseudo = Vec::with_capacity(12 + segment.len());
    pseudo.extend_from_slice(&src);
    pseudo.extend_from_slice(&dst);
    pseudo.extend_from_slice(&[0, protocol]);
    pseudo.extend_from_slice(&(segment.len() as u16).to_be_bytes());
    pseudo.extend_from_slice(segment);
    checksum(&pseudo)
}

/// Parses a TCP segment and verifies its checksum (`src`/`dst` = IP addresses).
pub fn parse_tcp(src: Ipv4Addr, dst: Ipv4Addr, p: &[u8]) -> Option<TcpSegment<'_>> {
    if p.len() < TCP_HEADER || transport_checksum(src, dst, PROTO_TCP, p) != 0 {
        return None;
    }
    let offset = (p[12] >> 4) as usize * 4;
    if offset < TCP_HEADER || offset > p.len() {
        return None;
    }
    // options: MSS (kind 2) and window scale (kind 3) interest us
    let mut mss = None;
    let mut wscale = None;
    let mut i = TCP_HEADER;
    while i < offset {
        match p[i] {
            0 => break,
            1 => i += 1,
            kind => {
                let len = *p.get(i + 1)? as usize;
                if len < 2 || i + len > offset {
                    break;
                }
                if kind == 2 && len == 4 {
                    mss = Some(be16(p, i + 2));
                }
                if kind == 3 && len == 3 {
                    wscale = Some(p[i + 2].min(14)); // RFC 7323: larger shifts count as 14
                }
                i += len;
            }
        }
    }
    Some(TcpSegment {
        src_port: be16(p, 0),
        dst_port: be16(p, 2),
        seq: be32(p, 4),
        ack: be32(p, 8),
        flags: p[13],
        window: be16(p, 14),
        mss,
        wscale,
        payload: &p[offset..],
    })
}

/// Builds a TCP segment with checksum; SYN segments carry an MSS option
/// and, with `wscale`, a window scale option (NOP + kind 3).
#[allow(clippy::too_many_arguments)]
pub fn build_tcp(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    wscale: Option<u8>,
    payload: &[u8],
) -> Vec<u8> {
    let mut options: Vec<u8> = Vec::new();
    if flags & TCP_SYN != 0 {
        options.extend_from_slice(&[2, 4, 0x05, 0xB4]); // MSS 1460
        if let Some(shift) = wscale {
            options.extend_from_slice(&[1, 3, 3, shift]);
        }
    }
    let offset = TCP_HEADER + options.len();
    let mut p = Vec::with_capacity(offset + payload.len());
    p.extend_from_slice(&src_port.to_be_bytes());
    p.extend_from_slice(&dst_port.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&ack.to_be_bytes());
    p.extend_from_slice(&[((offset / 4) as u8) << 4, flags]);
    p.extend_from_slice(&window.to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0]); // checksum, urgent pointer
    p.extend_from_slice(&options);
    p.extend_from_slice(payload);
    let sum = transport_checksum(src, dst, PROTO_TCP, &p);
    p[16..18].copy_from_slice(&sum.to_be_bytes());
    p
}

/// Sequence number comparison modulo 2^32: is `a` before `b`?
pub fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

pub fn seq_le(a: u32, b: u32) -> bool {
    a == b || seq_lt(a, b)
}

// ---- DHCP (RFC 2131: BOOTP message + options) ----

pub const DHCP_DISCOVER: u8 = 1;
pub const DHCP_OFFER: u8 = 2;
pub const DHCP_REQUEST: u8 = 3;
pub const DHCP_ACK: u8 = 5;
pub const DHCP_NAK: u8 = 6;
const DHCP_MAGIC: u32 = 0x6382_5363;
const DHCP_OPTIONS: usize = 240; // fixed BOOTP part + magic cookie

/// The parts of a DHCP reply we use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DhcpReply {
    pub msg_type: u8,
    pub your_ip: Ipv4Addr,
    pub server_id: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub router: Ipv4Addr,
    pub dns: Ipv4Addr,
    pub lease_secs: u32,
}

/// A REQUEST that extends the lease of `ciaddr` (RENEWING / REBINDING):
/// the address is in `ciaddr`, no requested-address or server option, no
/// broadcast flag (we have an address, the server answers it directly).
pub fn build_dhcp_renew(xid: u32, mac: Mac, ciaddr: Ipv4Addr) -> Vec<u8> {
    let mut p = alloc::vec![0u8; DHCP_OPTIONS];
    p[0] = 1; // BOOTREQUEST
    p[1] = 1; // Ethernet
    p[2] = 6;
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[12..16].copy_from_slice(&ciaddr);
    p[28..34].copy_from_slice(&mac);
    p[236..240].copy_from_slice(&DHCP_MAGIC.to_be_bytes());
    p.extend_from_slice(&[53, 1, DHCP_REQUEST]);
    p.extend_from_slice(&[55, 3, 1, 3, 6]);
    p.push(255);
    p
}

/// A client message (DISCOVER or REQUEST) with the broadcast flag set, so
/// that the server answers before we have an address.
pub fn build_dhcp(msg_type: u8, xid: u32, mac: Mac, requested: Option<Ipv4Addr>, server: Option<Ipv4Addr>) -> Vec<u8> {
    let mut p = alloc::vec![0u8; DHCP_OPTIONS];
    p[0] = 1; // BOOTREQUEST
    p[1] = 1; // Ethernet
    p[2] = 6;
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast flag
    p[28..34].copy_from_slice(&mac);
    p[236..240].copy_from_slice(&DHCP_MAGIC.to_be_bytes());
    p.extend_from_slice(&[53, 1, msg_type]);
    if let Some(ip) = requested {
        p.extend_from_slice(&[50, 4]);
        p.extend_from_slice(&ip);
    }
    if let Some(ip) = server {
        p.extend_from_slice(&[54, 4]);
        p.extend_from_slice(&ip);
    }
    p.extend_from_slice(&[55, 3, 1, 3, 6]); // ask for netmask, router, DNS
    p.push(255);
    p
}

/// Parses a server reply for transaction `xid`.
pub fn parse_dhcp(p: &[u8], xid: u32) -> Option<DhcpReply> {
    if p.len() < DHCP_OPTIONS || p[0] != 2 || be32(p, 4) != xid || be32(p, 236) != DHCP_MAGIC {
        return None;
    }
    let mut r = DhcpReply {
        your_ip: p[16..20].try_into().unwrap(),
        ..DhcpReply::default()
    };
    let mut i = DHCP_OPTIONS;
    while i < p.len() {
        let code = p[i];
        if code == 255 {
            break;
        }
        if code == 0 {
            i += 1;
            continue;
        }
        let len = *p.get(i + 1)? as usize;
        let v = p.get(i + 2..i + 2 + len)?;
        let ip = || -> Ipv4Addr { v.get(..4).map_or([0; 4], |b| b.try_into().unwrap()) };
        match code {
            53 if len == 1 => r.msg_type = v[0],
            1 => r.netmask = ip(),
            3 => r.router = ip(),
            6 => r.dns = ip(),
            54 => r.server_id = ip(),
            51 if len == 4 => r.lease_secs = be32(v, 0),
            _ => {}
        }
        i += 2 + len;
    }
    (r.msg_type != 0).then_some(r)
}

/// "10.0.2.15" -> [10, 0, 2, 15]
pub fn parse_ipv4_addr(s: &str) -> Option<Ipv4Addr> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for b in &mut out {
        *b = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn checksum_matches_a_known_header() {
        // a classic example header with checksum 0xB861
        let mut h = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xC0, 0xA8, 0x00, 0x01, 0xC0, 0xA8,
            0x00, 0xC7,
        ];
        assert_eq!(checksum(&h), 0xB861);
        h[10] = 0xB8;
        h[11] = 0x61;
        assert_eq!(checksum(&h), 0, "a header with its checksum sums to zero");
        assert_eq!(checksum(&[0x01]), !0x0100u16, "odd length is padded");
    }

    #[test_case]
    fn ipv4_round_trip_and_validation() {
        let p = build_ipv4([10, 0, 2, 15], [10, 0, 2, 2], PROTO_UDP, 7, b"payload");
        let parsed = parse_ipv4(&p).unwrap();
        assert_eq!(parsed.src, [10, 0, 2, 15]);
        assert_eq!(parsed.dst, [10, 0, 2, 2]);
        assert_eq!((parsed.protocol, parsed.ttl), (PROTO_UDP, 64));
        assert_eq!(parsed.payload, b"payload");
        let mut broken = p.clone();
        broken[15] ^= 1; // corrupt the source address
        assert!(parse_ipv4(&broken).is_none(), "bad checksum accepted");
        let mut padded = p.clone();
        padded.extend_from_slice(&[0; 10]); // Ethernet padding after the packet
        assert_eq!(parse_ipv4(&padded).unwrap().payload, b"payload");
    }

    #[test_case]
    fn arp_and_ethernet_round_trip() {
        let a = Arp {
            op: ARP_REQUEST,
            sender_mac: [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            sender_ip: [10, 0, 2, 15],
            target_mac: [0; 6],
            target_ip: [10, 0, 2, 2],
        };
        let frame = build_eth(BROADCAST_MAC, a.sender_mac, ETHERTYPE_ARP, &build_arp(&a));
        let eth = parse_eth(&frame).unwrap();
        assert_eq!((eth.dst, eth.ethertype), (BROADCAST_MAC, ETHERTYPE_ARP));
        assert_eq!(parse_arp(eth.payload), Some(a));
        assert!(parse_arp(&eth.payload[..20]).is_none());
    }

    #[test_case]
    fn tcp_round_trip_and_sequence_math() {
        let (a, b) = ([10, 0, 2, 15], [10, 0, 2, 2]);
        let syn = build_tcp(a, b, 40000, 80, 0xFFFF_FFF0, 0, TCP_SYN, 32768, Some(7), b"");
        let s = parse_tcp(a, b, &syn).unwrap();
        assert_eq!(
            (s.src_port, s.dst_port, s.seq, s.flags, s.window),
            (40000, 80, 0xFFFF_FFF0, TCP_SYN, 32768)
        );
        assert_eq!((s.mss, s.wscale), (Some(1460), Some(7)));
        let plain_syn = build_tcp(a, b, 40000, 80, 1, 0, TCP_SYN, 1000, None, b"");
        assert_eq!(parse_tcp(a, b, &plain_syn).unwrap().wscale, None);
        let data = build_tcp(a, b, 1, 2, 5, 9, TCP_ACK | TCP_PSH, 100, Some(7), b"hi there");
        let d = parse_tcp(a, b, &data).unwrap();
        assert_eq!((d.ack, d.mss, d.wscale, d.payload), (9, None, None, &b"hi there"[..]));
        assert!(
            parse_tcp([10, 0, 2, 16], b, &data).is_none(),
            "checksum must cover the IP addresses"
        );
        // wrap-around
        assert!(seq_lt(0xFFFF_FFF0, 0x10) && !seq_lt(0x10, 0xFFFF_FFF0));
        assert!(seq_le(7, 7) && seq_le(7, 8) && !seq_le(8, 7));
    }

    #[test_case]
    fn dhcp_messages() {
        let mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let d = build_dhcp(DHCP_REQUEST, 0xABCD, mac, Some([10, 0, 2, 15]), Some([10, 0, 2, 2]));
        assert_eq!((d[0], &d[28..34]), (1, &mac[..]));
        // a renewal names our address in ciaddr and asks for a unicast answer
        let r = build_dhcp_renew(0x1234, mac, [10, 0, 2, 15]);
        assert_eq!((&r[12..16], &r[10..12]), (&[10, 0, 2, 15][..], &[0, 0][..]));
        assert!(r.windows(3).any(|w| w == [53, 1, DHCP_REQUEST]) && !r.windows(2).any(|w| w == [50, 4]));
        assert!(
            d.windows(6).any(|w| w == [50, 4, 10, 0, 2, 15]),
            "requested address missing"
        );
        // turn it into a server reply: op 2, yiaddr, options OFFER + netmask + router + DNS + lease
        let mut r = d[..240].to_vec();
        r[0] = 2;
        r[16..20].copy_from_slice(&[10, 0, 2, 15]);
        r.extend_from_slice(&[
            53, 1, DHCP_OFFER, 1, 4, 255, 255, 255, 0, 3, 4, 10, 0, 2, 2, 6, 4, 10, 0, 2, 3,
        ]);
        r.extend_from_slice(&[54, 4, 10, 0, 2, 2, 51, 4, 0, 0, 0x0E, 0x10, 0, 255]);
        let p = parse_dhcp(&r, 0xABCD).unwrap();
        assert_eq!(
            (p.msg_type, p.your_ip, p.netmask),
            (DHCP_OFFER, [10, 0, 2, 15], [255, 255, 255, 0])
        );
        assert_eq!(
            (p.router, p.dns, p.server_id, p.lease_secs),
            ([10, 0, 2, 2], [10, 0, 2, 3], [10, 0, 2, 2], 3600)
        );
        assert!(parse_dhcp(&r, 0x1234).is_none(), "wrong transaction id accepted");
    }

    #[test_case]
    fn icmp_and_udp_round_trip() {
        let e = build_icmp_echo(ICMP_ECHO_REQUEST, 0x1234, 5, b"ping data");
        let p = parse_icmp_echo(&e).unwrap();
        assert_eq!(
            (p.kind, p.id, p.seq, p.data),
            (ICMP_ECHO_REQUEST, 0x1234, 5, &b"ping data"[..])
        );
        let u = build_udp([10, 0, 2, 15], [10, 0, 2, 2], 40000, 7, b"hello");
        let d = parse_udp(&u).unwrap();
        assert_eq!((d.src_port, d.dst_port, d.payload), (40000, 7, &b"hello"[..]));
        assert_ne!(u16::from_be_bytes([u[6], u[7]]), 0, "UDP checksum missing");
        assert_eq!(parse_ipv4_addr("10.0.2.2"), Some([10, 0, 2, 2]));
        assert_eq!(parse_ipv4_addr("10.0.2"), None);
        assert_eq!(parse_ipv4_addr("10.0.2.300"), None);
    }
}
