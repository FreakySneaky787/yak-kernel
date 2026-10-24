//! Intel 8254x ("e1000") Gigabit Ethernet controller, as emulated by QEMU
//! (`-device e1000`, 82540EM, PCI 8086:100E).
//!
//! The card exchanges frames with RAM by DMA through two rings of 16-byte
//! descriptors (Intel 8254x manual, chapter 3):
//! - receive: each descriptor points at a 2 KiB buffer; the card fills
//!   buffers from the head, sets "descriptor done" (DD), and we hand
//!   descriptors back by moving the tail register (RDT);
//! - transmit: we fill a descriptor (buffer, length, "end of packet" +
//!   "report status") and move the tail (TDT); the card sends and sets DD.
//!
//! Polled (QEMU's e1000 has no MSI, and legacy INTx routing would need the
//! ACPI _PRT tables): the network thread calls `receive` regularly.

use super::pci;
use crate::mem::paging::{self, Flags};
use crate::mem::pmm::alloc_frame_zeroed;
use crate::mem::{PhysAddr, VirtAddr};
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

const REG_CTRL: u64 = 0x0000;
const REG_STATUS: u64 = 0x0008;
const REG_EERD: u64 = 0x0014;
const REG_ICR: u64 = 0x00C0;
const REG_IMC: u64 = 0x00D8;
const REG_RCTL: u64 = 0x0100;
const REG_TCTL: u64 = 0x0400;
const REG_TIPG: u64 = 0x0410;
const REG_RDBAL: u64 = 0x2800;
const REG_RDBAH: u64 = 0x2804;
const REG_RDLEN: u64 = 0x2808;
const REG_RDH: u64 = 0x2810;
const REG_RDT: u64 = 0x2818;
const REG_TDBAL: u64 = 0x3800;
const REG_TDBAH: u64 = 0x3804;
const REG_TDLEN: u64 = 0x3808;
const REG_TDH: u64 = 0x3810;
const REG_TDT: u64 = 0x3818;
const REG_MTA: u64 = 0x5200;
const REG_RAL0: u64 = 0x5400;
const REG_RAH0: u64 = 0x5404;

const CTRL_SLU: u32 = 1 << 6; // set link up
const CTRL_RST: u32 = 1 << 26;
const STATUS_LU: u32 = 1 << 1; // link up
const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15; // accept broadcast
const RCTL_SECRC: u32 = 1 << 26; // strip the Ethernet CRC
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3; // pad short packets
const TCMD_EOP: u8 = 1 << 0;
const TCMD_IFCS: u8 = 1 << 1; // insert the CRC
const TCMD_RS: u8 = 1 << 3; // report status (sets DD)
const DESC_DD: u8 = 1 << 0;
const RX_EOP: u8 = 1 << 1;

const RX_COUNT: usize = 32;
const TX_COUNT: usize = 8;
const BUF_SIZE: usize = 2048;

