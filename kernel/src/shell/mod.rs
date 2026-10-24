//! The kernel shell: reads lines with the line editor, splits them into
//! words and runs built-in commands.
//!
//! Commands write to a `&mut dyn fmt::Write`, so unit tests can run them
//! against a `String`; the interactive shell uses `Stdout` (console + serial).

pub mod line_editor;
pub mod parser;

use crate::arch::x86_64::{cpu, power};
use crate::drivers::keyboard;
use crate::drivers::scancode::Layout;
use crate::mem::{heap, pmm};
use crate::{boot, input, task, time};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use line_editor::{EditResult, LineEditor};

pub const PROMPT: &str = "\x1b[92mkernel\x1b[0m> ";

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
        name: "colors",
        usage: "colors",
        help: "show the console colour palette",
        run: cmd_colors,
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

/// Command names starting with `prefix` (for tab completion).
pub fn complete_command(prefix: &str) -> Vec<String> {
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .map(|c| String::from(c.name))
        .collect()
}

pub struct Shell {
    editor: LineEditor,
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
        }
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
        let Some(command) = find_command(name) else {
            writeln!(out, "\x1b[91merror:\x1b[0m unknown command '{}' (try 'help')", name).ok();
            return false;
        };
        match (command.run)(self, &words[1..], out) {
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
            self.editor.start(PROMPT, &mut out);
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
    let memmap = boot::MEMMAP.response().ok_or("no memory map from the bootloader")?;
    for e in memmap.entries() {
        writeln!(
            out,
            "  {:#014x} - {:#014x}  {:>9} KiB  {}",
            e.base,
            e.base + e.length,
            e.length / 1024,
            memmap_type_name(e.type_)
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
        "up {:02}:{:02}:{:02}.{:03} ({} ticks at {} Hz)",
        h,
        m,
        s,
        ms % 1000,
        time::ticks(),
        time::TIMER_HZ
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
    writeln!(out, "  TID  STATE         CPU    NAME").ok();
    for t in threads {
        let state = match t.state {
            task::State::Running => String::from("running"),
            task::State::Ready => String::from("ready"),
            task::State::Blocked(task::Wait::Until(_)) => String::from("sleeping"),
            task::State::Blocked(task::Wait::Input) => String::from("wait input"),
            task::State::Blocked(task::Wait::Join(id)) => format!("join {}", id),
            task::State::Dead => String::from("dead"),
        };
        writeln!(
            out,
            "  {:>3}  {:<12} {:>3}%    {}",
            t.id,
            state,
            t.cpu_ticks * 100 / total,
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

fn cmd_colors(_: &mut Shell, _: &[String], out: &mut dyn Write) -> Result<(), String> {
    for base in [30, 90] {
        for i in 0..8 {
            write!(out, "\x1b[{}m{:>4} ", base + i, base + i).ok();
        }
        writeln!(out, "\x1b[0m").ok();
    }
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
    fn int3_returns() {
        assert!(run("int3").0);
    }

    #[test_case]
    fn completion_candidates() {
        assert_eq!(complete_command("me"), ["mem", "memmap"]);
        assert!(complete_command("zzz").is_empty());
    }
}
