BITS 16
ORG 0x8000

PARAMS       equ 0x8F00
PARAM_CR3    equ PARAMS
PARAM_STACK  equ PARAMS + 8
PARAM_ENTRY  equ PARAMS + 16

    ; SIPI starts us at 0800:0000. reload CS so the ORG 0x8000 labels work.
    cli
    jmp 0:real

real:
    xor ax, ax
    mov ds, ax
    lgdt [gdt_desc]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp 0x18:pm_entry

align 8
; same selectors as the kernel GDT so IDT gates stay valid before the AP loads its own
gdt:
    dq 0
    dq 0x00AF9A000000FFFF
    dq 0x00CF92000000FFFF
    dq 0x00CF9A000000FFFF
gdt_end:
gdt_desc:
    dw gdt_end - gdt - 1
    dd gdt

BITS 32
pm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax

    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax
    mov eax, [PARAM_CR3]
    mov cr3, eax
    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8
    wrmsr
    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    jmp 0x08:lm_entry

BITS 64
lm_entry:
    xor eax, eax
    mov fs, ax
    mov gs, ax
    mov rsp, [PARAM_STACK]
    mov rdi, PARAMS
    mov rax, [PARAM_ENTRY]
    xor ebp, ebp
    call rax
.halt:
    hlt
    jmp .halt
