/* * Multikernel Linker Script for x86_64
 * Defines the layout for the core kernel image.
 */

ENTRY(_start)

/* Define the physical and virtual addresses */
PHYS_ADDR = 0x00200000; /* 2MB - standard for loading above the ISA hole */

SECTIONS
{
    /* Start the kernel at the physical address */
    . = PHYS_ADDR;

    /* * The Multiboot header must be very early in the image 
     * to be found by GRUB/Limine.
     */
    .boot ALIGN(4K) :
    {
        KEEP(*(.multiboot_header))
    }

    /* --- Kernel Code Section --- */
    .text ALIGN(4K) :
    {
        *(.text .text.*)
    }

    /* --- Read-Only Data (Constants, Strings) --- */
    .rodata ALIGN(4K) :
    {
        *(.rodata .rodata.*)
    }

    /* --- Initialized Data --- */
    .data ALIGN(4K) :
    {
        *(.data .data.*)
    }

    /* --- Uninitialized Data and Stack --- */
    .bss ALIGN(4K) :
    {
        *(COMMON)
        *(.bss .bss.*)
        /* Ensure we have room for a basic bootstrap stack */
        *(.bootstrap_stack)
    }

    /* * Define a symbol for the end of the kernel image. 
     * Useful for the PMM (Physical Memory Manager) to know where free RAM starts.
     */
    _kernel_end = .;

    /* Discard unnecessary sections that bloat the binary */
    /DISCARD/ :
    {
        *(.comment)
        *(.eh_frame)
        *(.note.gnu.build-id)
    }
}
