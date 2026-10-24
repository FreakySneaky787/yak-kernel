//! Fixed-size byte ring buffer, used between interrupt handlers (producers)
//! and the rest of the kernel (consumers). No heap needed.

/// Fixed-size ring buffer without heap. When full, new bytes are dropped.
pub struct RingBuffer<const N: usize> {
    buf: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> RingBuffer<N> {
    pub const fn new() -> Self {
        RingBuffer {
            buf: [0; N],
            head: 0,
            len: 0,
        }
    }

    /// Returns `false` if the buffer was full.
    pub fn push(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.buf[(self.head + self.len) % N] = byte;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(byte)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn ring_buffer_fifo_and_overflow() {
        let mut rb: RingBuffer<3> = RingBuffer::new();
        assert!(rb.push(1) && rb.push(2) && rb.push(3));
        assert!(!rb.push(4)); // full
        assert_eq!(rb.pop(), Some(1));
        assert!(rb.push(5)); // wrap-around
        assert_eq!(
            (rb.pop(), rb.pop(), rb.pop(), rb.pop()),
            (Some(2), Some(3), Some(5), None)
        );
    }
}
