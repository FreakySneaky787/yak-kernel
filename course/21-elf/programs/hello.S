# The first program that is a file (chapter 21): it writes a line - system call 1, `write(address, length)` -
# and ends with exit code 42 - system call 2, `exit(code)`.
# Built by the Makefile: `as` turns it into an object file, `ld` (with user.ld) into an ELF file.
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        mov     eax, 1                  # write
        lea     rdi, [rip + message]
        mov     esi, offset length      # "offset": the number itself, not the memory at that address
        int     0x80
        mov     eax, 2                  # exit(42)
        mov     edi, 42
        int     0x80
        ud2                             # exit does not return

        .section .rodata
message:
        .ascii  "Hello from ring 3!\n"
        .set    length, . - message
