# Design-Entscheidungen

## Grundlagen
| Thema | Entscheidung | Warum |
|---|---|---|
| Sprache | Rust nightly, `#![no_std]`, etwas Inline-Assembly | Speichersicherheit; nightly wird für `custom_test_frameworks` und `abi_x86_interrupt` gebraucht |
| Target | `x86_64-unknown-none` (eingebaut) | Bare Metal, Soft-Float (kein SSE im Kernel) → keine eigene Target-JSON nötig |
| Architektur | x86_64 | entspricht dem Host-PC, beste Doku (OSDev, Intel SDM) |
| Bootloader | Limine v11, Boot-Protokoll Base Revision 6, Crate `limine` 0.6.5 | Startet direkt im Long Mode mit Paging, liefert Memory Map, HHDM und Framebuffer |
| Boot-Medium | Hybrid-ISO (BIOS + UEFI) über xorriso | Lässt sich in QEMU mit `-cdrom` booten, beide Firmware-Pfade sind testbar |
| Externe Crates | nur `limine`, `spin` | Alles andere schreiben wir „from scratch“ |

## Speicherlayout
- Kernel liegt im Higher Half ab `0xffffffff80000000` (die oberen 2 GiB), `code-model=kernel`, `relocation-model=static`.
- Limine mappt den gesamten physischen Speicher zusätzlich ab dem HHDM-Offset (`boot::HHDM`).
  Eine physische Adresse `p` erreichen wir also über `p + hhdm_offset`.
- Linker-Sections: `.limine_requests` (zwischen Start- und End-Marker), `.text`, `.rodata`, `.data`/`.bss`, jede auf Seitengröße ausgerichtet und mit eigenem PT_LOAD.

## Ausgabe
- `serial_println!`: nur COM1. `println!`: Konsole **und** COM1. Beide sperren Interrupts, während sie den Lock halten (`without_interrupts`).
- Panic-Handler nutzen nur Serial, denn der Konsolen-Lock könnte beim Panic noch gehalten sein.
- Konsole: arbeitet auf einer `Surface` (32 bpp, Stride in Pixeln). Scrollen per Zeilen-Memcopy im Framebuffer. Das ist langsam auf echter Hardware, in QEMU aber okay.
  Font: Terminus 8×16 (PSF2) per `include_bytes!`. Latin-1-Zeichen gehen über die Unicode-Tabelle, unbekannte Zeichen werden zu `?`.

## Testen
- Crate-Aufbau: `src/lib.rs` enthält den ganzen Kernel. `src/main.rs` ist nur `kmain` + Panic-Handler.
  Jedes Test-Binary (lib-Unit-Tests, `tests/*.rs`) definiert sein eigenes `kmain`. Der Linker nutzt in allen `ENTRY(kmain)`.
- Unit-Tests: `#[test_case]` direkt im Modul. Integrationstests: `kernel/tests/<name>.rs` mit eigenem Harness-Setup (siehe `tests/boot.rs`).
  Tests, die panicken müssen, bekommen `harness = false` in `Cargo.toml` (siehe `tests/should_panic.rs`).
- `scripts/run-qemu.sh` ist der Cargo-Runner. Liegt das ELF unter `deps/`, ist es ein Test-Binary, dann laufen QEMU headless und ein Timeout.
- Ergebnis über das `isa-debug-exit`-Gerät an Port 0xF4: Wert 0x10 → QEMU-Exitcode 33 = PASS, 0x11 → 35 = FAIL.
- Alle Testausgaben gehen über COM1 (`serial_println!`).

## Interrupt-Vektoren
_(kommt mit M5/M6)_
