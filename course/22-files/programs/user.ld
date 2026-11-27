/* How `ld` lays out the programs of this course (chapter 21). Two LOAD segments, as the loader (elf.rs) wants
   them: code and constants, readable and executable, from 0x400000; data and .bss, readable and writable,
   from 0x401000 - a page of their own, so that no page has the flags of two segments. */
ENTRY(_start)

PHDRS
{
    code PT_LOAD FLAGS(5);  /* R X */
    data PT_LOAD FLAGS(6);  /* R W */
}

SECTIONS
{
    . = 0x400000;
    .text   : { *(.text .text.*) } :code
    .rodata : { *(.rodata .rodata.*) } :code

    . = 0x401000;
    .data : { *(.data .data.*) } :data
    .bss  : { *(.bss .bss.* COMMON) } :data

    /DISCARD/ : { *(.note* .comment .eh_frame*) }
}
