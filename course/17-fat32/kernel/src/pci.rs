//! PCI: how the kernel finds the devices of the machine.
//!
//! Every PCI device has a 256-byte *configuration space*: who made it (vendor and device ID), what it is (class,
//! subclass, programming interface) and where its registers are (the base address registers, BARs). We reach it
//! through two I/O ports - write an address to 0xCF8, then read or write the double word at 0xCFC (the "legacy
//! configuration mechanism", PCI Local Bus Specification 3.0, section 3.2.2.3.2).

use crate::port::{inl, outl};
use alloc::vec::Vec;
use core::fmt;
use spin::Mutex;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

// Offsets in the configuration space (header type 0)
const VENDOR_DEVICE: u8 = 0x00;
const COMMAND: u8 = 0x04;
const CLASS: u8 = 0x08;
const HEADER_TYPE: u8 = 0x0C;
const BAR0: u8 = 0x10;

/// Command register bits: answer accesses to the memory BARs, and read and write RAM by itself (DMA).
const COMMAND_MEMORY: u32 = 1 << 1;
const COMMAND_BUS_MASTER: u32 = 1 << 2;

/// An access is two steps - address, then data - so two CPUs must not mix theirs up.
static CONFIG: Mutex<()> = Mutex::new(());

/// Where a device sits: bus (0-255), device (0-31), function (0-7).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl fmt::Debug for Location {
    /// The way `lspci` writes it: 00:1f.2
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.bus, self.device, self.function)
    }
}

impl Location {
    /// What goes to 0xCF8: bit 31 "enable", then bus, device, function and the offset of a double word.
    fn address(self, offset: u8) -> u32 {
        1 << 31
            | (self.bus as u32) << 16
            | (self.device as u32) << 11
            | (self.function as u32) << 8
            | (offset & 0xFC) as u32
    }

    pub fn read(self, offset: u8) -> u32 {
        let _lock = CONFIG.lock();
        unsafe {
            outl(CONFIG_ADDRESS, self.address(offset));
            inl(CONFIG_DATA)
        }
    }

    pub fn write(self, offset: u8, value: u32) {
        let _lock = CONFIG.lock();
        unsafe {
            outl(CONFIG_ADDRESS, self.address(offset));
            outl(CONFIG_DATA, value);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub location: Location,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

impl Device {
    /// The physical address a memory BAR points to - `None` for a BAR in I/O port space. A 64-bit BAR takes two
    /// slots: the high half is in the next one.
    pub fn memory_bar(&self, index: u8) -> Option<u64> {
        let offset = BAR0 + index * 4;
        let low = self.location.read(offset);
        if low & 1 != 0 {
            return None;
        }
        let base = (low & !0xF) as u64; // the low 4 bits are flags
        let is_64bit = (low >> 1) & 0b11 == 0b10;
        Some(if is_64bit {
            base | (self.location.read(offset + 4) as u64) << 32
        } else {
            base
        })
    }

    /// Lets the device answer at its memory BARs and access RAM itself. The command register is the low half of
    /// its double word; the high half is the status register, whose bits are cleared by writing 1 - we write 0
    /// there, which changes nothing.
    pub fn enable_dma(&self) {
        let command = self.location.read(COMMAND) & 0xFFFF;
        self.location
            .write(COMMAND, command | COMMAND_MEMORY | COMMAND_BUS_MASTER);
    }

    #[cfg(test)]
    pub fn command(&self) -> u16 {
        self.location.read(COMMAND) as u16
    }

    /// A few classes by name (PCI Code and ID Assignment Specification); enough for a QEMU machine.
    pub fn kind(&self) -> &'static str {
        match (self.class, self.subclass) {
            (0x01, 0x06) => "SATA controller",
            (0x01, _) => "storage controller",
            (0x02, _) => "network controller",
            (0x03, _) => "display controller",
            (0x06, 0x00) => "host bridge",
            (0x06, 0x01) => "ISA bridge",
            (0x06, _) => "bridge",
            (0x0C, 0x05) => "SMBus controller",
            _ => "other device",
        }
    }
}

/// The device at `location`, if there is one: nobody there answers vendor 0xFFFF.
fn probe(location: Location) -> Option<Device> {
    let id = location.read(VENDOR_DEVICE);
    if id as u16 == 0xFFFF {
        return None;
    }
    let class = location.read(CLASS);
    Some(Device {
        location,
        vendor: id as u16,
        device: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
    })
}

/// Every device on every bus - by trying all 256 x 32 places. Functions 1 to 7 are asked only if function 0 says
/// it has more than one (bit 7 of its header type).
pub fn enumerate() -> Vec<Device> {
    let mut devices = Vec::new();
    for bus in 0..=255 {
        for device in 0..32 {
            let Some(first) = probe(Location {
                bus,
                device,
                function: 0,
            }) else {
                continue;
            };
            devices.push(first);
            let header_type = (first.location.read(HEADER_TYPE) >> 16) as u8;
            if header_type & 0x80 != 0 {
                devices.extend((1..8).filter_map(|function| probe(Location { bus, device, function })));
            }
        }
    }
    devices
}

/// The first device of a class, subclass and programming interface.
pub fn find(class: u8, subclass: u8, prog_if: u8) -> Option<Device> {
    enumerate()
        .into_iter()
        .find(|d| (d.class, d.subclass, d.prog_if) == (class, subclass, prog_if))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn the_q35_host_bridge_is_device_zero() {
        let devices = enumerate();
        let host = devices[0];
        assert_eq!(
            host.location,
            Location {
                bus: 0,
                device: 0,
                function: 0
            }
        );
        assert_eq!((host.vendor, host.device), (0x8086, 0x29C0)); // Intel 82G33, QEMU's q35
        assert_eq!(host.kind(), "host bridge");
    }

    #[test_case]
    fn the_sata_controller_is_found() {
        let ahci = find(0x01, 0x06, 0x01).expect("q35 has an AHCI controller");
        assert_eq!((ahci.vendor, ahci.device), (0x8086, 0x2922)); // ICH9
        assert_eq!(
            ahci.location,
            Location {
                bus: 0,
                device: 0x1F,
                function: 2
            }
        );
        let bar = ahci.memory_bar(5).expect("BAR5 is a memory BAR");
        assert!(bar != 0 && bar % 0x1000 == 0, "{:#x}", bar);
    }

    #[test_case]
    fn nothing_answers_where_there_is_no_device() {
        assert!(
            probe(Location {
                bus: 0,
                device: 31,
                function: 7
            })
            .is_none()
        );
        assert!(
            probe(Location {
                bus: 255,
                device: 31,
                function: 0
            })
            .is_none()
        );
    }

    #[test_case]
    fn dma_is_on_after_init() {
        let ahci = find(0x01, 0x06, 0x01).unwrap();
        assert_eq!(
            ahci.command() as u32 & (COMMAND_MEMORY | COMMAND_BUS_MASTER),
            COMMAND_MEMORY | COMMAND_BUS_MASTER
        );
    }
}
