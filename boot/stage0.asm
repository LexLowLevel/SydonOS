BITS 16
ORG 0x7C00

start:
    jmp 0x0000:.norm
.norm:
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7C00
    sti
    mov [0x0500], dl

    in al, 0x92
    or al, 2
    out 0x92, al

    mov si, dap
    mov ah, 0x42
    mov dl, [0x0500]
    int 0x13
    jc disk_fail

    jmp 0x0000:0x8000

disk_fail:
    mov al, '!'
    mov ah, 0x0E
    mov bx, 0x0007
    int 0x10
.halt:
    hlt
    jmp .halt

align 4
dap:
    db 0x10, 0
    dw 32
    dw 0x8000, 0x0000
    dq 1

times 510 - ($ - $$) db 0
dw 0xAA55
