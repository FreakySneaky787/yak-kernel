//! The kernel shell: reads lines with the line editor, splits them into
//! words and runs built-in commands.
//!
//! Commands write to a `&mut dyn fmt::Write`, so unit tests can run them
//! against a `String`; the interactive shell uses `Stdout` (console + serial).
//!
//! A word that is not a built-in command is looked up as a program: a path
//! (`./prog`, `/bin/hello`) or a name in `/bin`. Programs run as user
//! processes in the shell's current directory and write directly to the
//! console (`ls`, `cat`, `cp` are such programs since M17).

pub mod keys;
pub mod line_editor;
pub mod parser;

use crate::arch::x86_64::{cpu, power};
use crate::drivers::keyboard;
use crate::drivers::scancode::Layout;
use crate::fs::{self, fat32::FsError};
use crate::mem::{heap, pmm};
use crate::process::{self, ExitStatus};
use crate::{input, task, time};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use line_editor::{EditResult, LineEditor};
use spin::Mutex;

/// Prompt: "kernel:<current directory>> " (with colours).
fn prompt(cwd: &str) -> String {
    format!("\x1b[92mkernel\x1b[0m:\x1b[94m{}\x1b[0m> ", cwd)
}

/// Writes to the screen and the serial port.
pub struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        crate::console::_print(format_args!("{}", s));
        Ok(())
    }
}

type CommandFn = fn(&mut Shell, &[String], &mut dyn Write) -> Result<(), String>;

pub struct Command {
    pub name: &'static str,
    pub usage: &'static str,
    pub help: &'static str,
    run: CommandFn,
}

pub static COMMANDS: &[Command] = &[
    Command {
        name: "help",
        usage: "help [command]",
        help: "list commands or show help for one",
        run: cmd_help,
    },
    Command {
        name: "echo",
        usage: "echo [words...]",
        help: "print the arguments",
        run: cmd_echo,
    },
    Command {
        name: "clear",
        usage: "clear",
        help: "clear the screen",
        run: cmd_clear,
    },
    Command {
        name: "mem",
        usage: "mem",
        help: "physical memory and heap usage",
        run: cmd_mem,
    },
    Command {
        name: "memmap",
        usage: "memmap",
        help: "memory map reported by the bootloader",
        run: cmd_memmap,
    },
    Command {
        name: "uptime",
        usage: "uptime",
        help: "time since boot",
        run: cmd_uptime,
    },
    Command {
        name: "sleep",
        usage: "sleep <ms>",
        help: "wait for the given number of milliseconds",
        run: cmd_sleep,
    },
    Command {
        name: "cpu",
        usage: "cpu",
        help: "processor vendor, model and features",
        run: cmd_cpu,
    },
    Command {
        name: "acpi",
        usage: "acpi",
        help: "ACPI tables, CPUs and interrupt routing",
        run: cmd_acpi,
    },
    Command {
        name: "lspci",
        usage: "lspci",
        help: "list PCI devices",
        run: cmd_lspci,
    },
    Command {
        name: "disk",
        usage: "disk",
        help: "storage controller and disk information",
        run: cmd_disk,
    },
    Command {
        name: "hexdump",
        usage: "hexdump <sector> [count]",
        help: "show raw disk sectors",
        run: cmd_hexdump,
    },
    Command {
        name: "layout",
        usage: "layout [de|us]",
        help: "show or set the keyboard layout",
        run: cmd_layout,
    },
    Command {
        name: "history",
        usage: "history",
        help: "previously entered commands",
        run: cmd_history,
    },
    Command {
        name: "ps",
        usage: "ps",
        help: "list kernel threads",
        run: cmd_ps,
    },
    Command {
        name: "demo",
        usage: "demo [threads]",
        help: "run worker threads in parallel (multitasking demo)",
        run: cmd_demo,
    },
    Command {
        name: "run",
        usage: "run <program> [args...]",
        help: "run a user program (also: just type its name if it is in /bin)",
        run: cmd_run,
    },
    Command {
        name: "netstat",
        usage: "netstat",
        help: "TCP connections and listening ports",
        run: cmd_netstat,
    },
    Command {
        name: "colors",
        usage: "colors",
        help: "show the console colour palette",
        run: cmd_colors,
    },
    Command {
        name: "cd",
        usage: "cd [path]",
        help: "change the current directory",
        run: cmd_cd,
    },
    Command {
        name: "pwd",
        usage: "pwd",
        help: "print the current directory",
        run: cmd_pwd,
    },
    Command {
        name: "write",
        usage: "write <file> <text...>",
        help: "replace a file's contents with a line of text",
        run: cmd_write,
    },
    Command {
        name: "append",
        usage: "append <file> <text...>",
        help: "add a line of text to a file",
        run: cmd_append,
    },
    Command {
        name: "touch",
        usage: "touch <file>",
        help: "create an empty file if it does not exist",
        run: cmd_touch,
    },
    Command {
        name: "mkdir",
        usage: "mkdir <dir>",
        help: "create a directory",
        run: cmd_mkdir,
    },
    Command {
        name: "rm",
        usage: "rm <path>",
        help: "delete a file or an empty directory",
        run: cmd_rm,
    },
    Command {
        name: "mv",
        usage: "mv <from> <to>",
        help: "rename or move a file or directory",
        run: cmd_mv,
    },
    Command {
        name: "stat",
        usage: "stat <path>",
        help: "details about a file or directory",
        run: cmd_stat,
    },
    Command {
        name: "df",
        usage: "df",
        help: "file system usage",
        run: cmd_df,
    },
    Command {
        name: "date",
        usage: "date",
        help: "current date and time (CMOS clock)",
        run: cmd_date,
    },
    Command {
        name: "version",
        usage: "version",
        help: "kernel version",
        run: cmd_version,
    },
    Command {
        name: "int3",
        usage: "int3",
        help: "trigger a breakpoint exception (harmless)",
        run: cmd_int3,
    },
    Command {
        name: "panic",
        usage: "panic [message]",
        help: "trigger a kernel panic",
        run: cmd_panic,
    },
    Command {
        name: "reboot",
        usage: "reboot",
        help: "restart the machine",
        run: cmd_reboot,
    },
    Command {
        name: "poweroff",
        usage: "poweroff",
        help: "switch the machine off",
        run: cmd_poweroff,
    },
];

