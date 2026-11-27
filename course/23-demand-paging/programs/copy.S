# Chapter 22: copies /hello.txt to /copy.txt, 16 bytes at a time. Exit code: the number of bytes copied (21 for the
# test disk's hello.txt); 255 if a call failed.
# The new calls: 4 open(path, length, flags) with READ = 1, WRITE = 2, CREATE = 4, TRUNCATE = 8; 5 close(fd).
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        mov     eax, 4                  # open("/hello.txt", READ)
        lea     rdi, [rip + from]
        mov     esi, offset from_length
        mov     edx, 1
        int     0x80
        test    rax, rax
        js      failed
        mov     ebx, eax                # source
        mov     eax, 4                  # open("/copy.txt", WRITE | CREATE | TRUNCATE)
        lea     rdi, [rip + to]
        mov     esi, offset to_length
        mov     edx, 14
        int     0x80
        test    rax, rax
        js      failed
        mov     r12d, eax               # destination
        xor     r13d, r13d              # bytes copied so far
again:
        mov     eax, 3                  # read(source, buffer, 16)
        mov     edi, ebx
        lea     rsi, [rip + buffer]
        mov     edx, 16
        int     0x80
        test    rax, rax
        js      failed
        jz      done
        mov     r14, rax                # how many we got
        mov     edx, eax                # write(destination, buffer, that many)
        mov     eax, 1
        mov     edi, r12d
        lea     rsi, [rip + buffer]
        int     0x80
        cmp     rax, r14
        jne     failed                  # a short write or an error
        add     r13, r14
        jmp     again
done:
        mov     eax, 5                  # close both
        mov     edi, ebx
        int     0x80
        mov     eax, 5
        mov     edi, r12d
        int     0x80
        mov     rdi, r13                # exit(bytes copied)
        jmp     leave
failed:
        mov     edi, 255
leave:
        mov     eax, 2
        int     0x80
        ud2

        .section .rodata
from:
        .ascii  "/hello.txt"
        .set    from_length, . - from
to:
        .ascii  "/copy.txt"
        .set    to_length, . - to

        .bss
buffer:
        .skip   16
