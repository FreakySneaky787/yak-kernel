# Chapter 23: asks for a heap of 32 MiB with `brk` - and touches three pages of it: the first, one 16 MiB further, the
# last. The kernel maps a frame when each is first touched, three in all, not 8192.
# System calls: 1 write(fd, address, length), 2 exit(code), 7 brk(address): the new end of the heap, or the old one for
# address 0. Exit code 0 if what the program stored in the three places is what it reads back.
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        mov     eax, 7                  # brk(0): where does the heap start?
        xor     edi, edi
        int     0x80
        mov     rbx, rax
        mov     eax, 7                  # brk(start + 32 MiB)
        lea     rdi, [rbx + 0x2000000]
        int     0x80
        lea     rcx, [rbx + 0x2000000]
        cmp     rax, rcx                # the break is where we asked for it - or the kernel said no
        jne     failed
        mov     rax, 0x1CAB1E           # three stores, far apart: first page, middle, last 8 bytes
        mov     [rbx], rax
        mov     [rbx + 0x1000000], rax
        mov     [rbx + 0x2000000 - 8], rax
        cmp     [rbx], rax              # and back
        jne     failed
        cmp     [rbx + 0x1000000], rax
        jne     failed
        cmp     [rbx + 0x2000000 - 8], rax
        jne     failed
        mov     eax, 1                  # write(1, message, length)
        mov     edi, 1
        lea     rsi, [rip + message]
        mov     edx, offset length
        int     0x80
        xor     edi, edi                # exit(0)
        jmp     leave
failed:
        mov     edi, 1
leave:
        mov     eax, 2
        int     0x80
        ud2

        .section .rodata
message:
        .ascii  "Heap: 32 MiB asked for, three pages touched\n"
        .set    length, . - message
