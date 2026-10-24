//! CMOS real-time clock: the battery-backed wall clock of the PC.
//! Registers are read through ports 0x70 (index) and 0x71 (data). Values
//! may be BCD-encoded and the clock may be mid-update, so we read until two
//! consecutive readings agree.

use crate::arch::x86_64::port::{inb, outb};
use crate::arch::x86_64::without_interrupts;
use core::fmt;

const INDEX: u16 = 0x70;
const DATA: u16 = 0x71;
const NMI_DISABLE: u8 = 0x80;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_CENTURY: u8 = 0x32;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

fn read_register(reg: u8) -> u8 {
    unsafe {
        outb(INDEX, NMI_DISABLE | reg);
        inb(DATA)
    }
}

fn bcd_to_binary(value: u8) -> u8 {
    (value & 0x0F) + (value >> 4) * 10
}

fn read_raw() -> [u8; 7] {
    while read_register(REG_STATUS_A) & 0x80 != 0 {
        core::hint::spin_loop(); // update in progress
    }
    [
        read_register(REG_SECONDS),
        read_register(REG_MINUTES),
        read_register(REG_HOURS),
        read_register(REG_DAY),
        read_register(REG_MONTH),
        read_register(REG_YEAR),
        read_register(REG_CENTURY),
    ]
}

/// Current date and time (UTC in QEMU by default).
pub fn now() -> DateTime {
    without_interrupts(|| {
        let mut raw = read_raw();
        loop {
            let again = read_raw();
            if again == raw {
                break;
            }
            raw = again;
        }
        let status_b = read_register(REG_STATUS_B);
        let binary = status_b & 0x04 != 0;
        let hour_24 = status_b & 0x02 != 0;
        let conv = |v: u8| if binary { v } else { bcd_to_binary(v) };

        let pm = raw[2] & 0x80 != 0;
        let mut hour = conv(raw[2] & 0x7F);
        if !hour_24 {
            hour = match (hour, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (h, true) => h + 12,
                (h, false) => h,
            };
        }
        let century = match conv(raw[6]) {
            c @ 19..=21 => c as u16,
            _ => 20, // no century register: assume 20xx
        };
        DateTime {
            year: century * 100 + conv(raw[5]) as u16,
            month: conv(raw[4]),
            day: conv(raw[3]),
            hour,
            minute: conv(raw[1]),
            second: conv(raw[0]),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn bcd_conversion() {
        assert_eq!(bcd_to_binary(0x59), 59);
        assert_eq!(bcd_to_binary(0x00), 0);
    }

    #[test_case]
    fn clock_returns_plausible_date() {
        let t = now();
        assert!((2024..2100).contains(&t.year), "year {}", t.year);
        assert!((1..=12).contains(&t.month) && (1..=31).contains(&t.day));
        assert!(t.hour < 24 && t.minute < 60 && t.second < 60);
    }
}
