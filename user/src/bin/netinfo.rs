//! Shows the network configuration and traffic counters.

#![no_std]
#![no_main]

use ulib::{eprintln, println, sys};

ulib::entry!(main);

fn main(_args: ulib::Args) -> i32 {
    let mut i = sys::NetInfo::default();
    if sys::net_info(&mut i) < 0 {
        eprintln!("netinfo: no network card");
        return 1;
    }
    let m = i.mac;
    let [a, b, c, d] = i.ip;
    let [ma, mb, mc, md] = i.netmask;
    let [ga, gb, gc, gd] = i.gateway;
    println!("eth0: e1000, link {}", if i.link_up != 0 { "up" } else { "down" });
    println!(
        "  MAC      {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    );
    println!("  IPv4     {}.{}.{}.{} netmask {}.{}.{}.{}", a, b, c, d, ma, mb, mc, md);
    println!("  gateway  {}.{}.{}.{}", ga, gb, gc, gd);
    let [da, db, dc, dd] = i.dns;
    println!("  DNS      {}.{}.{}.{}", da, db, dc, dd);
    println!("  config   {}", if i.dhcp != 0 { "DHCP" } else { "static" });
    println!("  frames   {} received, {} sent", i.rx_frames, i.tx_frames);
    0
}