pub fn find_command(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// Command and `/bin` program names starting with `prefix` (for tab completion).
pub fn complete_command(prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .map(|c| String::from(c.name))
        .collect();
    let programs = fs::with_root(|fs| fs.read_dir(BIN_DIR)).unwrap_or_default();
    for entry in programs.iter().filter(|e| !e.is_dir()) {
        if entry.name.starts_with(prefix) && !names.contains(&entry.name) {
            names.push(entry.name.clone());
        }
    }
    names.sort();
    names
}

/// Where the shell looks for programs typed by name.
pub const BIN_DIR: &str = "/bin";

/// Built-ins not offered to user space through `kshell`: the ones that use
/// the kernel shell's own state (`/bin/sh` has its own versions) and the ones
/// that exist as programs in /bin since M22 (built on real system calls).
pub const KSHELL_EXCLUDED: &[&str] = &[
    "cd", "pwd", "history", "run", // shell state
    "mem", "uptime", "ps", "df", "date", "mkdir", "rm", "touch", "stat", "mv", // /bin programs
];

/// Names of the built-ins available through `kshell`.
pub fn kshell_commands() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .map(|c| c.name)
        .filter(|n| !KSHELL_EXCLUDED.contains(n))
        .collect()
}

pub struct Shell {
    editor: LineEditor,
    /// Current directory (absolute, normalized).
    cwd: String,
    /// Collect program output and pass it to the command's `out` (tests)
    /// instead of letting programs write to the console.
    capture_programs: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

impl Shell {
    pub fn new() -> Shell {
        Shell {
            editor: LineEditor::new(),
            cwd: String::from("/"),
            capture_programs: false,
        }
    }

    /// Runs `line` only if it is a built-in available to user programs (see
    /// `KSHELL_EXCLUDED`). `None` = not such a built-in; otherwise success.
    pub fn execute_builtin(&mut self, line: &str, out: &mut dyn Write) -> Option<bool> {
        let words = parser::split(line).ok()?;
        let command = find_command(words.first()?).filter(|c| !KSHELL_EXCLUDED.contains(&c.name))?;
        Some(match (command.run)(self, &words[1..], out) {
            Ok(()) => true,
            Err(e) => {
                writeln!(out, "\x1b[91merror:\x1b[0m {}", e).ok();
                false
            }
        })
    }

    /// Sets the current directory (absolute, normalized).
    pub fn set_cwd(&mut self, cwd: &str) {
        self.cwd = String::from(cwd);
    }

    /// Parses and runs one command line. Returns `true` on success.
    pub fn execute(&mut self, line: &str, out: &mut dyn Write) -> bool {
        let words = match parser::split(line) {
            Ok(words) => words,
            Err(e) => {
                writeln!(out, "\x1b[91merror:\x1b[0m {}", e).ok();
                return false;
            }
        };
        let Some(name) = words.first() else {
            return true;
        };
        let result = match find_command(name) {
            Some(command) => (command.run)(self, &words[1..], out),
            None => match self.find_program(name) {
                Some(path) => self.run_program(&path, &words[1..], out),
                None => Err(format!("unknown command '{}' (try 'help')", name)),
            },
        };
        match result {
            Ok(()) => true,
            Err(e) => {
                writeln!(out, "\x1b[91merror:\x1b[0m {}", e).ok();
                false
            }
        }
    }

