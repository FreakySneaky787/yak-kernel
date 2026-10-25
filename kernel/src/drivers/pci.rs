//! PCI bus enumeration via the legacy configuration mechanism (ports
//! 0xCF8 = address, 0xCFC = data). Every device has a 256-byte configuration
//! space with vendor/device IDs, class codes and base address registers (BARs).

use crate::arch::x86_64::port::{inl, outl};
use alloc::vec::Vec;
use core::fmt;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

const REG_VENDOR_DEVICE: u8 = 0x00;
const REG_COMMAND: u8 = 0x04;
const REG_CLASS: u8 = 0x08;
const REG_HEADER_TYPE: u8 = 0x0C;
const REG_BAR0: u8 = 0x10;
const REG_INTERRUPT: u8 = 0x3C;
const REG_STATUS: u8 = 0x06;
const REG_CAPABILITIES: u8 = 0x34;
const STATUS_CAPABILITY_LIST: u16 = 1 << 4;
const COMMAND_INTX_DISABLE: u16 = 1 << 10;
pub const CAP_MSI: u8 = 0x05;

const COMMAND_MEMORY_SPACE: u16 = 1 << 1;
const COMMAND_BUS_MASTER: u16 = 1 << 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.bus, self.device, self.function)
    }
}

impl Location {
    fn address(&self, offset: u8) -> u32 {
        1 << 31
            | (self.bus as u32) << 16
            | (self.device as u32) << 11
            | (self.function as u32) << 8
            | (offset & 0xFC) as u32
    }

    pub fn read32(&self, offset: u8) -> u32 {
        crate::arch::x86_64::without_interrupts(|| unsafe {
            outl(CONFIG_ADDRESS, self.address(offset));
            inl(CONFIG_DATA)
        })
    }

    pub fn write32(&self, offset: u8, value: u32) {
        crate::arch::x86_64::without_interrupts(|| unsafe {
            outl(CONFIG_ADDRESS, self.address(offset));
            outl(CONFIG_DATA, value);
        })
    }

    pub fn read16(&self, offset: u8) -> u16 {
        (self.read32(offset) >> ((offset & 2) * 8)) as u16
    }

    pub fn write16(&self, offset: u8, value: u16) {
        let shift = (offset & 2) * 8;
        let old = self.read32(offset);
        self.write32(offset, (old & !(0xFFFF << shift)) | (value as u32) << shift);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub location: Location,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub interrupt_line: u8,
}

impl Device {
    /// Physical base address of a memory BAR (handles 64-bit BARs).
    pub fn memory_bar(&self, index: u8) -> Option<u64> {
        let offset = REG_BAR0 + index * 4;
        let low = self.location.read32(offset);
        if low & 1 != 0 {
            return None; // I/O space BAR
        }
        let base = (low & !0xF) as u64;
        let is_64bit = (low >> 1) & 0b11 == 0b10;
        Some(if is_64bit {
            base | (self.location.read32(offset + 4) as u64) << 32
        } else {
            base
        })
    }

    /// Lets the device answer memory accesses and do DMA (bus mastering).
    pub fn enable_mmio_and_dma(&self) {
        let cmd = self.location.read16(REG_COMMAND);
        self.location
            .write16(REG_COMMAND, cmd | COMMAND_MEMORY_SPACE | COMMAND_BUS_MASTER);
    }

    /// Offset of the capability `id` in the configuration space, if the
    /// device has it (the capabilities form a linked list from 0x34).
    pub fn capability(&self, id: u8) -> Option<u8> {
        if self.location.read16(REG_STATUS) & STATUS_CAPABILITY_LIST == 0 {
            return None;
        }
        let mut ptr = (self.location.read16(REG_CAPABILITIES) & 0xFC) as u8;
        for _ in 0..48 {
            if ptr == 0 {
                return None;
            }
            let header = self.location.read16(ptr);
            if header as u8 == id {
                return Some(ptr);
            }
            ptr = ((header >> 8) as u8) & 0xFC;
        }
        None
    }

