//! Integration test (M58): TCP when packets get lost. Loss is injected on
//! the loopback path (`net::set_loopback_loss`), so both directions of a
//! connection to ourselves lose packets – data, ACKs, SYN, SYN-ACK and FIN.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::testing::runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel::net::tcp::{self, TcpError};
use kernel::net::{self, packet::Ipv4Addr};
use kernel::{task, time};

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    test_main();
    kernel::qemu::exit(kernel::qemu::ExitCode::Success)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}

fn me() -> Ipv4Addr {
    net::config().expect("network configured").ip
}

/// Sends `len` bytes to an echo server on `port` and returns what came back.
fn echo(port: u16, len: usize) -> Vec<u8> {
    tcp::listen(port).unwrap();
    let server = task::spawn("echo", move || {
        let (id, _, _) = tcp::accept(port, || false).unwrap();
        loop {
            let data = tcp::recv(id, 4096, || false).unwrap();
            if data.is_empty() {
                break;
            }
            tcp::send(id, &data, || false).unwrap();
        }
        tcp::close(id);
    });
    let id = tcp::connect(me(), port, 10_000, || false).expect("connect despite losses");
    let data: Vec<u8> = (0..len as u32).map(|i| (i % 251) as u8).collect();
    let sender = {
        let data = data.clone();
        task::spawn("send", move || {
            assert_eq!(tcp::send(id, &data, || false), Ok(data.len()));
            tcp::shutdown(id);
        })
    };
    let mut got = Vec::new();
    loop {
        let chunk = tcp::recv(id, 8192, || false).expect("recv despite losses");
        if chunk.is_empty() {
            break;
        }
        got.extend_from_slice(&chunk);
    }
    task::join(sender);
    task::join(server);
    tcp::close(id);
    tcp::unlisten(port);
    assert!(got == data, "echoed data differs ({} of {} bytes)", got.len(), data.len());
    got
}

#[test_case]
fn every_fifth_packet_lost_the_data_still_arrives() {
    let before = tcp::retransmissions();
    // phase 1..=3: the first lost packet is the SYN, the SYN-ACK, the first ACK
    for phase in 1..=3 {
        net::set_loopback_loss(5, phase);
        echo(7300 + phase as u16, 20_000);
    }
    net::set_loopback_loss(0, 0);
    let retransmitted = tcp::retransmissions() - before;
    kernel::serial_println!("    {} retransmission timeouts", retransmitted);
    assert!(retransmitted >= 3, "losses must have been repaired by retransmission");
}

#[test_case]
fn connecting_into_silence_times_out() {
    tcp::listen(7310).unwrap();
    net::set_loopback_loss(1, 0); // every packet
    let start = time::uptime_ms();
    let result = tcp::connect(me(), 7310, 1500, || false);
    let took = time::uptime_ms() - start;
    net::set_loopback_loss(0, 0);
    tcp::unlisten(7310);
    assert_eq!(result, Err(TcpError::TimedOut));
    assert!((1500..5000).contains(&took), "connect gave up after {} ms", took);
}

#[test_case]
fn a_peer_that_goes_silent_ends_the_connection() {
    static SERVER_ID: AtomicU64 = AtomicU64::new(0);
    tcp::set_max_retries(3);
    tcp::listen(7320).unwrap();
    let server = task::spawn("silent", || {
        let (id, _, _) = tcp::accept(7320, || false).unwrap();
        SERVER_ID.store(id, Ordering::SeqCst);
    });
    let id = tcp::connect(me(), 7320, 3000, || false).unwrap();
    task::join(server);
    // some acknowledged data first: the round trip time is measured, the timeout drops to 200 ms
    assert_eq!(tcp::send(id, &[1; 100], || false), Ok(100));
    time::sleep_ms(100);
    net::set_loopback_loss(1, 0); // the peer is gone: nothing arrives any more
    let start = time::uptime_ms();
    assert_eq!(tcp::send(id, &[7; 10_000], || false), Ok(10_000)); // buffered
    let result = tcp::recv(id, 100, || false);
    let took = time::uptime_ms() - start;
    net::set_loopback_loss(0, 0);
    tcp::set_max_retries(8);
    assert_eq!(result, Err(TcpError::TimedOut));
    // 3 retransmissions, the timeout doubling from 200 ms: 0.2 + 0.4 + 0.8 + 1.6 s
    kernel::serial_println!("    gave up after {} ms", took);
    assert!((2000..8000).contains(&took), "gave up after {} ms", took);
    assert_eq!(tcp::send(id, b"more", || false).err(), Some(TcpError::TimedOut), "the error stays");
    tcp::close(id);
    tcp::close(SERVER_ID.load(Ordering::SeqCst));
    tcp::unlisten(7320);
}
