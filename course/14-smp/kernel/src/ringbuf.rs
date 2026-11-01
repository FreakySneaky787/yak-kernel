//! A ring buffer of bytes with a fixed size - the mailbox between an interrupt handler, which puts bytes in, and
//! the rest of the kernel, which takes them out. Fixed size, because there is no heap yet (part II).

pub struct RingBuffer<const N: usize> {
    bytes: [u8; N],
    /// Where the oldest byte is, and how many there are; the next free place is `(head + len) % N`.
    head: usize,
    len: usize,
}

impl<const N: usize> RingBuffer<N> {
    pub const fn new() -> Self {
        RingBuffer {
            bytes: [0; N],
            head: 0,
            len: 0,
        }
    }

    /// Adds `byte` at the end; `false` if the buffer is full - then the byte is dropped.
    pub fn push(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.bytes[(self.head + self.len) % N] = byte;
        self.len += 1;
        true
    }

    /// Takes the oldest byte out.
    pub fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn first_in_first_out_and_full_is_full() {
        let mut buffer: RingBuffer<3> = RingBuffer::new();
        assert!(buffer.push(1) && buffer.push(2) && buffer.push(3));
        assert!(!buffer.push(4)); // full: dropped
        assert_eq!(buffer.pop(), Some(1));
        assert!(buffer.push(5)); // goes into the place of 1: the buffer wraps around
        assert_eq!(
            (buffer.pop(), buffer.pop(), buffer.pop(), buffer.pop()),
            (Some(2), Some(3), Some(5), None)
        );
    }
}
