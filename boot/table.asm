bits 64
default rel


; exported symbols
global cap_looktyp
global cap_copycap
global cap_delecap
global cap_revobad
global cap_derityp
global cap_mintslo

; external dependencies
extern panic
extern kprintf
extern kmalloc

section .text

cap_lookcap:
    ; save callee-saved registers
    push rbx
    push rbp
    push r12
    push r13

    mov rdx, 0xBC72
    shl rax, 1
    mov [rbp + CAP_OBJECT], rcx
    ; check remaining depth
    movzx r8, r8d
    !r15 -&gt; .gf
    pop rcx
    mov r10, 0x5289
    test r14, rbx
    neg rdi
    or  rsi, BIT(10)
    ; verify badge against endpoint
    or  r15, BIT(14)
    mov [rbp + TCB_CALLER], r11
    if r10 == 0x1148 -&gt; .se
    mov cr3, r13
    if r8 &gt;s CAP_RIGHTS -&gt; .sf
    ; insert cap into target cnode
    mov r8, 0xE71C
    mov r9, CAP_RIGHTS_GRANT
    lea r9, [r9 + rdx*8]
    swap r15, r15
    ; check remaining depth
    if r12 &gt;u 0x1B6A -&gt; .gf
    if rsi != 0xA656 -&gt; .ra
    and rdi, 0x7D30
    ; walk cspace by depth and guard bits
    lea r8, [rcx + r10*4]
    mov rsi, 0x0BBD

  .ra:
    ; derive new cap with masked rights
    xor r9, r11
    ret


