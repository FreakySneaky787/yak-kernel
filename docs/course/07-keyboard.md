# 7 – The keyboard

**Goal:** keys typed in the QEMU window appear on the screen. The keyboard raises IRQ 1, a handler stores the byte,
and the kernel decodes it outside the handler.
**You end up with:** a kernel that echoes what you type, with Shift, Caps Lock, Enter and Backspace. `make test`
types "Hi YAK!" on the emulated keyboard and checks that it arrives. This ends part I.
**Code:** [`course/07-keyboard/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/07-keyboard) (chapter
6 plus `ringbuf.rs`, `scancode.rs`, `keyboard.rs`, one more IRQ handler, Backspace in the console, and typing in
`run-qemu.sh`)
**What changed:** `diff -ru course/06-timer course/07-keyboard` (in the repository) shows every line this chapter adds.

## Background: scancodes
Behind the keyboard sits the **PS/2 controller**, an old chip that QEMU emulates like the PIC. On every key event
it puts one byte at I/O port `0x60` and raises IRQ 1. The byte is a **scancode**. The firmware sets the controller
up to translate everything into *set 1*, the oldest code set:

- **Press and release:** pressing a key sends its code (the *make*). Releasing it sends the same code with bit 7 set
  (the *break*). The H key sends `0x23` and then `0xA3`. Holding a key repeats the make.
- **A code names a position, not a letter.** `0x15` is the key right of T. That key is a Y on a US keyboard and a Z
  on a German one. What a position means is the **layout**, and the kernel has to know it. Ours is US. QEMU's
  `sendkey` names keys by their US position too.
- **Shift has no letters of its own.** Shift, Ctrl and Caps Lock send codes like any other key. Whether H gives `h`
  or `H` is something the kernel works out, by remembering which modifier keys are down.
- **Prefixes.** Keys that came after the first PC keyboards (the arrows, right Ctrl, …) send `0xE0` and then one
  byte. Without the prefix, the up arrow (`E0 48`) would look like the 8 of the number pad.

## Step 1: a ring buffer
The handler must be quick, so it only stores the byte; the rest of the kernel takes it out later. In between sits a
**ring buffer**: a fixed array with a read position and a length. A fixed size is the only kind we can have, because
there is no heap yet (that comes in part II). When the buffer is full, a new byte is dropped. An interrupt handler
cannot wait for somebody to make room.

```rust,file=kernel/src/ringbuf.rs
    /// Adds `byte` at the end; `false` if the buffer is full - then the byte is dropped.
    pub fn push(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.bytes[(self.head + self.len) % N] = byte;
        self.len += 1;
        true
    }
```

The `% N` is the "ring": past the last place, the buffer goes on at the first one.

## Step 2: the handler
The handler on vector 33 reads the byte and answers the PIC, exactly like the timer:

```rust,file=kernel/src/keyboard.rs
pub fn handle_interrupt() {
    let byte = unsafe { inb(DATA_PORT) };
    SCANCODES.lock().push(byte);
}
```

```rust,file=kernel/src/interrupts.rs
extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    keyboard::handle_interrupt();
    pic::end_of_interrupt(KEYBOARD_IRQ);
}
```

The `inb` is not optional. Until the byte is read, the controller keeps it and sends nothing new.
The same holds at the start. A byte that waits from before, such as a key pressed during boot, has to go too
before IRQ 1 is unmasked. Otherwise nobody reads it and the keyboard stays silent. `keyboard::init` empties the
controller, and `interrupts::start_keyboard` then opens the line:

```rust,file=kernel/src/interrupts.rs
pub fn start_keyboard() {
    keyboard::init();
    pic::unmask(KEYBOARD_IRQ);
}
```

In `interrupts::init` the gate joins the others:
`idt.set(pic::vector(KEYBOARD_IRQ), keyboard);`.

## Step 3: from bytes to keys
`scancode.rs` has no hardware in it at all. A `Decoder` takes one byte at a time and sometimes gives a key back. It
remembers three things: both Shift keys and Caps Lock. A fourth flag says whether the last byte was a prefix, in
which case the next byte is skipped:

```rust,file=kernel/src/scancode.rs
        let pressed = byte & RELEASED == 0;
        match byte & !RELEASED {
            LEFT_SHIFT => self.left_shift = pressed,
            RIGHT_SHIFT => self.right_shift = pressed,
            CAPS_LOCK if pressed => self.caps_lock = !self.caps_lock,
            _ if !pressed => {}
            ENTER => return Some(Key::Enter),
            BACKSPACE => return Some(Key::Backspace),
            code => return self.character(code).map(Key::Char),
        }