    /// The interactive loop: never returns.
    pub fn run(&mut self) -> ! {
        let mut out = Stdout;
        loop {
            let prompt = prompt(&self.cwd);
            self.editor.start(&prompt, &mut out);
            loop {
                let event = input::read_key();
                match self.editor.handle(event, &mut out, &complete_command) {
                    EditResult::Pending => {}
                    EditResult::Cancelled => break,
                    EditResult::Line(line) => {
                        self.execute(&line, &mut out);
                        break;
                    }
                }
            }
        }
    }
}

// ---- Commands -----------------------------------------------------------------

fn cmd_help(_: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    if let Some(name) = args.first() {
        let c = find_command(name).ok_or_else(|| format!("no such command: {}", name))?;
        writeln!(out, "usage: {}\n  {}", c.usage, c.help).ok();
        return Ok(());
    }
    writeln!(out, "Available commands:").ok();
    for c in COMMANDS {
        writeln!(out, "  \x1b[96m{:<18}\x1b[0m {}", c.usage, c.help).ok();
    }
    writeln!(
        out,
        "Keys: arrows, Home/End, Tab completes, Up/Down history, Ctrl+C cancel, Ctrl+L clear"
    )
    .ok();
    Ok(())
}

fn cmd_echo(_: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    writeln!(out, "{}", args.join(" ")).ok();
    Ok(())
}

fn cmd_clear(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    out.write_str("\x1b[2J\x1b[H").ok();
    Ok(())
}

fn cmd_mem(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    const MIB: u64 = 1024 * 1024;
    let p = pmm::stats();
    let used = p.usable_bytes() - p.free_bytes();
    writeln!(
        out,
        "Physical: {} MiB usable, {} MiB used, {} MiB free ({} frames of 4 KiB free)",
        p.usable_bytes() / MIB,
        used / MIB,
        p.free_bytes() / MIB,
        p.free_frames
    )
    .ok();
    let h = heap::stats();
    writeln!(
        out,
        "Heap:     {} KiB mapped, {} bytes used, {} free blocks (largest {} KiB)",
        h.total / 1024,
        h.used,
        h.free_blocks,
        h.largest_free_block / 1024
    )
    .ok();
    Ok(())
}

fn memmap_type_name(t: u64) -> &'static str {
    use limine::memmap::*;
    match t {
        MEMMAP_USABLE => "usable",
        MEMMAP_RESERVED => "reserved",
        MEMMAP_ACPI_RECLAIMABLE => "ACPI reclaimable",
        MEMMAP_ACPI_NVS => "ACPI NVS",
        MEMMAP_BAD_MEMORY => "bad memory",
        MEMMAP_BOOTLOADER_RECLAIMABLE => "bootloader reclaimable",
        MEMMAP_EXECUTABLE_AND_MODULES => "kernel and modules",
        MEMMAP_FRAMEBUFFER => "framebuffer",
        MEMMAP_MAPPED_RESERVED => "mapped reserved",
        _ => "unknown",
    }
}

fn cmd_memmap(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    // a copy: the bootloader's own map lives in memory that is reclaimed after boot
    for (base, length, type_) in pmm::boot_memmap() {
        writeln!(
            out,
            "  {:#014x} - {:#014x}  {:>9} KiB  {}",
            base,
            base + length,
            length / 1024,
            memmap_type_name(type_)
        )
        .ok();
    }
    Ok(())
}

