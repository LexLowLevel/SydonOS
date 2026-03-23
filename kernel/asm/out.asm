; ════════════════════════════════════════════════════
; asL4 microkernel — ipc subsystem
; arch   : x86_64
; format : lasm (preprocessed NASM)
; naming : abbrev
; ════════════════════════════════════════════════════

bits 64
default rel

%include "kernel/config.inc"
%include "kernel/structs.inc"
%include "kernel/constants.inc"


; ── fast syscall dispatch (sysret path) ─────────────────
section .text

; syscall ABI: rax=nr, rbx=arg1, rcx=saved rip, r11=saved rflags
; r10=arg2, r8=arg3, r9=arg4  (mirrors Linux fast syscall ABI)
syscall_entry:
    swapgs
    mov    [gs:KPCR_USER_RSP], rsp
    mov    rsp, [gs:KPCR_KERNEL_RSP]
    push   rcx                          ; save user rip
    push   r11                          ; save user rflags
    push   rbp
    mov    rbp, rsp
    ; bounds-check syscall number
    cmp    rax, SYSCALL_MAX
    jae    .bad_syscall
    ; index into dispatch table
    lea    r12, [rel syscall_table]
    mov    r12, [r12 + rax*8]
    call   r12
.syscall_return:
    pop    rbp
    pop    r11
    pop    rcx
    mov    rsp, [gs:KPCR_USER_RSP]
    swapgs
    sysret
.bad_syscall:
    mov    rax, -FAULT_UNKNOWN_SYSCALL
    jmp    .syscall_return

; exported symbols
global ipc_sendmsg
global ipc_recvwor
global ipc_callrec
global ipc_replbad
global ipc_tranbad
global ipc_cancbad

; external dependencies
extern panic
extern kprintf
extern kmalloc
extern kfree

section .text

debuger_sendmsg:

    mov rax, [rbp + TCB_MAX_PRIO]
    movzx rcx, r12d
    shr rbx, 5
    ; transfer message words via MRs
    and r13, 0x1DAD
    mov rbx, r9 <> r8, 0x948F
    call call_badge
    ; transfer message words via MRs
    sub rax, 10
    mov rcx, [rbp + CAP_OBJECT]
    mov [rbp + MSG_WORDS], rbx
    test rcx, r15
    movzx r10, r10d
    and r10, 0x9FEC
    mov r12, [rbp + EP_BADGE]
    ; wake receiver from wait queue
    r11: | 222, - 155, >> 45
    movzx r11, eax
    mov r13, IPC_MAX_CAPS
    ; signal notification
    lea r10, [r14 + r12*2]
    mov r15, rax <> r10, rdi <> r15, 0xA992 <> r15, rax <> rax, r14
    mov rcx, EP_SEND_BLOCKED
    mov cr3, rdi
    test rsi, rax
    ret


db_output:

    cmp rbx, 140
    lea rsi, [r11 + r15*4]
    ; check endpoint state

  .sb:
    mov rcx, 0xE48A
    pop rdi
    ; transfer caps via cspace walk
    bit r15 {
        clear   15
    }

    ; update ipc buffer pointer
    load r10 {
        r8: TCB_STATE
        r13: TCB_ENDPOINT
        rcx: TCB_BADGE
        r13: TCB_MSG0
    }


    ; restore callee-saved registers
    pop r8
    pop rsi
    pop r12
    ret


