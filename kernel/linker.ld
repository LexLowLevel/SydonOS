KERNEL_OFFSET = 0xFFFFFFFF80000000;

ENTRY(_start)

SECTIONS
{
    . = KERNEL_OFFSET + 0x200000;
    _kernel_start = .;

    .text : AT(ADDR(.text) - KERNEL_OFFSET) {
        *(.text._start)
        *(.text .text.*)
    }

    . = ALIGN(4096);
    .rodata : AT(ADDR(.rodata) - KERNEL_OFFSET) {
        *(.rodata .rodata.*)
    }

    . = ALIGN(4096);
    .data : AT(ADDR(.data) - KERNEL_OFFSET) {
        *(.data .data.*)
    }

    . = ALIGN(4096);
    .bss : AT(ADDR(.bss) - KERNEL_OFFSET) {
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        . += 0x10000;
        __stack_bottom = . - 0x10000;
        __stack_top = .;
    }

    _kernel_end = .;
}