fn cmd_uptime(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    let ms = time::uptime_ms();
    let (h, m, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    writeln!(
        out,
        "up {:02}:{:02}:{:02}.{:03} ({} ticks at {} Hz, TSC {} MHz)",
        h,
        m,
        s,
        ms % 1000,
        time::ticks(),
        time::TIMER_HZ,
        crate::arch::x86_64::tsc::hz() / 1_000_000
    )
    .ok();
    Ok(())
}

fn cmd_sleep(_: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let ms: u64 = args
        .first()
        .ok_or("usage: sleep <ms>")?
        .parse()
        .map_err(|_| "not a number")?;
    time::sleep_ms(ms);
    writeln!(out, "slept {} ms", ms).ok();
    Ok(())
}

fn cmd_cpu(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    writeln!(out, "Vendor: {}", cpu::vendor()).ok();
    if let Some(brand) = cpu::brand() {
        writeln!(out, "Model:  {}", brand).ok();
    }
    let f = cpu::features();
    let flags = [
        ("sse", f.sse),
        ("sse2", f.sse2),
        ("apic", f.apic),
        ("x2apic", f.x2apic),
        ("nx", f.nx),
        ("lm", f.long_mode),
        ("hypervisor", f.hypervisor),
    ];
    write!(out, "Flags: ").ok();
    for (name, on) in flags {
        if on {
            write!(out, " {}", name).ok();
        }
    }
    writeln!(out).ok();
    let ids = crate::arch::x86_64::smp::online_apic_ids();
    write!(out, "CPUs:   {} online (APIC IDs", ids.len()).ok();
    for id in ids {
        write!(out, " {}", id).ok();
    }
    writeln!(out, ")").ok();
    Ok(())
}

fn cmd_acpi(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    let info = crate::acpi::info().ok_or("ACPI was not initialized")?;
    writeln!(out, "ACPI revision {} (OEM '{}')", info.revision, info.oem_id).ok();
    write!(out, "Tables:").ok();
    for t in &info.tables {
        write!(out, " {}", t.signature_str()).ok();
    }
    writeln!(out).ok();
    let madt = &info.madt;
    writeln!(
        out,
        "Local APIC at {:#x}, legacy 8259 present: {}",
        madt.local_apic_address, madt.has_8259
    )
    .ok();
    for cpu in &madt.local_apics {
        writeln!(
            out,
            "  CPU {}: APIC ID {}{}",
            cpu.processor_id,
            cpu.apic_id,
            if cpu.flags & 1 != 0 { "" } else { " (disabled)" }
        )
        .ok();
    }
    for io in &madt.io_apics {
        writeln!(
            out,
            "  I/O APIC {} at {:#x}, GSI base {}",
            io.id, io.address, io.gsi_base
        )
        .ok();
    }
    for o in &madt.overrides {
        writeln!(
            out,
            "  ISA IRQ {} -> GSI {}{}{}",
            o.source,
            o.gsi,
            if o.active_low() { ", active low" } else { "" },
            if o.level_triggered() { ", level" } else { "" }
        )
        .ok();
    }
    Ok(())
}

fn cmd_lspci(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    for d in crate::drivers::pci::enumerate() {
        writeln!(
            out,
            "  {:?}  {:04x}:{:04x}  class {:02x}.{:02x}.{:02x}  {}",
            d.location,
            d.vendor_id,
            d.device_id,
            d.class,
            d.subclass,
            d.prog_if,
            d.class_name()
        )
        .ok();
    }
    Ok(())
}

fn cmd_disk(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    use crate::drivers::block::BlockDevice;
    for p in crate::drivers::STORAGE_PORTS.lock().iter() {
        writeln!(out, "  AHCI port {}: {:?}", p.port, p.kind).ok();
    }
    let disk = crate::drivers::DISK.lock();
    let d = disk.as_ref().ok_or("no disk found")?;
    let bytes = d.sector_count() * 512;
    writeln!(
        out,
        "Disk: '{}' on port {}, {} sectors ({} MiB)",
        d.model(),
        d.port(),
        d.sector_count(),
        bytes / (1024 * 1024)
    )
    .ok();
    let (msi, irqs) = crate::drivers::ahci::interrupt_stats();
    if msi {
        writeln!(out, "Completion: MSI interrupts ({} so far)", irqs).ok();
    } else {
        writeln!(out, "Completion: polling").ok();
    }
    if let Some((hits, misses, cached)) = fs::cache_stats() {
        writeln!(
            out,
            "Cache: {} KiB in use, {} hits, {} misses ({} % hits)",
            cached / 2,
            hits,
            misses,
            hits * 100 / (hits + misses).max(1)
        )
        .ok();
    }
    Ok(())
}

fn cmd_hexdump(_: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    use crate::drivers::block::BlockDevice;
    let lba: u64 = args
        .first()
        .ok_or("usage: hexdump <sector> [count]")?
        .parse()
        .map_err(|_| "bad sector number")?;
    let count: usize = args.get(1).map_or(Ok(1), |c| c.parse()).map_err(|_| "bad count")?;
    if !(1..=8).contains(&count) {
        return Err(String::from("count must be 1..8"));
    }
    let mut buf = alloc::vec![0u8; count * 512];
    crate::drivers::DISK
        .lock()
        .as_mut()
        .ok_or("no disk found")?
        .read(lba, &mut buf)
        .map_err(|e| format!("read failed: {:?}", e))?;
    for (i, line) in buf.chunks(16).enumerate() {
        write!(out, "{:08x}  ", lba * 512 + i as u64 * 16).ok();
        for b in line {
            write!(out, "{:02x} ", b).ok();
        }
        let text: String = line
            .iter()
            .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
            .collect();
        writeln!(out, " {}", text).ok();
    }
    Ok(())
}

fn cmd_layout(_: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    match args.first().map(|s| s.as_str()) {
        None => {}
        Some("de") => keyboard::set_layout(Layout::De),
        Some("us") => keyboard::set_layout(Layout::Us),
        Some(other) => return Err(format!("unknown layout '{}' (de, us)", other)),
    }
    writeln!(out, "keyboard layout: {:?}", keyboard::layout()).ok();
    Ok(())
}

fn cmd_history(shell: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    for (i, line) in shell.editor.history().iter().enumerate() {
        writeln!(out, "{:4}  {}", i + 1, line).ok();
    }
    Ok(())
}

fn cmd_ps(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    let threads = task::list();
    let total: u64 = threads.iter().map(|t| t.cpu_ticks).sum::<u64>().max(1);
    writeln!(out, "  TID  STATE         CPU  ON   NAME").ok();
    for t in threads {
        let state = match t.state {
            task::State::Running => String::from("running"),
            task::State::Ready => String::from("ready"),
            task::State::Blocked(task::Wait::Until(_)) => String::from("sleeping"),
            task::State::Blocked(task::Wait::Input) => String::from("wait input"),
            task::State::Blocked(task::Wait::Join(id)) => format!("join {}", id),
            task::State::Blocked(task::Wait::Pipe(_)) => String::from("wait pipe"),
            task::State::Blocked(task::Wait::Net) => String::from("wait net"),
            task::State::Blocked(task::Wait::Disk) => String::from("wait disk"),
            task::State::Blocked(task::Wait::Lock(_)) => String::from("wait lock"),
            task::State::Blocked(task::Wait::Futex(_)) => String::from("wait futex"),
            task::State::Dead => String::from("dead"),
        };
        let on = t.cpu.map_or(String::from("-"), |c| format!("{}", c));
        writeln!(
            out,
            "  {:>3}  {:<12} {:>3}%  {:>2}   {}",
            t.id,
            state,
            t.cpu_ticks * 100 / total,
            on,
            t.name
        )
        .ok();
    }
    Ok(())
}

fn cmd_demo(_: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let count: u64 = match args.first() {
        Some(a) => a.parse().map_err(|_| "not a number")?,
        None => 3,
    };
    if !(1..=16).contains(&count) {
        return Err(String::from("use 1 to 16 threads"));
    }
    writeln!(
        out,
        "starting {} worker threads, each counts primes below 30000...",
        count
    )
    .ok();
    let start = time::ticks();
    let ids: Vec<task::Tid> = (1..=count)
        .map(|n| {
            task::spawn("demo-worker", move || {
                // naive prime counting: pure CPU work, preempted by the timer
                let primes = (2u64..30_000)
                    .filter(|&k| (2..).take_while(|d| d * d <= k).all(|d| k % d != 0))
                    .count();
                let tid = task::current_id();
                crate::println!(
                    "  worker {} (thread {}) found {} primes after {} ms",
                    n,
                    tid,
                    primes,
                    time::uptime_ms()
                );
            })
        })
        .collect();
    for id in ids {
        task::join(id);
    }
    writeln!(
        out,
        "all workers done in {} ms",
        (time::ticks() - start) * time::MS_PER_TICK
    )
    .ok();
    Ok(())
}

fn cmd_netstat(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    use crate::net::tcp;
    let ip = |a: [u8; 4]| format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]);
    let me = crate::net::config().ok_or("no network card")?.ip;
    writeln!(out, "  PROTO  LOCAL                  REMOTE                 STATE").ok();
    for port in tcp::listening_ports() {
        writeln!(
            out,
            "  tcp    {:<22} {:<22} Listen",
            format!("{}:{}", ip(me), port),
            "*"
        )
        .ok();
    }
    for (state, local, remote, rport) in tcp::connections() {
        writeln!(
            out,
            "  tcp    {:<22} {:<22} {:?}",
            format!("{}:{}", ip(me), local),
            format!("{}:{}", ip(remote), rport),
            state
        )
        .ok();
    }
    Ok(())
}