/// Device IDs of 8254x variants that work the same way.
const DEVICE_IDS: &[u16] = &[0x100E, 0x100F, 0x1004];

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDesc {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TxDesc {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

const _: () = assert!(core::mem::size_of::<RxDesc>() == 16 && core::mem::size_of::<TxDesc>() == 16);

pub struct E1000 {
    regs: VirtAddr,
    mac: [u8; 6],
    rx_ring: PhysAddr,
    rx_bufs: [PhysAddr; RX_COUNT],
    rx_next: usize,
    tx_ring: PhysAddr,
    tx_bufs: [PhysAddr; TX_COUNT],
    tx_next: usize,
    pub rx_frames: u64,
    pub tx_frames: u64,
}

#[derive(Debug)]
pub enum NicError {
    NotFound,
    NoBar,
    OutOfMemory,
    TooLong,
}

impl E1000 {
    fn read(&self, reg: u64) -> u32 {
        unsafe { ((self.regs.as_u64() + reg) as *const u32).read_volatile() }
    }

    fn write(&self, reg: u64, value: u32) {
        unsafe { ((self.regs.as_u64() + reg) as *mut u32).write_volatile(value) }
    }

    fn rx_desc(&self, i: usize) -> *mut RxDesc {
        (self.rx_ring.to_virt().as_u64() as *mut RxDesc).wrapping_add(i)
    }

    fn tx_desc(&self, i: usize) -> *mut TxDesc {
        (self.tx_ring.to_virt().as_u64() as *mut TxDesc).wrapping_add(i)
    }

    /// Finds, resets and starts the first e1000 card.
    pub fn probe() -> Result<E1000, NicError> {
        let dev = pci::enumerate()
            .into_iter()
            .find(|d| d.vendor_id == 0x8086 && DEVICE_IDS.contains(&d.device_id))
            .ok_or(NicError::NotFound)?;
        let bar = dev.memory_bar(0).ok_or(NicError::NoBar)?;
        dev.enable_mmio_and_dma();
        let regs = paging::ensure_hhdm_mapped(PhysAddr(bar), 0x20000, Flags::MMIO).map_err(|_| NicError::NoBar)?;

        let alloc = || alloc_frame_zeroed().ok_or(NicError::OutOfMemory);
        let mut nic = E1000 {
            regs,
            mac: [0; 6],
            rx_ring: alloc()?,
            rx_bufs: [PhysAddr(0); RX_COUNT],
            rx_next: 0,
            tx_ring: alloc()?,
            tx_bufs: [PhysAddr(0); TX_COUNT],
            tx_next: 0,
            rx_frames: 0,
            tx_frames: 0,
        };
        // two 2 KiB buffers per frame
        for i in (0..RX_COUNT).step_by(2) {
            let frame = alloc()?;
            nic.rx_bufs[i] = frame;
            nic.rx_bufs[i + 1] = PhysAddr(frame.as_u64() + BUF_SIZE as u64);
        }
        for i in (0..TX_COUNT).step_by(2) {
            let frame = alloc()?;
            nic.tx_bufs[i] = frame;
            nic.tx_bufs[i + 1] = PhysAddr(frame.as_u64() + BUF_SIZE as u64);
        }
        nic.reset_and_start();
        Ok(nic)
    }

    fn reset_and_start(&mut self) {
        self.write(REG_IMC, 0xFFFF_FFFF); // no interrupts (we poll)
        self.write(REG_CTRL, self.read(REG_CTRL) | CTRL_RST);
        for _ in 0..1_000_000 {
            if self.read(REG_CTRL) & CTRL_RST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        self.write(REG_IMC, 0xFFFF_FFFF);
        self.read(REG_ICR); // clear pending causes
        self.write(REG_CTRL, self.read(REG_CTRL) | CTRL_SLU);

        self.mac = self.read_mac();
        for i in 0..128 {
            self.write(REG_MTA + i * 4, 0); // no multicast
        }

        for i in 0..RX_COUNT {
            unsafe {
                self.rx_desc(i).write_volatile(RxDesc {
                    addr: self.rx_bufs[i].as_u64(),
                    length: 0,
                    checksum: 0,
                    status: 0,
                    errors: 0,
                    special: 0,
                })
            };
        }
        self.write(REG_RDBAL, self.rx_ring.as_u64() as u32);
        self.write(REG_RDBAH, (self.rx_ring.as_u64() >> 32) as u32);
        self.write(REG_RDLEN, (RX_COUNT * 16) as u32);
        self.write(REG_RDH, 0);
        self.write(REG_RDT, (RX_COUNT - 1) as u32); // all buffers belong to the card
        self.write(REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC); // BSIZE 00 = 2048 bytes

        for i in 0..TX_COUNT {
            // DD set = "free" for our bookkeeping
            unsafe {
                self.tx_desc(i).write_volatile(TxDesc {
                    addr: self.tx_bufs[i].as_u64(),
                    length: 0,
                    cso: 0,
                    cmd: 0,
                    status: DESC_DD,
                    css: 0,
                    special: 0,
                })
            };
        }
        self.write(REG_TDBAL, self.tx_ring.as_u64() as u32);
        self.write(REG_TDBAH, (self.tx_ring.as_u64() >> 32) as u32);
        self.write(REG_TDLEN, (TX_COUNT * 16) as u32);
        self.write(REG_TDH, 0);
        self.write(REG_TDT, 0);
        // collision threshold 0x10, collision distance 0x40 (full duplex values from the manual)
        self.write(REG_TCTL, TCTL_EN | TCTL_PSP | (0x10 << 4) | (0x40 << 12));
        self.write(REG_TIPG, 0x0060_200A);
    }

    /// MAC address from the receive address registers (QEMU loads them from
    /// the EEPROM at reset), or from the EEPROM directly.
    fn read_mac(&self) -> [u8; 6] {
        let (low, high) = (self.read(REG_RAL0), self.read(REG_RAH0));
        if low != 0 {
            let l = low.to_le_bytes();
            let h = high.to_le_bytes();
            return [l[0], l[1], l[2], l[3], h[0], h[1]];
        }
        let mut mac = [0u8; 6];
        for word in 0..3u32 {
            self.write(REG_EERD, 1 | (word << 8));
            let mut v = self.read(REG_EERD);
            while v & (1 << 4) == 0 {
                v = self.read(REG_EERD);
            }
            let data = (v >> 16) as u16;
            mac[word as usize * 2..word as usize * 2 + 2].copy_from_slice(&data.to_le_bytes());
        }
        mac
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn link_up(&self) -> bool {
        self.read(REG_STATUS) & STATUS_LU != 0
    }

    /// Queues one Ethernet frame (without CRC) for sending.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), NicError> {
        if frame.len() > BUF_SIZE {
            return Err(NicError::TooLong);
        }
        let i = self.tx_next;
        // wait until the card is done with this slot (normally it is long done)
        let desc = self.tx_desc(i);
        let mut spins = 0u64;
        while unsafe { (*desc).status } & DESC_DD == 0 {
            fence(Ordering::Acquire);
            spins += 1;
            if spins > 10_000_000 {
                return Err(NicError::OutOfMemory);
            }
            core::hint::spin_loop();
        }
        let buf = self.tx_bufs[i].to_virt().as_mut_ptr::<u8>();
        unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), buf, frame.len()) };
        unsafe {
            desc.write_volatile(TxDesc {
                addr: self.tx_bufs[i].as_u64(),
                length: frame.len() as u16,
                cso: 0,
                cmd: TCMD_EOP | TCMD_IFCS | TCMD_RS,
                status: 0,
                css: 0,
                special: 0,
            })
        };
        fence(Ordering::Release); // descriptor before the tail update
        self.tx_next = (i + 1) % TX_COUNT;
        self.write(REG_TDT, self.tx_next as u32);
        self.tx_frames += 1;
        Ok(())
    }

    /// The next received frame, if any (without CRC).
    pub fn receive(&mut self) -> Option<Vec<u8>> {
        let i = self.rx_next;
        let desc = unsafe { self.rx_desc(i).read_volatile() };
        if desc.status & DESC_DD == 0 {
            return None;
        }
        fence(Ordering::Acquire); // the buffer after the status
        let len = (desc.length as usize).min(BUF_SIZE);
        let buf = self.rx_bufs[i].to_virt().as_ptr::<u8>();
        let data = unsafe { core::slice::from_raw_parts(buf, len) }.to_vec();
        let complete = desc.status & RX_EOP != 0;
        unsafe {
            self.rx_desc(i).write_volatile(RxDesc {
                addr: self.rx_bufs[i].as_u64(),
                length: 0,
                checksum: 0,
                status: 0,
                errors: 0,
                special: 0,
            })
        };
        fence(Ordering::Release);
        self.write(REG_RDT, i as u32); // give the buffer back
        self.rx_next = (i + 1) % RX_COUNT;
        if !complete {
            return self.receive(); // frames larger than a buffer are not used by us; drop
        }
        self.rx_frames += 1;
        Some(data)
    }
}
