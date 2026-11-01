# 11 – A first shell

**Goal:** a kernel you can talk to: it prints a prompt, reads a line from the keyboard, and runs what you typed.
**You end up with:** a line editor, a splitter that turns a line into words (with quotes), a table of six commands
(`help`, `echo`, `clear`, `uptime`, `sleep`, `mem`), and a `make test` that types four commands and checks what
the shell answers. This chapter is part III.
**Code:** [`course/11-shell/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/11-shell) (chapter 10
plus `words.rs`, `shell.rs`, and a `kmain` that ends in the shell instead of chapter 7's echo loop)
**What changed:** `diff -ru course/10-heap course/11-shell` (in the repository) shows every line this chapter adds.

## Background: a loop with four steps
A shell is a loop:

```text
print the prompt  ->  read a line  ->  split it into words  ->  run the command the first word names
       ^                                                                       |
       +-----------------------------------------------------------------------+
```

None of the steps is new hardware. Reading keys is chapter 7, sleeping chapter 6, the numbers of `mem` come from
chapters 8 and 10. What is new is that the kernel now keeps things whose size it learns only at run time: a line
as long as you type it, as many words as the line has. Before chapter 10 that needed fixed arrays and limits
everywhere; now it is a `String` and a `Vec<String>`.

One decision runs through the whole chapter: every piece writes to a `core::fmt::Write`, not to the screen. The
shell passes the screen; a test passes a `String` – and can compare exactly what a command printed.

## Step 1: words
`words::split` walks the line once. A space ends a word, unless a quote is open; inside `"…"` or `'…'` a space is
part of the word, and the quotes themselves are not:

```rust,file=kernel/src/words.rs
        match (quote, c) {
            // inside quotes: everything up to the matching quote belongs to the word
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
```

`in_word` tells "no word yet" from "an empty word": two spaces in a row are no word, `''` is one. A quote that is
still open at the end of the line is an error, `UnclosedQuote`, not a guess.

## Step 2: a line editor
`LineEditor::feed` takes one key at a time. A character goes into the line and onto the screen; Enter hands back
the line; Backspace takes the last character out and erases it on the screen with chapter 7's `"\x08 \x08"`:

```rust,file=kernel/src/shell.rs
            Key::Backspace => {
                // at the start of the line there is nothing to erase - and the prompt must stay
                if self.line.pop().is_some() {
                    let _ = out.write_str("\x08 \x08");
                }
            }
```

A line holds at most 72 characters. With the 5 characters of the prompt it fits into a terminal of 80 columns and
never wraps – which matters because `\x08` only moves left inside a row, never up into the row above.

## Step 3: a table of commands
A command is a name, a usage line, a help text and a function. The function gets the words after the name and the
place to write to, and may fail with a message:

```rust,file=kernel/src/shell.rs
type Run = fn(&[String], &mut dyn Write) -> Result<(), String>;
```

All commands are one `static` slice. `help` walks it, and a new command is one more entry. `execute` puts the
steps together – split, take the first word, find it, call it:

```rust,file=kernel/src/shell.rs
    let Some((name, args)) = words.split_first() else {
        return; // an empty line: nothing to do
    };
    match COMMANDS.iter().find(|command| command.name == name) {
        Some(command) => {
            if let Err(message) = (command.run)(args, out) {
                let _ = writeln!(out, "{}: {}", name, message);
            }
        }
```

`sleep` shows the error path: it wants exactly one argument, and a number. A slice pattern checks the first, `parse`
the second:

```rust,file=kernel/src/shell.rs
    let [ms] = args else {
        return Err(String::from("usage: sleep <ms>"));
    };
```

## Step 4: the loop
`run` is the loop from the background, with the screen as its `Write`. `kmain` no longer echoes keys; it starts
the keyboard, prints `Shell: type help and Enter` and calls `shell::run()`, which never returns.

What the reference kernel's shell (`kernel/src/shell/`, about 2100 lines) has besides: input from the serial port
as well (IRQ 4), so the shell works in a terminal; escape sequences for the arrow keys and a cursor that moves
inside the line; history, tab completion and Ctrl keys; backslash escapes in words; and many more commands. This
chapter reads only the PS/2 keyboard – type into the QEMU window, not into the terminal.

## The tests
`make test` now runs 71 tests in the kernel; ten are new:
- `spaces_separate_words`, `quotes_keep_spaces_in_a_word`, `an_unclosed_quote_is_an_error` – the splitter, also
  with empty words (`''`) and a quote inside the other kind (`"it's"`);
- `the_line_editor_echoes_and_erases` – exactly the bytes on the screen, and one Backspace too many does nothing;
- `a_full_line_takes_no_more_keys` – 77 keys, 72 characters;
- `echo_joins_its_words`, `empty_lines_and_unknown_commands`, `help_lists_every_command`;
- `sleep_checks_its_argument_and_sleeps` – three wrong calls and one that waits at least 50 ms;
- `uptime_and_mem_report_numbers`.

The boot check types four lines with QEMU's `sendkey`: `echo Hi  YAK!` (with Shift, Caps Lock and two spaces),
`echo shelx`, Backspace, `l`, then `nope` and `mem` – and wants `Hi YAK!` with one space, `shell`, the unknown
command and the memory numbers.

## Run it
```sh
cd course/11-shell
make run
```
Click into the QEMU window and type. What the boot check typed, as a terminal shows the serial output:

```text
Shell: type help and Enter
yak> echo Hi  YAK!
Hi YAK!
yak> echo shell
shell
yak> nope
unknown command: nope (try help)
yak> mem
frames: 31854 of 31884 free (124 of 124 MiB)
heap: 128 bytes in use, 64 KiB mapped
yak> 
```
The second line was typed as `shelx`, Backspace, `l`. The serial log keeps every byte – `shelx\x08 \x08l` – and a
terminal, like the screen, moves back over the `x`, covers it with a space and moves back again.
And `help`, `uptime` and a wrong `sleep`:

```text
yak> help
  help         list the commands
  echo [words] print the words, one space between them
  clear        clear the screen
  uptime       time since boot
  sleep <ms>   wait for <ms> milliseconds
  mem          free frames and the heap
yak> uptime
up 3.94 s
yak> sleep soon
sleep: not a number: soon
```

## What can go wrong
- **Backspace at the start of the line:** without the `if`, Backspace erases on the screen even when the line is
  empty – and eats the prompt. `echo` and six Backspaces leave `yak` of `yak> ` on the screen. The test says
  `left: "ls\u{8} \u{8}\u{8} \u{8}\u{8} \u{8}hi\n"`, one erase too many.
- **Every space ends a word:** split on each space without `in_word`, and two spaces make an empty word between
  them. `echo  a   'b  c'` then prints ` a   b  c` instead of `a b  c`.
- **No limit on the line:** 77 keys give a line of 77 characters (`left: 77, right: 72`). A line long enough
  wraps – on an 80-column terminal much sooner than on the 160-column screen – and since chapter 3's `put_char`
  moves `\x08` only inside a row, Backspace cannot erase what wrapped.

## Experiments
1. Add a command `add <a> <b>` that prints the sum. What should `add 1` and `add 1 x` say?
2. Add a history: the last ten lines in a `Vec<String>`, and a command `history` that prints them.
3. Let `echo` understand a backslash: `echo a\ b` is the one word `a b`. Where in `split` does it go, and what
   should a backslash at the end of the line do?
4. Read the serial port too: turn on its receive interrupt (IRQ 4), put the bytes into a second ring buffer, and
   let `read_key` take from both. Which byte does a terminal send for Enter, and which for Backspace?

The shell is the place every later part plugs into: a command per new subsystem – for the CPUs, the disk, the
network.