fn cmd_colors(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    for base in [30, 90] {
        for i in 0..8 {
            write!(out, "\x1b[{}m{:>4} ", base + i, base + i).ok();
        }
        writeln!(out, "\x1b[0m").ok();
    }
    Ok(())
}

// ---- user programs ----

impl Shell {
    /// Resolves a program name: paths relative to the cwd, plain names in `/bin`.
    fn find_program(&self, name: &str) -> Option<String> {
        let path = if name.contains('/') {
            self.path(name)
        } else {
            format!("{}/{}", BIN_DIR, name)
        };
        let entry = fs::with_root(|fs| fs.lookup(&path)).ok()?;
        (!entry.is_dir()).then_some(path)
    }
}

impl Shell {
    /// Makes programs write into the command's `out` (for tests).
    pub fn capture_programs(&mut self) {
        self.capture_programs = true;
    }

    /// Loads `path` from the disk, runs it with `args` in the current
    /// directory and waits for it. Arguments are passed NUL-separated.
    fn run_program(&self, path: &str, args: &[String], out: &mut dyn Write) -> Result<(), String> {
        let image = fs::with_root(|fs| fs.read_file(path)).map_err(|e| fs_err(path, e))?;
        let name = path.rsplit('/').next().unwrap_or(path);
        let sink = Arc::new(Mutex::new(Vec::new()));
        let output = if self.capture_programs {
            process::Output::Capture(sink.clone())
        } else {
            process::Output::Console
        };
        // In the foreground: Ctrl+C kills the program
        let opts = process::Options {
            cwd: &self.cwd,
            foreground: true,
            ..process::Options::new(output)
        };
        let pid = process::spawn_with(name, &image, &args.join("\0"), opts).map_err(|e| format!("{}: {}", path, e))?;
        let status = process::wait(pid).ok_or("process vanished")?;
        out.write_str(&String::from_utf8_lossy(&sink.lock())).ok();
        exit_status_result(name, status)
    }
}

