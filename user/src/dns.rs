//! DNS client (RFC 1035): asks a name server for the IPv4 address (A record)
//! of a name, over UDP port 53. The default server comes from DHCP
//! (`sys::net_info`).

use crate::sys;
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    BadName,
    /// The server says the name does not exist.
    NotFound,
    /// The server answered with an error code.
    ServerFailure(u8),
    NoAnswer,
    Malformed,
    /// No network, socket error or timeout (-errno).
    Net(i64),
}

impl fmt::Display for DnsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DnsError::BadName => write!(f, "invalid name"),
            DnsError::NotFound => write!(f, "no such host"),
            DnsError::ServerFailure(c) => write!(f, "server error {}", c),
            DnsError::NoAnswer => write!(f, "no address in the answer"),
            DnsError::Malformed => write!(f, "malformed answer"),
            DnsError::Net(e) => write!(f, "{}", crate::fs::Error(-e)),
        }
    }
}

const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;

fn be16(p: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*p.get(i)?, *p.get(i + 1)?]))
}

/// A query for the A record of `name` with transaction id `id`.
pub fn build_query(id: u16, name: &str) -> Result<Vec<u8>, DnsError> {
    let name = name.trim_end_matches('.');
    if name.is_empty() || name.len() > 253 {
        return Err(DnsError::BadName);
    }
    let mut q = Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00]); // recursion desired
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // 1 question
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(DnsError::BadName);
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&TYPE_A.to_be_bytes());
    q.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(q)
}

/// Skips a (possibly compressed) name starting at `i`; returns the index after it.
fn skip_name(p: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let len = *p.get(i)?;
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(i + 2); // a pointer ends the name
        }
        i += 1 + len as usize;
    }
}

/// The IPv4 addresses (A records) in the answer to query `id`.
pub fn parse_response(p: &[u8], id: u16) -> Result<Vec<[u8; 4]>, DnsError> {
    if p.len() < 12 || be16(p, 0) != Some(id) || p[2] & 0x80 == 0 {
        return Err(DnsError::Malformed);
    }
    match p[3] & 0x0F {
        0 => {}
        3 => return Err(DnsError::NotFound),
        c => return Err(DnsError::ServerFailure(c)),
    }
    let questions = be16(p, 4).ok_or(DnsError::Malformed)?;
    let answers = be16(p, 6).ok_or(DnsError::Malformed)?;
    let mut i = 12;
    for _ in 0..questions {
        i = skip_name(p, i).ok_or(DnsError::Malformed)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..answers {
        i = skip_name(p, i).ok_or(DnsError::Malformed)?;
        let (rtype, class) = (
            be16(p, i).ok_or(DnsError::Malformed)?,
            be16(p, i + 2).ok_or(DnsError::Malformed)?,
        );
        let rdlen = be16(p, i + 8).ok_or(DnsError::Malformed)? as usize;
        let data = p.get(i + 10..i + 10 + rdlen).ok_or(DnsError::Malformed)?;
        if rtype == TYPE_A && class == CLASS_IN && rdlen == 4 {
            out.push([data[0], data[1], data[2], data[3]]);
        }
        i += 10 + rdlen;
    }
    if out.is_empty() {
        Err(DnsError::NoAnswer)
    } else {
        Ok(out)
    }
}

/// The name server from DHCP (or the static fallback).
pub fn default_server() -> [u8; 4] {
    let mut info = sys::NetInfo::default();
    sys::net_info(&mut info);
    info.dns
}

/// The first address of `name` (an IP literal is returned as it is).
pub fn resolve(name: &str, server: [u8; 4], port: u16) -> Result<[u8; 4], DnsError> {
    if let Some(ip) = sys::parse_ip(name) {
        return Ok(ip);
    }
    let fd = sys::udp_socket(0);
    if fd < 0 {
        return Err(DnsError::Net(fd));
    }
    let fd = fd as u64;
    let id = (sys::clock_us() & 0xFFFF) as u16;
    let query = build_query(id, name)?;
    let mut result = Err(DnsError::Net(-110));
    'tries: for _ in 0..3 {
        let sent = sys::sendto(fd, &query, server, port);
        if sent < 0 {
            result = Err(DnsError::Net(sent));
            break;
        }
        let mut buf = [0u8; 512];
        let mut from = sys::SockAddr::default();
        loop {
            let n = sys::recvfrom(fd, &mut buf, &mut from, 1500);
            if n < 0 {
                result = Err(DnsError::Net(n));
                continue 'tries; // timeout: ask again
            }
            match parse_response(&buf[..n as usize], id) {
                Err(DnsError::Malformed) => continue, // not our answer
                r => {
                    result = r.map(|ips| ips[0]);
                    break 'tries;
                }
            }
        }
    }
    sys::close(fd);
    result
}

/// Checks for `host --selftest`; returns the descriptions of failed ones.
pub fn selftest() -> Vec<&'static str> {
    let mut failed = Vec::new();
    let mut check = |ok: bool, what: &'static str| {
        if !ok {
            failed.push(what);
        }
    };
    let q = build_query(0x1234, "www.example.test").unwrap();
    check(&q[12..30] == b"\x03www\x07example\x04test\x00", "query name encoding");
    check(
        build_query(1, "a..b").is_err() && build_query(1, "").is_err(),
        "invalid names rejected",
    );
    // an answer with a compressed name pointing at the question (offset 12)
    let mut r = q.clone();
    r[2] = 0x81;
    r[3] = 0x80; // response, no error
    r[7] = 2; // two answers
    for ip in [[93, 184, 216, 34], [1, 2, 3, 4]] {
        r.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0x0E, 0x10, 0, 4]);
        r.extend_from_slice(&ip);
    }
    check(
        parse_response(&r, 0x1234) == Ok(alloc::vec![[93, 184, 216, 34], [1, 2, 3, 4]]),
        "two A records",
    );
    check(
        parse_response(&r, 0x9999) == Err(DnsError::Malformed),
        "wrong id rejected",
    );
    let mut nx = q.clone();
    nx[2] = 0x81;
    nx[3] = 0x83; // NXDOMAIN
    check(parse_response(&nx, 0x1234) == Err(DnsError::NotFound), "NXDOMAIN");
    check(
        parse_response(&r[..r.len() - 2], 0x1234) == Err(DnsError::Malformed),
        "truncated answer",
    );
    check(resolve("10.0.2.2", [0; 4], 53) == Ok([10, 0, 2, 2]), "IP literal");
    failed
}
