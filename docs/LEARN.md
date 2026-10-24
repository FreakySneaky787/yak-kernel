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

## M2 – Testen ohne Betriebssystem
- Normalerweise braucht `cargo test` die Standardbibliothek, die wir nicht haben. Mit `custom_test_frameworks` sammelt der Compiler alle `#[test_case]`-Funktionen und übergibt sie **unserer** `runner`-Funktion.
- Jeder Test wird als eigener Kernel gebaut. `cargo test` ruft für jedes Binary den **Runner** (`scripts/run-qemu.sh`) auf: ISO bauen, QEMU headless starten.
- Den Rückkanal bildet das virtuelle QEMU-Gerät **isa-debug-exit**. Ein Schreibzugriff auf Port 0xF4 beendet QEMU sofort mit Exitcode `(wert << 1) | 1`, so wird aus „Kernel sagt OK“ ein Exitcode für Cargo.
- Ein **Timeout** fängt Hänger ab, zum Beispiel eine Endlosschleife oder einen Triple Fault, nach dem QEMU wegen `-no-reboot` anhält.

## M3 – Text auf dem Bildschirm
- Der **Framebuffer** ist ein Stück Speicher, bei dem jedes 32-bit-Wort ein Pixel ist (0x00RRGGBB). Was wir hineinschreiben, erscheint sofort auf dem Bildschirm.
- Eine **Bitmap-Schrift** speichert pro Zeichen 16 Bytes, eins pro Pixelzeile. Bit 7 ist das linke Pixel. Zum Zeichnen prüfen wir jedes Bit und setzen dann Vorder- oder Hintergrundfarbe.
- **PSF2** ist das Fontformat der Linux-Konsole: Header, Glyphen und eine Tabelle „welcher Unicode-Buchstabe ist welcher Glyph“. Darüber kommen die Umlaute.
- **Scrolling**: Ist die letzte Zeile voll, kopieren wir alle Pixelzeilen um eine Schrifthöhe (16 px) nach oben und leeren die unterste Zeile.

## M4 – GDT und TSS
- Die **GDT** ist eine Tabelle von Segment-Deskriptoren, ein Überbleibsel aus 16/32-bit-Zeiten. Im 64-bit-Modus zählen fast nur noch zwei Dinge: ob ein Code-Segment „64-bit“ ist (L-Bit) und welche Privilegstufe gilt (Ring 0 = Kernel, Ring 3 = Programme).
- Das **CS-Register** lässt sich nicht einfach beschreiben. Wir legen Selector und Rücksprungadresse auf den Stack und machen einen „far return“ (`retfq`), dann lädt die CPU CS neu.
- Der **TSS** enthält im 64-bit-Modus keine Task-Infos mehr, sondern Stack-Zeiger. Die **IST** (Interrupt Stack Table) sind bis zu 7 Notfall-Stacks. Läuft der Kernel-Stack über, ist er kaputt, und der Double-Fault-Handler bräuchte selbst einen Stack. Deshalb lassen wir die CPU dafür auf einen eigenen IST-Stack umschalten. Ohne den gäbe es einen **Triple Fault**, und die CPU würde neu starten.

## M5 – Interrupts und Exceptions
- Eine **Exception** meldet die CPU selbst, wenn etwas schiefgeht: Division durch 0, ungültiger Befehl, Zugriff auf nicht gemappten Speicher (**Page Fault**) …
- Die **IDT** ist eine Tabelle mit 256 Einträgen. Für jeden Vektor steht dort, welche Funktion die CPU anspringen soll. Die CPU legt vorher RIP, CS, RFLAGS, RSP und SS auf den Stack (den **Interrupt Stack Frame**), bei manchen Exceptions zusätzlich einen **Fehlercode**.
- Handler enden mit `iretq` statt `ret`, damit die CPU den Frame wieder abräumt. Das erledigt `extern "x86-interrupt"` für uns.
- **Page Fault**: In CR2 steht die Adresse, die den Fehler ausgelöst hat. Der Fehlercode sagt, ob die Seite fehlte oder geschützt war, ob gelesen oder geschrieben wurde und ob Kernel oder User zugegriffen hat.
- **Double Fault**: Tritt auf, wenn beim Behandeln einer Exception eine weitere passiert. Klappt auch der nicht, kommt der **Triple Fault**, und die CPU startet neu. Mit `-no-reboot` bleibt QEMU dann einfach stehen.

## M6 – Hardware-Interrupts, PIC und Timer
- Geräte melden sich per **IRQ** (Interrupt Request). Der **8259-PIC** sammelt 16 IRQ-Leitungen (zwei Chips à 8, kaskadiert) und reicht sie als Interrupt-Vektor an die CPU weiter.
- **Remap**: Ab Werk würde IRQ0 als Vektor 8 ankommen, und das ist auch der Double Fault. Deshalb legen wir die IRQs auf 32–47.
- **EOI** (End of Interrupt): Jeder Handler muss dem PIC melden, dass er fertig ist, sonst kommt kein weiterer Interrupt.
- Der **PIT** ist ein Zähler mit 1,19 MHz. Teiler 11932 ergibt ~100 Interrupts pro Sekunde, also einen „Tick“ alle 10 ms. Daraus bauen wir Uptime und `sleep`.
- `hlt` legt die CPU schlafen, bis der nächste Interrupt kommt. So wartet der Kernel, ohne 100 % CPU zu verbrauchen.
- **APIC**: Moderne CPUs haben pro Kern einen Local APIC. Er sitzt zwischen PIC und CPU. Limine hat seinen PIC-Eingang gesperrt, darum schalten wir ihn vorerst ganz ab (Bug #2).