fn exit_status_result(name: &str, status: ExitStatus) -> Result<(), String> {
    match status {
        ExitStatus::Exited(0) => Ok(()),
        ExitStatus::Exited(code) => Err(format!("{} exited with code {}", name, code)),
        ExitStatus::Killed(reason) => Err(format!("{} was killed: {}", name, reason)),
    }
}

fn cmd_run(shell: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let program = arg(args, 0, "run <program> [args...]")?;
    let path = shell.path(program);
    shell.run_program(&path, &args[1..], out)
}

// ---- file system commands ----

impl Shell {
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    fn path(&self, arg: &str) -> String {
        fs::normalize(&self.cwd, arg)
    }
}

fn fs_err(path: &str, e: FsError) -> String {
    format!("{}: {}", path, e)
}

fn arg<'a>(args: &'a [String], i: usize, usage: &str) -> Result<&'a str, String> {
    args.get(i)
        .map(|s| s.as_str())
        .ok_or_else(|| format!("usage: {}", usage))
}

fn cmd_cd(shell: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let path = shell.path(args.first().map_or("/", |s| s.as_str()));
    let entry = fs::with_root(|fs| fs.lookup(&path)).map_err(|e| fs_err(&path, e))?;
    if !entry.is_dir() {
        return Err(fs_err(&path, FsError::NotADirectory));
    }
    shell.cwd = path;
    Ok(())
}

fn cmd_pwd(shell: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    writeln!(out, "{}", shell.cwd).ok();
    Ok(())
}

fn text_line(args: &[String]) -> Vec<u8> {
    let mut line = args.join(" ");
    line.push('\n');
    line.into_bytes()
}

fn cmd_write(shell: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let path = shell.path(arg(args, 0, "write <file> <text...>")?);
    fs::with_root(|fs| fs.write_file(&path, &text_line(&args[1..]))).map_err(|e| fs_err(&path, e))
}

fn cmd_append(shell: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let path = shell.path(arg(args, 0, "append <file> <text...>")?);
    fs::with_root(|fs| fs.append_file(&path, &text_line(&args[1..]))).map_err(|e| fs_err(&path, e))
}

fn cmd_touch(shell: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let path = shell.path(arg(args, 0, "touch <file>")?);
    fs::with_root(|fs| match fs.lookup(&path) {
        Ok(_) => Ok(()),
        Err(FsError::NotFound) => fs.write_file(&path, b""),
        Err(e) => Err(e),
    })
    .map_err(|e| fs_err(&path, e))
}

fn cmd_mkdir(shell: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let path = shell.path(arg(args, 0, "mkdir <dir>")?);
    fs::with_root(|fs| fs.create_dir(&path)).map_err(|e| fs_err(&path, e))
}

fn cmd_rm(shell: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let path = shell.path(arg(args, 0, "rm <path>")?);
    if shell.cwd == path || shell.cwd.starts_with(&format!("{}/", path)) {
        return Err(String::from(
            "cannot remove the current directory or one of its parents",
        ));
    }
    fs::with_root(|fs| fs.remove(&path)).map_err(|e| fs_err(&path, e))
}

fn cmd_mv(shell: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let from = shell.path(arg(args, 0, "mv <from> <to>")?);
    let mut to = shell.path(arg(args, 1, "mv <from> <to>")?);
    // "mv file dir" moves the file into the directory
    if let Ok(target) = fs::with_root(|fs| fs.lookup(&to)) {
        if target.is_dir() {
            let name = from.rsplit('/').next().unwrap_or("");
            to = fs::normalize(&to, name);
        }
    }
    fs::with_root(|fs| fs.rename(&from, &to)).map_err(|e| fs_err(&from, e))
}

