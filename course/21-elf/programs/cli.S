# A program that does what ring 3 may not: `cli` is a privileged instruction. The CPU raises a general protection
# fault at the very first instruction, and the kernel ends the program (chapter 20) - and goes on.
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        cli
        mov     eax, 2                  # never reached
        xor     edi, edi
        int     0x80
