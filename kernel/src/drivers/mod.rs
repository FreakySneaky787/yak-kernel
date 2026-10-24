pub mod ahci;
pub mod block;
pub mod keyboard;
pub mod pci;
pub mod rtc;
pub mod scancode;

use alloc::vec::Vec;
use spin::Mutex;

/// The system disk (first SATA disk), if one was found at boot.
///
/// The lock is held during disk I/O with interrupts **enabled** (commands are
/// polled and may take a while; the timer must keep running). Never take it
/// from an interrupt handler.
pub static DISK: Mutex<Option<ahci::AhciDisk>> = Mutex::new(None);

/// AHCI ports found at boot (for diagnostics).
pub static STORAGE_PORTS: Mutex<Vec<ahci::PortInfo>> = Mutex::new(Vec::new());

/// Probes storage controllers. Missing disks are not fatal.
pub fn init_storage() {
    match ahci::probe() {
        Ok((mut disks, ports)) => {
            *STORAGE_PORTS.lock() = ports;
            if !disks.is_empty() {
                *DISK.lock() = Some(disks.remove(0));
            }
        }
        Err(e) => crate::serial_println!("storage: {}", e),
    }
}