    /// Switches the device to message signaled interrupts: it then writes
    /// `vector` to the local APIC of `apic_id` instead of using an interrupt
    /// pin (whose routing would need the ACPI AML tables). False if the
    /// device has no MSI capability.
    pub fn enable_msi(&self, vector: u8, apic_id: u8) -> bool {
        let Some(cap) = self.capability(CAP_MSI) else {
            return false;
        };
        let control = self.location.read16(cap + 2);
        let is_64bit = control & (1 << 7) != 0;
        // message address: the LAPIC MMIO window with the destination APIC ID
        self.location.write32(cap + 4, 0xFEE0_0000 | (apic_id as u32) << 12);
        if is_64bit {
            self.location.write32(cap + 8, 0);
            self.location.write16(cap + 12, vector as u16); // fixed delivery, edge
        } else {
            self.location.write16(cap + 8, vector as u16);
        }
        // enable, one message (multiple message enable = 0)
        self.location.write16(cap + 2, (control & !(0b111 << 4)) | 1);
        let cmd = self.location.read16(REG_COMMAND);
        self.location.write16(REG_COMMAND, cmd | COMMAND_INTX_DISABLE);
        true
    }

    pub fn command(&self) -> u16 {
        self.location.read16(REG_COMMAND)
    }

    pub fn class_name(&self) -> &'static str {
        match (self.class, self.subclass) {
            (0x01, 0x01) => "IDE controller",
            (0x01, 0x06) => "SATA controller",
            (0x01, 0x08) => "NVMe controller",
            (0x01, _) => "storage controller",
            (0x02, _) => "network controller",
            (0x03, _) => "display controller",
            (0x06, 0x00) => "host bridge",
            (0x06, 0x01) => "ISA bridge",
            (0x06, 0x04) => "PCI bridge",
            (0x06, _) => "bridge",
            (0x0C, 0x03) => "USB controller",
            (0x0C, 0x05) => "SMBus controller",
            _ => "other device",
        }
    }
}

fn probe(location: Location) -> Option<Device> {
    let id = location.read32(REG_VENDOR_DEVICE);
    let vendor_id = id as u16;
    if vendor_id == 0xFFFF {
        return None; // nothing there
    }
    let class = location.read32(REG_CLASS);
    Some(Device {
        location,
        vendor_id,
        device_id: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
        interrupt_line: location.read32(REG_INTERRUPT) as u8,
    })
}

/// Scans every bus/device/function (brute force; fast enough in practice).
pub fn enumerate() -> Vec<Device> {
    let mut devices = Vec::new();
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            let Some(dev) = probe(Location {
                bus,
                device,
                function: 0,
            }) else {
                continue;
            };
            devices.push(dev);
            // Bit 7 of the header type: multi-function device
            let header_type = (dev.location.read32(REG_HEADER_TYPE) >> 16) as u8;
            if header_type & 0x80 != 0 {
                for function in 1..8u8 {
                    if let Some(f) = probe(Location { bus, device, function }) {
                        devices.push(f);
                    }
                }
            }
        }
    }
    devices
}

/// First device with the given class/subclass/programming interface.
pub fn find(class: u8, subclass: u8, prog_if: u8) -> Option<Device> {
    enumerate()
        .into_iter()
        .find(|d| d.class == class && d.subclass == subclass && d.prog_if == prog_if)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn finds_q35_devices() {
        let devices = enumerate();
        // q35 host bridge (Intel 82G33/MCH) at 00:00.0
        let host = devices
            .iter()
            .find(|d| {
                d.location
                    == Location {
                        bus: 0,
                        device: 0,
                        function: 0,
                    }
            })
            .unwrap();
        assert_eq!(host.vendor_id, 0x8086);
        assert_eq!((host.class, host.subclass), (0x06, 0x00));
        // ICH9 AHCI controller
        assert!(
            devices
                .iter()
                .any(|d| d.class == 0x01 && d.subclass == 0x06 && d.prog_if == 0x01)
        );
    }

    #[test_case]
    fn ahci_has_memory_bar5() {
        let ahci = find(0x01, 0x06, 0x01).unwrap();
        let abar = ahci.memory_bar(5).expect("AHCI BAR5 must be a memory BAR");
        assert!(abar != 0 && abar % 0x1000 == 0);
    }
}
