//! Pipes: a bounded byte queue between a writing and a reading end.
//!
//! Each end is a `PipeEnd` inside a file descriptor; cloning a descriptor
//! (`dup`, `spawn`) clones the end and bumps the reader/writer count,
//! dropping it (close, process exit) lowers it again. That gives the usual
//! semantics:
//! - read from an empty pipe blocks while writers exist, returns 0 (end of
//!   file) once the last writer is gone;
//! - write to a full pipe blocks while readers exist; without readers it
//!   fails with EPIPE.
//!
//! The pipe lock is only ever taken with interrupts disabled, so a holder
//! cannot be preempted (descriptors are dropped with the process table
//! locked, interrupts off). Blocked threads wait on `Wait::Pipe(id)` and are
//! notified on every change; they also wake up for a `kill`.

use crate::arch::x86_64::without_interrupts;
use crate::task::{self, Wait};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// Bytes a pipe holds before writers block.
pub const PIPE_CAPACITY: usize = 64 * 1024;

pub struct Pipe {
    id: u64,
    inner: Mutex<Inner>,
}

struct Inner {
    buf: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PipeError {
    /// No reader left (EPIPE).
    Broken,
    /// The caller is being killed (EINTR).
    Interrupted,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One end of a pipe, owned by a file descriptor.
pub struct PipeEnd {
    pipe: Arc<Pipe>,
    pub write: bool,
}

/// Creates a pipe and returns its (read end, write end).
pub fn pipe() -> (PipeEnd, PipeEnd) {
    let pipe = Arc::new(Pipe {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        inner: Mutex::new(Inner {
            buf: VecDeque::new(),
            readers: 1,
            writers: 1,
        }),
    });
    (
        PipeEnd {
            pipe: pipe.clone(),
            write: false,
        },
        PipeEnd { pipe, write: true },
    )
}

impl Clone for PipeEnd {
    fn clone(&self) -> PipeEnd {
        without_interrupts(|| {
            let mut inner = self.pipe.inner.lock();
            if self.write {
                inner.writers += 1;
            } else {
                inner.readers += 1;
            }
        });
        PipeEnd {
            pipe: self.pipe.clone(),
            write: self.write,
        }
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        without_interrupts(|| {
            let mut inner = self.pipe.inner.lock();
            if self.write {
                inner.writers -= 1;
            } else {
                inner.readers -= 1;
            }
        });
        // a reader may now see end of file, a writer a broken pipe
        task::notify(Wait::Pipe(self.pipe.id));
    }
}

impl PipeEnd {
    /// Reads up to `max` bytes, blocking while the pipe is empty and has
    /// writers. An empty result means end of file. `abort` is checked before
    /// blocking (a pending kill).
    pub fn read(&self, max: usize, abort: impl Fn() -> bool) -> Result<Vec<u8>, PipeError> {
        loop {
            let result = without_interrupts(|| {
                let mut inner = self.pipe.inner.lock();
                if !inner.buf.is_empty() || inner.writers == 0 {
                    let n = inner.buf.len().min(max);
                    Some(inner.buf.drain(..n).collect())
                } else {
                    None
                }
            });
            if let Some(data) = result {
                task::notify(Wait::Pipe(self.pipe.id)); // room for writers
                return Ok(data);
            }
            if abort() {
                return Err(PipeError::Interrupted);
            }
            task::wait_for(Wait::Pipe(self.pipe.id), || self.readable() || abort());
        }
    }

    /// Data to read, or no writer left (end of file).
    fn readable(&self) -> bool {
        without_interrupts(|| {
            let inner = self.pipe.inner.lock();
            !inner.buf.is_empty() || inner.writers == 0
        })
    }

    /// Room to write, or no reader left (broken pipe).
    fn writable(&self) -> bool {
        without_interrupts(|| {
            let inner = self.pipe.inner.lock();
            inner.buf.len() < PIPE_CAPACITY || inner.readers == 0
        })
    }

    /// Writes all of `data`, blocking while the pipe is full. Returns the
    /// number of bytes written (less than `data.len()` only if interrupted
    /// after a partial write).
    pub fn write(&self, data: &[u8], abort: impl Fn() -> bool) -> Result<usize, PipeError> {
        let mut done = 0;
        while done < data.len() {
            let (written, broken) = without_interrupts(|| {
                let mut inner = self.pipe.inner.lock();
                if inner.readers == 0 {
                    (0, true)
                } else {
                    let n = (PIPE_CAPACITY - inner.buf.len()).min(data.len() - done);
                    inner.buf.extend(&data[done..done + n]);
                    (n, false)
                }
            });
            if broken {
                return Err(PipeError::Broken);
            }
            if written > 0 {
                done += written;
                task::notify(Wait::Pipe(self.pipe.id)); // data for readers
                continue;
            }
            if abort() {
                return if done > 0 {
                    Ok(done)
                } else {
                    Err(PipeError::Interrupted)
                };
            }
            task::wait_for(Wait::Pipe(self.pipe.id), || self.writable() || abort());
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn never() -> bool {
        false
    }

    #[test_case]
    fn data_flows_and_eof_follows() {
        let (r, w) = pipe();
        assert_eq!(w.write(b"hello ", never), Ok(6));
        assert_eq!(w.write(b"pipe", never), Ok(4));
        assert_eq!(r.read(3, never).unwrap(), b"hel");
        assert_eq!(r.read(100, never).unwrap(), b"lo pipe");
        let w2 = w.clone();
        drop(w);
        assert_eq!(w2.write(b"!", never), Ok(1), "a clone keeps the pipe open");
        drop(w2);
        assert_eq!(r.read(100, never).unwrap(), b"!");
        assert!(r.read(100, never).unwrap().is_empty(), "no writers left: end of file");
    }

    #[test_case]
    fn writing_without_readers_fails() {
        let (r, w) = pipe();
        drop(r);
        assert_eq!(w.write(b"x", never), Err(PipeError::Broken));
    }

    #[test_case]
    fn full_pipe_blocks_the_writer_until_read() {
        let (r, w) = pipe();
        let total = 3 * PIPE_CAPACITY + 123;
        let writer = task::spawn("pipe-writer", move || {
            let data: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
            assert_eq!(w.write(&data, never), Ok(total));
        }); // `w` is dropped when the writer thread ends -> EOF
        let mut got = Vec::new();
        loop {
            let chunk = r.read(10_000, never).unwrap();
            if chunk.is_empty() {
                break;
            }
            got.extend_from_slice(&chunk);
        }
        task::join(writer);
        assert_eq!(got.len(), total);
        assert!(got.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
    }

    #[test_case]
    fn blocked_reader_can_be_aborted() {
        let (r, _w) = pipe();
        assert_eq!(r.read(10, || true), Err(PipeError::Interrupted));
        let big = vec![0u8; PIPE_CAPACITY + 1];
        let (_r2, w2) = pipe();
        assert_eq!(
            w2.write(&big, || true),
            Ok(PIPE_CAPACITY),
            "partial write, then aborted"
        );
    }
}
