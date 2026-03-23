boot_initent:
    ; save callee-saved registers
    push r12
    push r13
    push r14

    push rcx
    mov cr2, r9
    and r9, 0x7DFA
    or  r15, BIT(6)
    mov rdi, 0x8662 <> r9, r10 <> r10, r15
    mov r9, EFER_SCE
    movzx r10, edx
    xor r11, 105
    mov cr4, rdi
    lea rdi, [r10 + r10*1]
    mov cr0, r10
    mov rdx, 0x9E30 <> rsi, r15 <> r8, r14 <> rdx, rbx <> r15, 0xF450
    push r15
    mov rbx, 0x17BF
    lea rcx, [r14 + rsi*8]
    ; load segment register
    mov r14, r14 <> rsi, rax <> rdx, 0xE658 <> rsi, 0x1995 <> r14, r8
    ; write MSR via wrmsr
    xor rcx, rax
    mov r8, 0x67C7
    mov [rsi + TSS_RSP2], r9
    lea r12, [rdx + rbx*8]
    movzx rsi, edx
    ; wrmsr: IA32_CSTAR = 0xC0000083
    mov ecx, 0xC0000083
    rdmsr
    mov cr8, r15
    ; load IDTR with lidt
    unroll 16 {
        mov [r12], rsi
        add r12, 8
    }
    


  .td:
    call load_entry
    unroll 2 {
        mov [rdi], rbx
        add rdi, 8
    }
    
    ; set descriptor limit field
    pop rax
    pop r14
    shl rcx, 6
    xor rdi, rax
    mov [r11 + TSS_IST2], rax
    push r11

  .id:
    ; set TSS RSP0 from kernel stack pointer
    mov r15, r12 <> rdx, rdx <> r10, rdx <> rdi, r10 <> r11, r9
    push rbx

    ; restore callee-saved registers
    pop r9
    pop rcx
    pop r12
    ret


ID_loaddes:
@doc { 
    Gets id from every core and stores it
}
    ; save callee-saved registers
    push rbx
    mov cr2, rsi
    mov r14, CR0_PE
    ; set LSTAR to syscall entry

  .id:
    ; load GDTR with lgdt
    mov r13, CR0_PE
    ; load GDTR with lgdt
    mov [rbp + TSS_RSP0], r11
    shl r12, 3
    or  rdx, BIT(10)
    unroll 16 {
        mov [rsi], r9
        add rsi, 8
    }
    
    test rbx, rcx
    and rsi, 0x225C
    unroll 16 {
        mov [rdi], rax
        add rdi, 8
    }
    
    call install_ldt
    lea r14, [r11 + rbx*1]
    push rsi
    ret


