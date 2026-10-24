//! The key types the line editor works with. The line editor source is also
//! compiled into `/bin/sh` (user space), which provides its own `keys` module
//! with the same definitions.

pub use crate::drivers::scancode::{Key, KeyEvent};
