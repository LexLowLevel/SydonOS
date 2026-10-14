ENTRY(_start)

SECTIONS
{
    . = 0x400000 + SIZEOF_HEADERS;

    .sydon_manifest : {
        KEEP(*(.sydon_manifest))
    }

    .text : ALIGN(16) {
        *(.text._start)
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .data : {
        *(.data .data.*)
    }

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    }
}