fn cmd_stat(shell: &mut Shell, args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let path = shell.path(arg(args, 0, "stat <path>")?);
    let e = fs::with_root(|fs| fs.lookup(&path)).map_err(|e| fs_err(&path, e))?;
    writeln!(out, "  path:      {}", path).ok();
    writeln!(out, "  type:      {}", if e.is_dir() { "directory" } else { "file" }).ok();
    writeln!(out, "  size:      {} bytes", e.size).ok();
    writeln!(out, "  modified:  {}", e.modified).ok();
    writeln!(out, "  8.3 name:  {}", e.short_name()).ok();
    writeln!(out, "  cluster:   {}", e.cluster).ok();
    Ok(())
}

fn cmd_df(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    let (label, s) = fs::with_root(|fs| Ok((String::from(fs.label()), fs.stats()))).map_err(|e| format!("{}", e))?;
    let total = s.total_clusters as u64 * s.cluster_size as u64;
    let free = s.free_clusters as u64 * s.cluster_size as u64;
    const KIB: u64 = 1024;
    writeln!(out, "FAT32 volume '{}' on the system disk", label).ok();
    writeln!(
        out,
        "  size {} KiB, used {} KiB, free {} KiB ({} % used), cluster size {} bytes",
        total / KIB,
        (total - free) / KIB,
        free / KIB,
        (total - free) * 100 / total.max(1),
        s.cluster_size
    )
    .ok();
    Ok(())
}

fn cmd_date(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    writeln!(out, "{}", crate::drivers::rtc::now()).ok();
    Ok(())
}

fn cmd_version(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    writeln!(
        out,
        "KernelTry {} (x86_64, Rust, Limine boot protocol)",
        env!("CARGO_PKG_VERSION")
    )
    .ok();
    Ok(())
}

fn cmd_int3(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    unsafe { core::arch::asm!("int3") };
    writeln!(out, "breakpoint handled, back in the shell").ok();
    Ok(())
}

fn cmd_panic(_: &mut Shell, args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let message = if args.is_empty() {
        String::from("requested by the user")
    } else {
        args.join(" ")
    };
    panic!("{}", message);
}

fn cmd_reboot(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    writeln!(out, "rebooting...").ok();
    power::reboot()
}

