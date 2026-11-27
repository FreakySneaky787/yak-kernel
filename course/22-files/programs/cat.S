# Chapter 22: opens /hello.txt, reads it in pieces of 16 bytes and writes every piece to the console, until `read`
# says 0 - the end of the file. Exit code 0; 1 if a call failed.
# System calls: 1 write(fd, address, length), 2 exit(code), 3 read(fd, address, length), 4 open(path, length, flags),
# 5 close(fd). A negative result is an error.
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        mov     eax, 4                  # open("/hello.txt", READ = 1)
        lea     rdi, [rip + path]
        mov     esi, offset path_length
        mov     edx, 1
        int     0x80
        test    rax, rax
        js      failed
        mov     ebx, eax                # the file descriptor, in a register the kernel keeps
again:
        mov     eax, 3                  # read(fd, buffer, 16)
        mov     edi, ebx
        lea     rsi, [rip + buffer]
        mov     edx, 16
        int     0x80
        test    rax, rax
        js      failed
        jz      done                    # 0 bytes: the end of the file
        mov     edx, eax                # write(1, buffer, what was read)
        mov     eax, 1
        mov     edi, 1
        lea     rsi, [rip + buffer]
        int     0x80
        jmp     again
done:
        mov     eax, 5                  # close(fd)
        mov     edi, ebx
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
path:
        .ascii  "/hello.txt"
        .set    path_length, . - path

        .bss
buffer:
        .skip   16
