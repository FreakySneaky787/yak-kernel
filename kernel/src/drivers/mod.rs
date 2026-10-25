pub mod ahci;
pub mod block;
pub mod e1000;
pub mod keyboard;
pub mod pci;
pub mod rtc;
pub mod scancode;

use crate::sync::Mutex;
use alloc::vec::Vec;

/// The system disk (first SATA disk), if one was found at boot.
///
/// A sleeping lock (`sync::Mutex`): it is held during disk I/O, which blocks
/// on the completion interrupt; other threads that need the disk sleep
/// meanwhile. Never take it from an interrupt handler.
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
