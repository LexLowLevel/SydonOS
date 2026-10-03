BITS 16
ORG 0x8000

%define COM1 0x3F8

BOOTINFO     equ 0x7000
BOOT_DRIVE   equ 0x0500
MANIFEST     equ 0x7E00
KERN_BUF     equ 0x20000
MANIFEST_LBA equ 33
KERN_LBA     equ 34
KERN_MAX     equ 1024

%macro PUTC 1
    mov dx, COM1 + 5
%%poll:
    in al, dx
    test al, 0x20
    jz %%poll
    mov dx, COM1
    mov al, %1
    out dx, al
%endmacro

stage1_start:
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7C00
    sti
    call serial_init
    PUTC '1'

    mov word [dap_cnt], 1
    mov word [dap_off], MANIFEST
    mov word [dap_seg], 0
    mov dword [dap_lba], MANIFEST_LBA
    mov dword [dap_lba + 4], 0
    call disk_read
    ; the manifest sector holds a magic and the kernel ELF size
    cmp dword [MANIFEST], 0x31445953
    jne bad_manifest
    mov eax, [MANIFEST + 4]
    mov [kern_bytes], eax
    add eax, 511
    shr eax, 9
    cmp eax, KERN_MAX
    ja too_big
    mov [kern_sects], ax

    mov dword [BOOTINFO], 0x42445953
    mov dword [BOOTINFO + 8], 0
    mov dword [BOOTINFO + 16], KERN_BUF
    mov dword [BOOTINFO + 20], 0
    mov eax, [kern_bytes]
    mov [BOOTINFO + 24], eax
    mov dword [BOOTINFO + 28], 0

    xor bp, bp
    mov di, BOOTINFO + 32
    xor ebx, ebx
.e820:
    mov dword [di + 20], 1      ; ACPI 3 attribute: entry valid
    mov eax, 0xE820
    mov edx, 0x534D4150
    mov ecx, 24
    int 0x15
    jc .e820_done
    cmp eax, 0x534D4150
    jne .e820_done
    add di, 24
    inc bp
    cmp bp, 32
    jae .e820_done
    test ebx, ebx
    jnz .e820
.e820_done:
    movzx eax, bp
    mov [BOOTINFO + 8], eax

    call load_kernel
    PUTC '2'

    cli
    lgdt [gdt_desc]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp 0x08:pm_entry

; reads the ELF in 64-sector chunks, moving the segment 32 KiB each time
load_kernel:
    mov word [cur_seg], KERN_BUF >> 4
    mov dword [cur_lba], KERN_LBA
    mov ax, [kern_sects]
    mov [remain], ax
.lk:
    cmp word [remain], 0
    je .lk_done
    mov ax, [remain]
    cmp ax, 64
    jbe .lk_cnt
    mov ax, 64
.lk_cnt:
    mov [chunk], ax
    mov [dap_cnt], ax
    mov word [dap_off], 0
    mov ax, [cur_seg]
    mov [dap_seg], ax
    mov eax, [cur_lba]
    mov [dap_lba], eax
    mov dword [dap_lba + 4], 0
    call disk_read
    mov ax, [remain]
    sub ax, [chunk]
    mov [remain], ax
    mov eax, [cur_lba]
    movzx ecx, word [chunk]
    add eax, ecx
    mov [cur_lba], eax
    mov ax, [cur_seg]
    add ax, 0x800
    mov [cur_seg], ax
    jmp .lk
.lk_done:
    ret

disk_read:
    mov si, dap
    mov dl, [BOOT_DRIVE]
    mov ah, 0x42
    int 0x13
    jc disk_fail
    ret

disk_fail:
    PUTC 'D'
.h1:
    hlt
    jmp .h1

bad_manifest:
    PUTC 'M'
.h2:
    hlt
    jmp .h2

too_big:
    PUTC 'T'
.h3:
    hlt
    jmp .h3

serial_init:
    mov dx, COM1 + 1
    xor al, al
    out dx, al
    mov dx, COM1 + 3
    mov al, 0x80
    out dx, al
    mov dx, COM1
    mov al, 0x01
    out dx, al
    mov dx, COM1 + 1
    xor al, al
    out dx, al
    mov dx, COM1 + 3
    mov al, 0x03
    out dx, al
    mov dx, COM1 + 2
    mov al, 0xC7
    out dx, al
    mov dx, COM1 + 4
    mov al, 0x0B
    out dx, al
    ret

align 4
dap:
    db 0x10, 0
dap_cnt: dw 0
dap_off: dw 0
dap_seg: dw 0
dap_lba: dq 0

kern_bytes: dd 0
kern_sects: dw 0
cur_seg:    dw 0
cur_lba:    dd 0
chunk:      dw 0
remain:     dw 0

align 8
gdt:
    dq 0
    dq 0x00CF9A000000FFFF
    dq 0x00CF92000000FFFF
    dq 0x00AF9A000000FFFF
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
    mov esp, 0x7C00

    ; tables at 0x1000-0x6FFF, all 2 MiB pages. identity maps 0-2 GiB and
    ; 3-4 GiB (LAPIC), and the top 2 GiB of the address space maps 0-2 GiB
    ; again for the kernel.
    xor eax, eax
    mov edi, 0x1000
    mov ecx, 0x6000 / 4
    rep stosd

    mov dword [0x1000], 0x2003
    mov dword [0x1000 + 511 * 8], 0x5003

    mov dword [0x2000], 0x3003
    mov dword [0x2000 + 8], 0x4003
    mov dword [0x2000 + 3 * 8], 0x6003

    mov dword [0x5000 + 510 * 8], 0x3003
    mov dword [0x5000 + 511 * 8], 0x4003

    mov edi, 0x3000
    mov eax, 0x83
    mov ecx, 512
    call fill_pd
    mov edi, 0x4000
    mov eax, 0x40000083
    mov ecx, 512
    call fill_pd
    mov edi, 0x6000
    mov eax, 0xC0000083
    mov ecx, 512
    call fill_pd

    mov eax, 0x1000
    mov cr3, eax
    mov eax, cr4
    or eax, 1 << 5              ; PAE
    mov cr4, eax
    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8              ; EFER.LME
    wrmsr
    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    jmp 0x18:lm_entry

fill_pd:
    mov [edi], eax
    mov dword [edi + 4], 0
    add eax, 0x200000
    add edi, 8
    loop fill_pd
    ret

BITS 64
lm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    mov esp, 0x7C00
    PUTC 'L'

    mov rsi, KERN_BUF
    cmp dword [rsi], 0x464C457F
    jne elf_bad
    movzx ebx, word [rsi + 0x38]
    movzx r8d, word [rsi + 0x36]
    mov r9, [rsi + 0x20]
    add r9, rsi
    mov r10, [rsi + 0x18]
    cld
.ph:
    test ebx, ebx
    jz .ph_done
    cmp dword [r9], 1
    jne .ph_next
    mov rsi, [r9 + 0x08]
    add rsi, KERN_BUF
    mov rdi, [r9 + 0x18]
    test rdi, rdi
    jnz .dst
    mov rdi, [r9 + 0x10]
.dst:
    mov rcx, [r9 + 0x20]
    rep movsb
    mov rcx, [r9 + 0x28]
    sub rcx, [r9 + 0x20]
    xor eax, eax
    rep stosb
.ph_next:
    add r9, r8
    dec ebx
    jmp .ph
.ph_done:
    mov rdi, BOOTINFO           ; kernel_main(boot_info)
    jmp r10

elf_bad:
    PUTC 'E'
.h4:
    hlt
    jmp .h4