fn cmd_poweroff(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    writeln!(out, "powering off...").ok();
    power::poweroff()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str) -> (bool, String) {
        let mut shell = Shell::new();
        shell.capture_programs();
        let mut out = String::new();
        let ok = shell.execute(line, &mut out);
        (ok, out)
    }

    #[test_case]
    fn echo_joins_arguments() {
        assert_eq!(
            run("echo hello   \"big world\""),
            (true, String::from("hello big world\n"))
        );
    }

    #[test_case]
    fn empty_line_is_fine() {
        assert_eq!(run("   "), (true, String::new()));
    }

    #[test_case]
    fn unknown_command_is_an_error() {
        let (ok, out) = run("frobnicate");
        assert!(!ok);
        assert!(out.contains("unknown command 'frobnicate'"));
    }

    #[test_case]
    fn parse_error_is_reported() {
        let (ok, out) = run("echo \"open");
        assert!(!ok);
        assert!(out.contains("unterminated"));
    }

    #[test_case]
    fn help_lists_every_command() {
        let (ok, out) = run("help");
        assert!(ok);
        for c in COMMANDS {
            assert!(out.contains(c.name), "help does not mention {}", c.name);
        }
        let (ok, out) = run("help echo");
        assert!(ok && out.contains("usage: echo"));
        assert!(!run("help nope").0);
    }

    #[test_case]
    fn info_commands_produce_output() {
        for cmd in [
            "mem", "memmap", "uptime", "cpu", "acpi", "version", "colors", "ps", "lspci", "disk",
        ] {
            let (ok, out) = run(cmd);
            assert!(ok && !out.is_empty(), "'{}' failed: {}", cmd, out);
        }
        assert!(run("memmap").1.contains("usable"));
    }

    #[test_case]
    fn layout_switching() {
        assert!(run("layout us").1.contains("Us"));
        assert!(run("layout de").1.contains("De"));
        assert!(!run("layout fr").0);
    }

    #[test_case]
    fn sleep_validates_argument() {
        assert!(!run("sleep").0);
        assert!(!run("sleep abc").0);
        assert!(run("sleep 20").0);
    }

    #[test_case]
    fn demo_runs_workers() {
        let (ok, out) = run("demo 2");
        assert!(ok, "{}", out);
        assert!(out.contains("all workers done"));
        assert!(!run("demo 99").0);
    }

    #[test_case]
    fn hexdump_shows_boot_signature() {
        let (ok, out) = run("hexdump 0");
        assert!(ok, "{}", out);
        assert!(out.contains("000001f0") && out.contains("55 aa"), "{}", out);
        assert!(!run("hexdump").0);
        assert!(!run("hexdump 0 99").0);
    }

    #[test_case]
    fn file_commands() {
        let mut shell = Shell::new();
        shell.capture_programs();
        let mut out = String::new();
        let mut ok = |shell: &mut Shell, line: &str| {
            out.clear();
            let r = shell.execute(line, &mut out);
            (r, out.clone())
        };
        assert!(ok(&mut shell, "mkdir shelltest").0);
        assert!(ok(&mut shell, "cd shelltest").0);
        assert_eq!(shell.cwd(), "/shelltest");
        assert!(ok(&mut shell, "write notes.txt hello file system").0);
        assert!(ok(&mut shell, "append notes.txt second line").0);
        assert_eq!(ok(&mut shell, "cat notes.txt").1, "hello file system\nsecond line\n");
        assert!(ok(&mut shell, "cp notes.txt \"copy of notes\"").0);
        assert_eq!(
            ok(&mut shell, "cat \"copy of notes\"").1,
            "hello file system\nsecond line\n"
        );
        assert!(ok(&mut shell, "rm \"copy of notes\"").0);
        assert!(ok(&mut shell, "ls").1.contains("notes.txt"));
        assert!(ok(&mut shell, "mv notes.txt \"renamed notes.txt\"").0);
        assert!(ok(&mut shell, "stat \"renamed notes.txt\"").1.contains("30 bytes"));
        assert!(ok(&mut shell, "touch empty").0);
        assert!(ok(&mut shell, "cat empty").1.is_empty());
        assert!(!ok(&mut shell, "rm /shelltest").0, "removing the cwd must fail");
        assert!(ok(&mut shell, "cd ..").0);
        assert_eq!(shell.cwd(), "/");
        assert!(!ok(&mut shell, "rm shelltest").1.is_empty(), "not empty");
        assert!(ok(&mut shell, "rm \"shelltest/renamed notes.txt\"").0);
        assert!(ok(&mut shell, "rm shelltest/empty").0);
        assert!(ok(&mut shell, "rm shelltest").0);
        assert!(ok(&mut shell, "cat nothing-here").1.contains("no such file"));
        assert!(ok(&mut shell, "df").1.contains("KERNELTRY"));
        assert!(!ok(&mut shell, "cd README.txt").0);
    }

    /// Needs the programs from `make user` on the test disk (`/bin`).
    #[test_case]
    fn programs_run_from_bin() {
        let (ok, out) = run("hello from \"a test\"");
        assert!(ok && out.contains("arg 1: from\n  arg 2: a test\n"), "{}", out);
        let (ok, out) = run("fstest");
        assert!(ok && out.contains("fstest: all checks passed"), "{}", out);
        assert!(run("/bin/fault exit 0").0);
        let (ok, out) = run("run /bin/fault exit 3");
        assert!(!ok && out.contains("fault exited with code 3"), "{}", out);
        let (ok, out) = run("fault null");
        assert!(!ok && out.contains("fault was killed: page fault at 0x0"), "{}", out);
        let (ok, out) = run("run /README.txt");
        assert!(!ok && out.contains("not an ELF file"), "{}", out);
        let (ok, out) = run("run /bin/nothing");
        assert!(!ok && out.contains("no such file"), "{}", out);
        assert_eq!(process::count(), 0);
    }

    #[test_case]
    fn builtins_for_user_space() {
        let mut shell = Shell::new();
        shell.set_cwd("/docs");
        let mut out = String::new();
        assert_eq!(shell.execute_builtin("echo from kshell", &mut out), Some(true));
        assert_eq!(out, "from kshell\n");
        assert_eq!(shell.execute_builtin("write kshell.tmp x", &mut out), Some(true));
        assert!(
            fs::with_root(|fs| fs.remove("/docs/kshell.tmp")).is_ok(),
            "relative to the cwd"
        );
        assert_eq!(shell.execute_builtin("sleep x", &mut out), Some(false));
        for excluded in ["cd /", "pwd", "history", "run /bin/hello", "nonexistent", "\"open"] {
            assert_eq!(shell.execute_builtin(excluded, &mut out), None, "{}", excluded);
        }
        let names = kshell_commands();
        assert!(names.contains(&"lspci") && names.contains(&"poweroff"));
        assert!(!names.contains(&"cd") && !names.contains(&"run") && !names.contains(&"mem"));
    }

    #[test_case]
    fn int3_returns() {
        assert!(run("int3").0);
    }

    #[test_case]
    fn completion_candidates() {
        assert_eq!(complete_command("me"), ["mem", "memmap"]);
        assert_eq!(complete_command("hel"), ["hello", "help"]);
        assert!(complete_command("zzz").is_empty());
    }
}