```

The layout is a table. The codes of one keyboard row follow each other (Q is `0x10`, W `0x11`, …), so the table
holds each row as a string, once without and once with Shift. It looks like the keyboard itself:

```rust,file=kernel/src/scancode.rs
const ROWS: [(u8, &str, &str); 5] = [
    (0x02, "1234567890-=", "!@#$%^&*()_+"),
    (0x10, "qwertyuiop[]", "QWERTYUIOP{}"),
    (0x1E, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
    (0x2B, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
    (0x39, " ", " "),
];
```

Caps Lock has one subtlety. It makes letters capitals and leaves digits alone, so Caps Lock and 1 still gives `1`.
Caps Lock together with Shift gives small letters again:

```rust,file=kernel/src/scancode.rs
        let upper = if normal.as_bytes()[i].is_ascii_alphabetic() {
            shift != self.caps_lock
        } else {
            shift
        };
```

## Step 4: waiting for a key
`read_key` takes bytes out of the buffer until one gives a key. When the buffer is empty, it sleeps in `hlt` like
`sleep_ms`. Two details make this harder than it looks:

```rust,file=kernel/src/keyboard.rs
        unsafe { asm!("cli", options(nomem, nostack)) };
        let byte = SCANCODES.lock().pop(); // a statement of its own: the lock is free again before `hlt`
        match byte {
            // `sti` lets interrupts in only after the next instruction - nothing can slip in before `hlt`
            None => unsafe { asm!("sti", "hlt", options(nomem, nostack)) },
```

- **The lock.** The handler and `read_key` share the buffer's spin lock. Suppose the handler interrupts `read_key`
  while `read_key` holds the lock. The handler then waits for a lock that will never be released, because its
  holder cannot run until the handler is done. So `read_key` takes the lock only with interrupts off (`cli`).
  It also lets go of the lock *before* `hlt`. A temporary in a `match` lives until the end of the `match`, so
  `match SCANCODES.lock().pop() { … }` would still hold the lock while the CPU sleeps. That is why the `pop` is a
  statement of its own.
- **The lost wake-up.** "Buffer empty? Then `hlt`" has a gap between the check and the `hlt`. A key that arrives
  inside the gap lands in the buffer, but the CPU goes to sleep after it, and only the *next* interrupt wakes it.
  Our timer ticks every 10 ms, so here the gap costs at most 10 ms. A kernel without a steady tick would sit on the
  key until the next one. The fix costs nothing: interrupts stay off from the check to the `hlt`. `sti` enables
  them only after the *following* instruction, so `sti; hlt` leaves no gap.

## Step 5: echo
`kmain` starts the keyboard after the timer and echoes what comes in. The test `kmain` calls
`interrupts::start_keyboard()` too, after `start_timer()`:

```rust,file=kernel/src/main.rs
    loop {
        match keyboard::read_key() {
            Key::Char(c) => print!("{}", c),
            Key::Enter => println!(),
            Key::Backspace => print!("\x08 \x08"),
        }
    }
```

`"\x08 \x08"` is how terminals erase: back one cell, a space over the character, back again. The serial terminal
already understands it. The console learns `\x08` (one cell back) in `put_char`:

```rust,file=kernel/src/console.rs
            '\x08' => self.column = self.column.saturating_sub(1),
```

`kmain` no longer ends in `halt()`; it waits for keys forever.

## Step 6: typing in the check
A test has nobody to press keys, so `run-qemu.sh` types itself. QEMU has a **monitor**, a command line for the
emulator, and its command `sendkey shift-h` presses Shift+H on the emulated keyboard. In check mode the monitor
reads its commands from a FIFO, a named pipe in `build/`. Once the serial output contains the line in
`KEYS_AFTER`, the script writes one `sendkey` per key in `KEYS`:

```bash,file=run-qemu.sh
    type_keys() {
        for key in $KEYS; do
            echo "sendkey $key" >&3
            sleep 0.2 # sendkey holds a key down for 100 ms
        done
    }
```

The Makefile names what to type: "Hi YAK!" with Shift and Caps Lock, then Enter.

```makefile,file=Makefile
TYPED := shift-h i spc caps_lock y a k caps_lock shift-1 ret
```

The kernel has to print that line back, so `Hi YAK!` is the last line of `expected.txt`. Before
`type something` appears, the keyboard is not ready yet; that is what `KEYS_AFTER` waits for.

## The tests
`make test` now runs 36 tests in the kernel. Seven are new:
- `first_in_first_out_and_full_is_full`: the ring buffer keeps the order, drops when full, and wraps around;
- `a_key_types_when_pressed_not_when_released`: letters, digits, space, Enter, Backspace, and nothing on release;
- `shift_counts_while_it_is_held`: `H` and `!` while Shift is down, `h` again after its break, and the right Shift
  works too;
- `caps_lock_toggles_letters_only`: `A` but `1`; Caps Lock with Shift gives `a`; a second Caps Lock turns it off;
- `prefixed_keys_are_skipped`: a "fake shift" (`E0 2A`, sent with some keys) makes no capitals, the up arrow
  gives nothing, and neither do the six bytes of Pause;
- `bytes_in_the_buffer_become_keys`: bytes put into the real buffer come out of `read_key` as `H`, `i`, Enter;
- `backspace_space_backspace_erases`: `"ab\x08 \x08c"` leaves `ac` on the screen.

Two older tests change. The PIC masks are now `0b1111_1100` (`only_the_timer_and_the_keyboard_are_let_through`),
and the IDT test expects gate 33 too. The boot checks follow: the normal one types and waits for `Hi YAK!`, and the
two fault checks stay as in chapter 5.

## Run it
```sh
cd course/07-keyboard
make run
```
After the timer line, the kernel waits:

```text
Keyboard: IRQ 1 on vector 33 - type something (US layout)
```
Click into the QEMU window and type. The characters appear on the screen and in the terminal. On a German keyboard
Y and Z are swapped, and `ü` gives `[`. QEMU passes on the key's position, and our table reads positions the US way.
Experiment 1 fixes that.

## What can go wrong
- **Nothing appears when you type:** the QEMU window does not have the keyboard (click into it), or one of the
  things the timer chapter lists: IRQ 1 masked, or interrupts off.
- **The first key arrives, then nothing:** either the handler does not read port `0x60`, so the controller keeps
  the byte and sends no new one, or it does not send the EOI, so the PIC holds back the next IRQ 1. Both look the
  same. Without the `inb` the check shows one `h`; without the EOI, typing `h i` shows one `h`. The check's own
  first key is Shift, so there it shows nothing at all.
- **Everything stops at the first key, the timer too:** the buffer's lock was held when the interrupt came. A
  `match SCANCODES.lock().pop()` in `read_key` does exactly this, because the lock is still taken during `hlt`. The
  handler then spins for the lock with interrupts off, forever.
- **Capitals after Print Screen or Home:** some keys send a "fake shift" (`E0 2A`). Without the prefix rule the
  `2A` would count as the left Shift. `prefixed_keys_are_skipped` fails if the rule is missing.

## Experiments
1. Write a German (QWERTZ) layout: `z` and `y` swap, and `ü`, `ö`, `ä`, `ß` come in. Those are no longer one byte
   each, so `as_bytes()[i]` will not do. What does `chars().nth(i)` cost instead?
2. Print each scancode in hex instead of decoding it (`print!("{:02x} ", byte)` in `read_key`). Press and release
   a key, hold a key, press an arrow, press Pause.
3. Make the ring buffer 4 bytes long and type `KEYS="a b c d e f g h"` into a kernel that sleeps a second before
   every `read_key`. Which keys arrive?
4. Remember Ctrl as well (`0x1D`, and `E0 1D` for the right one, which the prefix rule skips for now) and make
   Ctrl+L clear the screen.

Part I ends here. The kernel boots, tests itself, writes to the screen, survives its own crashes, keeps time and
reads the keyboard. All of it runs without a single allocation. Part II gives it memory: a frame allocator, its own
page tables, and a heap.
