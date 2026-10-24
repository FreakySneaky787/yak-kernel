# Lernnotizen

Kurze Erklärungen zu jedem Subsystem: was es ist und warum wir es brauchen.

## M1 – Booten mit Limine
- Beim Einschalten startet die **Firmware** (BIOS oder UEFI). Sie lädt den **Bootloader** (Limine) von der ISO.
- Limine liest `limine.conf`, lädt unser ELF (`/boot/kernel`), schaltet die CPU in den **64-bit Long Mode**, richtet Paging ein und springt zu `kmain`.
- Wir kommunizieren mit Limine über **Requests**: statische Structs mit Magic-Nummern in einer eigenen Section. Limine findet sie im ELF und trägt Zeiger auf die Antworten ein, zum Beispiel Framebuffer oder Memory Map.
- **Base Revision**: Mit welcher Protokoll-Version wir arbeiten. Kann Limine sie nicht, setzt es das Feld nicht auf 0, und wir halten an.

## M1 – Serielle Schnittstelle (UART 16550)
- Ein uralter Standardchip, über **I/O-Ports** (`in`/`out`-Befehle) ab Adresse 0x3F8 ansprechbar.
- Init: Baudrate über einen Teiler setzen (DLAB-Bit), Datenformat 8N1, FIFO an.
- Senden: warten, bis Bit 5 im Line Status Register sagt „Sendepuffer leer“, dann das Byte schreiben.
- QEMU leitet das mit `-serial stdio` in dein Terminal um. Das ist unser `printf`-Debugging, und Tests werten genau diese Ausgabe aus.

## M1 – Panic-Handler
- Ohne `std` gibt es kein Programm, das bei einem Absturz die Meldung ausgibt. Das machen wir selbst mit `#[panic_handler]`: Meldung auf Serial, dann `cli; hlt` für immer.
