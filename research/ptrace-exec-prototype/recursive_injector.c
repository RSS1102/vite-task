#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/futex.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

#if !defined(__x86_64__)
#error "The recursive browser prototype currently supports native x86-64 only"
#endif

extern char **environ;

#define ARRAY_LEN(values) (sizeof(values) / sizeof((values)[0]))
#define GATEWAY_MAGIC UINT64_C(0x4653505947415445)
#define GATEWAY_MAGIC_LOW UINT32_C(0x47415445)
#define FILTER_TAG UINT32_C(0x4653)
#define BRIDGE_SIGNAL (SIGRTMIN + 6)
#define VIRTUAL_ACTION_OFFSET 0U
#define INSTALL_ACTION_OFFSET 64U

/*
 * The copied handler contains no relocations, GOT/PLT references, TLS access,
 * libc calls, or stack-protector references. Runtime-specific values are
 * patched into the slots at the end of the blob before it is copied.
 */
__asm__(
    ".pushsection .text.fspy_recursive_injected,\"ax\",@progbits\n"
    ".balign 16\n"
    ".global fspy_recursive_blob_start\n"
    ".global fspy_recursive_handler\n"
    ".global fspy_recursive_restorer\n"
    ".global fspy_recursive_slot_rax\n"
    ".global fspy_recursive_slot_rdi\n"
    ".global fspy_recursive_slot_rsi\n"
    ".global fspy_recursive_slot_rdx\n"
    ".global fspy_recursive_slot_r10\n"
    ".global fspy_recursive_slot_r8\n"
    ".global fspy_recursive_slot_r9\n"
    ".global fspy_recursive_slot_sigmask\n"
    ".global fspy_recursive_slot_supervisor\n"
    ".global fspy_recursive_slot_signal\n"
    ".global fspy_recursive_slot_state\n"
    ".global fspy_recursive_slot_magic\n"
    ".global fspy_recursive_blob_end\n"
    "fspy_recursive_blob_start:\n"
    "fspy_recursive_handler:\n"
    "  push %rbp\n"
    "  mov %rsp, %rbp\n"
    "  push %rbx\n"
    "  push %r12\n"
    "  push %r13\n"
    "  push %r14\n"
    "  push %r15\n"
    "  sub $168, %rsp\n" /* keep the stack aligned for a logical handler */
    "  mov %rsi, %r12\n" /* siginfo_t * */
    "  mov %rdx, %r13\n" /* ucontext_t * */
    "  mov fspy_recursive_slot_state(%rip), %r14\n"
    /* A stacked filter may also return TRAP. Only fspy's filter tag belongs to
     * this dispatcher; all other traps belong to the target's logical action. */
    "  cmpl $0x4653, 4(%r12)\n" /* siginfo_t.si_errno */
    "  jne .Lfspy_logical_sigsys\n"
    "  mov 24(%r12), %eax\n" /* siginfo_t.si_syscall */
    "  cmp $39, %eax\n"      /* __NR_getpid */
    "  je .Lfspy_getpid\n"
    "  cmp $59, %eax\n" /* __NR_execve */
    "  je .Lfspy_exec\n"
    "  cmp $322, %eax\n" /* __NR_execveat */
    "  je .Lfspy_exec\n"
    "  cmp $13, %eax\n" /* __NR_rt_sigaction */
    "  je .Lfspy_sigaction\n"
    "  cmp $14, %eax\n" /* __NR_rt_sigprocmask */
    "  je .Lfspy_sigprocmask\n"
    "  cmp $47, %eax\n" /* __NR_recvmsg; diagnostic for Chromium zygote */
    "  je .Lfspy_recvmsg\n"
    "  cmp $217, %eax\n" /* __NR_getdents64 */
    "  je .Lfspy_passthrough\n"
    "  cmp $257, %eax\n" /* __NR_openat */
    "  je .Lfspy_passthrough\n"
    "  cmp $262, %eax\n" /* __NR_newfstatat */
    "  je .Lfspy_passthrough\n"
    "  cmp $269, %eax\n" /* __NR_faccessat */
    "  je .Lfspy_passthrough\n"
    "  cmp $332, %eax\n" /* __NR_statx */
    "  je .Lfspy_passthrough\n"
    "  cmp $437, %eax\n" /* __NR_openat2 */
    "  je .Lfspy_passthrough\n"
    "  cmp $439, %eax\n" /* __NR_faccessat2 */
    "  je .Lfspy_passthrough\n"
    "  mov $-38, %rax\n" /* -ENOSYS */
    "  jmp .Lfspy_store_result\n"

    ".Lfspy_logical_sigsys:\n"
    "  mov 0(%r14), %rax\n" /* virtual sa_handler/sa_sigaction */
    "  test %rax, %rax\n"   /* SIG_DFL */
    "  je .Lfspy_logical_default\n"
    "  cmp $1, %rax\n" /* SIG_IGN */
    "  je .Lfspy_return\n"
    "  mov 8(%r14), %rcx\n" /* virtual sa_flags */
    "  test $4, %ecx\n"     /* SA_SIGINFO */
    "  je .Lfspy_logical_one_arg\n"
    "  mov $31, %edi\n"
    "  mov %r12, %rsi\n"
    "  mov %r13, %rdx\n"
    "  call *%rax\n"
    "  jmp .Lfspy_return\n"
    ".Lfspy_logical_one_arg:\n"
    "  mov $31, %edi\n"
    "  call *%rax\n"
    "  jmp .Lfspy_return\n"
    ".Lfspy_logical_default:\n"
    "  mov $159, %edi\n" /* prototype equivalent of default SIGSYS death */
    "  mov $231, %eax\n" /* __NR_exit_group */
    "  syscall\n"
    "  ud2\n"

    ".Lfspy_getpid:\n"
    "  mov $39, %eax\n"
    "  mov fspy_recursive_slot_magic(%rip), %r9\n"
    "  syscall\n"
    "  jmp .Lfspy_store_result\n"

    /* Chromium's namespace sandbox performs a blocking recvmsg immediately
     * after launching its zygote. Keep this diagnostic in the research proof
     * until the transient-ptrace compatibility question is resolved. */
    ".Lfspy_recvmsg:\n"
    "  mov fspy_recursive_slot_rdi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdi\n"
    "  mov fspy_recursive_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rsi\n"
    "  mov fspy_recursive_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdx\n"
    "  mov fspy_recursive_slot_magic(%rip), %r9\n"
    "  mov $47, %eax\n" /* __NR_recvmsg */
    "  syscall\n"
    "  mov %rax, %r15\n"
    "  test %rax, %rax\n"
    "  js .Lfspy_recvmsg_error\n"
    "  jz .Lfspy_recvmsg_eof\n"
    "  lea .Lfspy_recvmsg_positive_message(%rip), %rsi\n"
    "  mov $32, %edx\n"
    "  jmp .Lfspy_recvmsg_log\n"
    ".Lfspy_recvmsg_eof:\n"
    "  lea .Lfspy_recvmsg_eof_message(%rip), %rsi\n"
    "  mov $27, %edx\n"
    "  jmp .Lfspy_recvmsg_log\n"
    ".Lfspy_recvmsg_error:\n"
    "  cmp $-4, %rax\n" /* -EINTR */
    "  je .Lfspy_recvmsg_eintr\n"
    "  cmp $-2, %rax\n" /* -ENOENT */
    "  je .Lfspy_recvmsg_enoent\n"
    "  lea .Lfspy_recvmsg_error_message(%rip), %rsi\n"
    "  mov $29, %edx\n"
    "  jmp .Lfspy_recvmsg_log\n"
    ".Lfspy_recvmsg_eintr:\n"
    "  lea .Lfspy_recvmsg_eintr_message(%rip), %rsi\n"
    "  mov $29, %edx\n"
    "  jmp .Lfspy_recvmsg_log\n"
    ".Lfspy_recvmsg_enoent:\n"
    "  lea .Lfspy_recvmsg_enoent_message(%rip), %rsi\n"
    "  mov $30, %edx\n"
    ".Lfspy_recvmsg_log:\n"
    "  mov $2, %edi\n"
    "  mov $1, %eax\n" /* __NR_write */
    "  syscall\n"
    "  mov %r15, %rax\n"
    "  jmp .Lfspy_store_result\n"

    ".Lfspy_passthrough:\n"
    "  mov %eax, %ebx\n"
    "  mov fspy_recursive_slot_rdi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdi\n"
    "  mov fspy_recursive_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rsi\n"
    "  mov fspy_recursive_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdx\n"
    "  mov fspy_recursive_slot_r10(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r10\n"
    "  mov fspy_recursive_slot_r8(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r8\n"
    "  mov fspy_recursive_slot_magic(%rip), %r9\n"
    "  mov %ebx, %eax\n"
    "  syscall\n"
    "  jmp .Lfspy_store_result\n"

    ".Lfspy_sigaction:\n"
    "  mov fspy_recursive_slot_rdi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdi\n"
    "  cmp $31, %edi\n" /* SIGSYS */
    "  jne .Lfspy_sigaction_passthrough\n"
    "  mov fspy_recursive_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdi\n" /* oldact */
    "  test %rdi, %rdi\n"
    "  je .Lfspy_sigaction_no_old\n"
    "  mov 0(%r14), %rax\n"
    "  mov %rax, 0(%rdi)\n"
    "  mov 8(%r14), %rax\n"
    "  mov %rax, 8(%rdi)\n"
    "  mov 16(%r14), %rax\n"
    "  mov %rax, 16(%rdi)\n"
    "  mov 24(%r14), %rax\n"
    "  mov %rax, 24(%rdi)\n"
    ".Lfspy_sigaction_no_old:\n"
    "  mov fspy_recursive_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rsi\n" /* act */
    "  test %rsi, %rsi\n"
    "  je .Lfspy_sigaction_done\n"
    "  mov 0(%rsi), %rax\n"
    "  mov %rax, 0(%r14)\n"
    "  mov 8(%rsi), %rax\n"
    "  mov %rax, 8(%r14)\n"
    "  mov 16(%rsi), %rax\n"
    "  mov %rax, 16(%r14)\n"
    "  mov 24(%rsi), %rax\n"
    "  mov %rax, 24(%r14)\n"
    ".Lfspy_sigaction_done:\n"
    "  xor %eax, %eax\n"
    "  jmp .Lfspy_store_result\n"
    ".Lfspy_sigaction_passthrough:\n"
    "  mov fspy_recursive_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rsi\n"
    "  mov fspy_recursive_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdx\n"
    "  mov fspy_recursive_slot_r10(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r10\n"
    "  mov $13, %eax\n"
    "  mov fspy_recursive_slot_magic(%rip), %r9\n"
    "  syscall\n"
    "  jmp .Lfspy_store_result\n"

    ".Lfspy_sigprocmask:\n"
    "  mov fspy_recursive_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdx\n" /* oldset */
    "  test %rdx, %rdx\n"
    "  je .Lfspy_sigmask_no_old\n"
    "  mov fspy_recursive_slot_sigmask(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rax\n"
    "  mov %rax, (%rdx)\n"
    ".Lfspy_sigmask_no_old:\n"
    "  mov fspy_recursive_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rsi\n" /* set */
    "  test %rsi, %rsi\n"
    "  je .Lfspy_sigmask_success\n"
    "  mov fspy_recursive_slot_r10(%rip), %rcx\n"
    "  cmpq $8, (%r13,%rcx)\n"
    "  jne .Lfspy_sigmask_einval\n"
    "  mov fspy_recursive_slot_sigmask(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rax\n" /* pre-signal mask */
    "  mov (%rsi), %rdx\n"
    "  mov fspy_recursive_slot_rdi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdi\n" /* how */
    "  test %edi, %edi\n"       /* SIG_BLOCK */
    "  je .Lfspy_sigmask_block\n"
    "  cmp $1, %edi\n" /* SIG_UNBLOCK */
    "  je .Lfspy_sigmask_unblock\n"
    "  cmp $2, %edi\n" /* SIG_SETMASK */
    "  jne .Lfspy_sigmask_einval\n"
    "  mov %rdx, %rax\n"
    "  jmp .Lfspy_sigmask_apply\n"
    ".Lfspy_sigmask_block:\n"
    "  or %rdx, %rax\n"
    "  jmp .Lfspy_sigmask_apply\n"
    ".Lfspy_sigmask_unblock:\n"
    "  not %rdx\n"
    "  and %rdx, %rax\n"
    ".Lfspy_sigmask_apply:\n"
    "  btr $30, %rax\n" /* SIGSYS must remain unblocked. */
    "  mov fspy_recursive_slot_sigmask(%rip), %rcx\n"
    "  mov %rax, (%r13,%rcx)\n"
    ".Lfspy_sigmask_success:\n"
    "  xor %eax, %eax\n"
    "  jmp .Lfspy_store_result\n"
    ".Lfspy_sigmask_einval:\n"
    "  mov $-22, %rax\n"
    "  jmp .Lfspy_store_result\n"

    ".Lfspy_exec:\n"
    "  movq $0, -48(%rbp)\n" /* supervisor-release flag */
    "  lea -192(%rbp), %rdi\n"
    "  xor %eax, %eax\n"
    "  mov $16, %ecx\n"
    "  rep stosq\n" /* clear a 128-byte siginfo_t */
    "  mov fspy_recursive_slot_signal(%rip), %eax\n"
    "  mov %eax, -192(%rbp)\n" /* si_signo */
    "  movl $-1, -184(%rbp)\n" /* si_code = SI_QUEUE */
    "  mov $186, %eax\n"       /* __NR_gettid */
    "  syscall\n"
    "  mov %eax, %ebx\n"
    "  mov %eax, -176(%rbp)\n" /* si_pid: exact requesting TID */
    "  mov $102, %eax\n"       /* __NR_getuid */
    "  syscall\n"
    "  mov %eax, -172(%rbp)\n" /* si_uid */
    "  lea -48(%rbp), %rax\n"
    "  mov %rax, -168(%rbp)\n" /* si_value.sival_ptr */
    "  mov fspy_recursive_slot_supervisor(%rip), %rdi\n"
    "  mov fspy_recursive_slot_signal(%rip), %rsi\n"
    "  lea -192(%rbp), %rdx\n"
    "  mov $129, %eax\n" /* __NR_rt_sigqueueinfo */
    "  syscall\n"
    "  test %rax, %rax\n"
    "  js .Lfspy_bridge_failed\n"
    ".Lfspy_wait_for_supervisor:\n"
    "  cmpq $0, -48(%rbp)\n"
    "  jne .Lfspy_exec_ready\n"
    "  lea -48(%rbp), %rdi\n"
    "  mov $128, %esi\n" /* FUTEX_WAIT_PRIVATE */
    "  xor %edx, %edx\n"
    "  xor %r10d, %r10d\n"
    "  xor %r8d, %r8d\n"
    "  xor %r9d, %r9d\n"
    "  mov $202, %eax\n" /* __NR_futex */
    "  syscall\n"
    "  jmp .Lfspy_wait_for_supervisor\n"
    ".Lfspy_exec_ready:\n"
    /* A successful exec never reaches rt_sigreturn, so undo the kernel's
     * automatic SIGSYS block before replacing the image. */
    "  movq $0x40000000, -56(%rbp)\n"
    "  mov $1, %edi\n" /* SIG_UNBLOCK */
    "  lea -56(%rbp), %rsi\n"
    "  xor %edx, %edx\n"
    "  mov $8, %r10d\n"
    "  mov fspy_recursive_slot_magic(%rip), %r9\n"
    "  mov $14, %eax\n" /* __NR_rt_sigprocmask */
    "  syscall\n"
    "  test %rax, %rax\n"
    "  js .Lfspy_bridge_failed\n"
    "  mov 24(%r12), %ebx\n"
    "  mov fspy_recursive_slot_rdi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdi\n"
    "  mov fspy_recursive_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rsi\n"
    "  mov fspy_recursive_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdx\n"
    "  mov fspy_recursive_slot_r10(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r10\n"
    "  mov fspy_recursive_slot_r8(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r8\n"
    "  mov fspy_recursive_slot_magic(%rip), %r9\n"
    "  mov %ebx, %eax\n"
    "  syscall\n"
    "  jmp .Lfspy_store_result\n" /* only a failed exec returns */
    ".Lfspy_bridge_failed:\n"
    "  mov $121, %edi\n"
    "  mov $231, %eax\n" /* __NR_exit_group */
    "  syscall\n"
    "  ud2\n"

    ".Lfspy_store_result:\n"
    "  mov fspy_recursive_slot_rax(%rip), %rcx\n"
    "  mov %rax, (%r13,%rcx)\n"
    ".Lfspy_return:\n"
    "  add $168, %rsp\n"
    "  pop %r15\n"
    "  pop %r14\n"
    "  pop %r13\n"
    "  pop %r12\n"
    "  pop %rbx\n"
    "  pop %rbp\n"
    "  ret\n"
    ".balign 8\n"
    "fspy_recursive_restorer:\n"
    "  mov $15, %eax\n" /* __NR_rt_sigreturn */
    "  syscall\n"
    "  ud2\n"
    ".Lfspy_recvmsg_positive_message: .ascii \"fspy: recvmsg returned positive\\n\"\n"
    ".Lfspy_recvmsg_eof_message: .ascii \"fspy: recvmsg returned EOF\\n\"\n"
    ".Lfspy_recvmsg_error_message: .ascii \"fspy: recvmsg returned error\\n\"\n"
    ".Lfspy_recvmsg_eintr_message: .ascii \"fspy: recvmsg returned EINTR\\n\"\n"
    ".Lfspy_recvmsg_enoent_message: .ascii \"fspy: recvmsg returned ENOENT\\n\"\n"
    ".balign 8\n"
    "fspy_recursive_slot_rax: .quad 0\n"
    "fspy_recursive_slot_rdi: .quad 0\n"
    "fspy_recursive_slot_rsi: .quad 0\n"
    "fspy_recursive_slot_rdx: .quad 0\n"
    "fspy_recursive_slot_r10: .quad 0\n"
    "fspy_recursive_slot_r8: .quad 0\n"
    "fspy_recursive_slot_r9: .quad 0\n"
    "fspy_recursive_slot_sigmask: .quad 0\n"
    "fspy_recursive_slot_supervisor: .quad 0\n"
    "fspy_recursive_slot_signal: .quad 0\n"
    "fspy_recursive_slot_state: .quad 0\n"
    "fspy_recursive_slot_magic: .quad 0\n"
    "fspy_recursive_blob_end:\n"
    ".popsection\n");

extern const unsigned char fspy_recursive_blob_start[];
extern const unsigned char fspy_recursive_handler[];
extern const unsigned char fspy_recursive_restorer[];
extern const unsigned char fspy_recursive_slot_rax[];
extern const unsigned char fspy_recursive_slot_rdi[];
extern const unsigned char fspy_recursive_slot_rsi[];
extern const unsigned char fspy_recursive_slot_rdx[];
extern const unsigned char fspy_recursive_slot_r10[];
extern const unsigned char fspy_recursive_slot_r8[];
extern const unsigned char fspy_recursive_slot_r9[];
extern const unsigned char fspy_recursive_slot_sigmask[];
extern const unsigned char fspy_recursive_slot_supervisor[];
extern const unsigned char fspy_recursive_slot_signal[];
extern const unsigned char fspy_recursive_slot_state[];
extern const unsigned char fspy_recursive_slot_magic[];
extern const unsigned char fspy_recursive_blob_end[];

struct kernel_sigaction_wire {
    uint64_t handler;
    uint64_t flags;
    uint64_t restorer;
    uint64_t mask;
};

struct bridge_state {
    pid_t root_pid;
    atomic_bool stopping;
    atomic_bool root_reaped;
    atomic_int root_status;
    atomic_uint exec_count;
    atomic_uint failed_exec_count;
};

static pid_t cleanup_process_group = -1;

static void cleanup_children(void)
{
    if (cleanup_process_group > 0)
        (void)kill(-cleanup_process_group, SIGKILL);
}

static void fatal(const char *message)
{
    perror(message);
    exit(EXIT_FAILURE);
}

static void fatal_message(const char *message)
{
    fputs(message, stderr);
    fputc('\n', stderr);
    exit(EXIT_FAILURE);
}

static size_t blob_offset(const unsigned char *symbol)
{
    return (size_t)(symbol - fspy_recursive_blob_start);
}

static void patch_u64(unsigned char *blob, const unsigned char *slot,
                      uint64_t value)
{
    memcpy(blob + blob_offset(slot), &value, sizeof(value));
}

static unsigned char *prepare_blob(uintptr_t state_address,
                                   pid_t supervisor_pid)
{
    const size_t blob_size =
        (size_t)(fspy_recursive_blob_end - fspy_recursive_blob_start);
    unsigned char *blob = malloc(blob_size);

    if (blob == NULL)
        fatal("malloc injected blob");
    memcpy(blob, fspy_recursive_blob_start, blob_size);
    patch_u64(blob, fspy_recursive_slot_rax,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RAX]));
    patch_u64(blob, fspy_recursive_slot_rdi,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RDI]));
    patch_u64(blob, fspy_recursive_slot_rsi,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RSI]));
    patch_u64(blob, fspy_recursive_slot_rdx,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RDX]));
    patch_u64(blob, fspy_recursive_slot_r10,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_R10]));
    patch_u64(blob, fspy_recursive_slot_r8,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_R8]));
    patch_u64(blob, fspy_recursive_slot_r9,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_R9]));
    patch_u64(blob, fspy_recursive_slot_sigmask,
              offsetof(ucontext_t, uc_sigmask));
    patch_u64(blob, fspy_recursive_slot_supervisor,
              (uint64_t)supervisor_pid);
    patch_u64(blob, fspy_recursive_slot_signal, BRIDGE_SIGNAL);
    patch_u64(blob, fspy_recursive_slot_state, state_address);
    patch_u64(blob, fspy_recursive_slot_magic, GATEWAY_MAGIC);
    return blob;
}

static struct kernel_sigaction_wire make_install_action(uintptr_t code_address)
{
    struct kernel_sigaction_wire action = {0};

    action.handler = code_address + blob_offset(fspy_recursive_handler);
    action.flags = SA_SIGINFO | SA_NODEFER | 0x04000000UL; /* SA_RESTORER */
    action.restorer = code_address + blob_offset(fspy_recursive_restorer);
    return action;
}

static void install_initial_handler(void)
{
    const size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    const size_t blob_size =
        (size_t)(fspy_recursive_blob_end - fspy_recursive_blob_start);
    unsigned char *mapping;
    unsigned char *blob;
    struct kernel_sigaction_wire action;

    if (page_size == 0 || blob_size > page_size)
        fatal_message("recursive injected blob does not fit in one page");
    mapping = mmap(NULL, page_size * 2, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED)
        fatal("mmap initial handler");
    blob = prepare_blob((uintptr_t)mapping + page_size, getppid());
    memcpy(mapping, blob, blob_size);
    free(blob);
    action = make_install_action((uintptr_t)mapping);
    if (syscall(SYS_rt_sigaction, SIGSYS, &action, NULL, 8) != 0)
        fatal("rt_sigaction initial handler");
    if (mprotect(mapping, page_size, PROT_READ | PROT_EXEC) != 0)
        fatal("mprotect initial handler");
}

#define FILTER_GATEWAY_BLOCK(syscall_number, tag)                              \
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (syscall_number), 0, 4),              \
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,                                    \
                 offsetof(struct seccomp_data, args[5])),                     \
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, GATEWAY_MAGIC_LOW, 1, 0),         \
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | (tag)),                  \
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW)

static void install_filter(void)
{
    struct sock_filter full_instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        FILTER_GATEWAY_BLOCK(SYS_execve, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_execveat, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_getpid, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_getdents64, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_openat, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_openat2, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_newfstatat, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_statx, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_faccessat, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_faccessat2, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_rt_sigaction, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_rt_sigprocmask, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_recvmsg, FILTER_TAG),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_filter minimal_instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        FILTER_GATEWAY_BLOCK(SYS_execve, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_execveat, FILTER_TAG),
        FILTER_GATEWAY_BLOCK(SYS_rt_sigaction, FILTER_TAG),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    const bool minimal = getenv("FSPY_MINIMAL_FILTER") != NULL;
    struct sock_fprog program = {
        .len = (unsigned short)(minimal ? ARRAY_LEN(minimal_instructions)
                                       : ARRAY_LEN(full_instructions)),
        .filter = minimal ? minimal_instructions : full_instructions,
    };

    fprintf(stderr, "bridge: installing %s syscall filter\n",
            minimal ? "minimal exec/signal" : "full research");
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        fatal("PR_SET_NO_NEW_PRIVS");
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program) != 0)
        fatal("SECCOMP_SET_MODE_FILTER");
}

static int ptrace_get_regs(pid_t pid, struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_GETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS,
                       &iov);
}

static int ptrace_set_regs(pid_t pid, const struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = (void *)regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_SETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS,
                       &iov);
}

static int wait_for_injected_breakpoint(pid_t pid)
{
    int status;

    if (waitpid(pid, &status, __WALL) < 0)
        return -1;
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        fprintf(stderr, "unexpected remote-syscall status: %#x\n", status);
        errno = EPROTO;
        return -1;
    }
    return 0;
}

static long remote_syscall(pid_t pid, long number, uint64_t a0, uint64_t a1,
                           uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5)
{
    struct user_regs_struct saved;
    struct user_regs_struct call_regs;
    struct user_regs_struct stopped;
    unsigned long original_word;
    unsigned long patched_word;
    uintptr_t pc;
    long result;

    if (ptrace_get_regs(pid, &saved) < 0)
        fatal("PTRACE_GETREGSET remote syscall");
    call_regs = saved;
    pc = (uintptr_t)saved.rip;
    errno = 0;
    original_word =
        (unsigned long)ptrace(PTRACE_PEEKTEXT, pid, (void *)pc, NULL);
    if (original_word == (unsigned long)-1 && errno != 0)
        fatal("PTRACE_PEEKTEXT remote syscall");
    patched_word = original_word;
    ((unsigned char *)&patched_word)[0] = 0x0f; /* syscall */
    ((unsigned char *)&patched_word)[1] = 0x05;
    ((unsigned char *)&patched_word)[2] = 0xcc; /* int3 */
    if (ptrace(PTRACE_POKETEXT, pid, (void *)pc, (void *)patched_word) < 0)
        fatal("PTRACE_POKETEXT remote syscall");
    call_regs.rax = (uint64_t)number;
    call_regs.orig_rax = UINT64_MAX;
    call_regs.rdi = a0;
    call_regs.rsi = a1;
    call_regs.rdx = a2;
    call_regs.r10 = a3;
    call_regs.r8 = a4;
    call_regs.r9 = a5;
    if (ptrace_set_regs(pid, &call_regs) < 0)
        fatal("PTRACE_SETREGSET remote syscall");
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) < 0)
        fatal("PTRACE_CONT remote syscall");
    if (wait_for_injected_breakpoint(pid) < 0)
        fatal("waitpid remote syscall");
    if (ptrace_get_regs(pid, &stopped) < 0)
        fatal("PTRACE_GETREGSET remote result");
    result = (long)stopped.rax;
    if (ptrace(PTRACE_POKETEXT, pid, (void *)pc, (void *)original_word) < 0)
        fatal("PTRACE_POKETEXT restore");
    if (ptrace_set_regs(pid, &saved) < 0)
        fatal("PTRACE_SETREGSET restore");
    return result;
}

static void remote_write(pid_t pid, uintptr_t destination, const void *source,
                         size_t length)
{
    const unsigned char *bytes = source;
    size_t offset = 0;

    while (offset < length) {
        unsigned long word = 0;
        size_t chunk = length - offset;
        if (chunk > sizeof(word))
            chunk = sizeof(word);
        memcpy(&word, bytes + offset, chunk);
        if (ptrace(PTRACE_POKEDATA, pid, (void *)(destination + offset),
                   (void *)word) < 0)
            fatal("PTRACE_POKEDATA injected blob");
        offset += chunk;
    }
}

static void inject_handler(pid_t pid)
{
    const size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    const size_t blob_size =
        (size_t)(fspy_recursive_blob_end - fspy_recursive_blob_start);
    unsigned char *blob;
    struct kernel_sigaction_wire action;
    uintptr_t remote_code;
    uintptr_t remote_state;
    long result;

    if (page_size == 0 || blob_size > page_size ||
        INSTALL_ACTION_OFFSET + sizeof(action) > page_size)
        fatal_message("recursive injected mapping layout is invalid");
    result = remote_syscall(pid, SYS_mmap, 0, page_size * 2,
                            PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, UINT64_MAX, 0);
    if (result < 0 && result >= -4095) {
        errno = (int)-result;
        fatal("remote mmap recursive handler");
    }
    remote_code = (uintptr_t)result;
    remote_state = remote_code + page_size;
    blob = prepare_blob(remote_state, getpid());
    remote_write(pid, remote_code, blob, blob_size);
    free(blob);
    action = make_install_action(remote_code);
    remote_write(pid, remote_state + INSTALL_ACTION_OFFSET, &action,
                 sizeof(action));
    result = remote_syscall(pid, SYS_rt_sigaction, SIGSYS,
                            remote_state + INSTALL_ACTION_OFFSET, 0, 8, 0,
                            GATEWAY_MAGIC);
    if (result != 0) {
        if (result < 0 && result >= -4095)
            errno = (int)-result;
        fatal("remote rt_sigaction recursive handler");
    }
    result = remote_syscall(pid, SYS_mprotect, remote_code, page_size,
                            PROT_READ | PROT_EXEC, 0, 0, 0);
    if (result != 0) {
        if (result < 0 && result >= -4095)
            errno = (int)-result;
        fatal("remote mprotect recursive handler");
    }
}

static void record_root_exit(struct bridge_state *bridge, pid_t pid, int status)
{
    if (pid == bridge->root_pid) {
        atomic_store(&bridge->root_status, status);
        atomic_store(&bridge->root_reaped, true);
    }
}

static pid_t wait_for_tracee(struct bridge_state *bridge, int *status)
{
    for (;;) {
        pid_t stopped = waitpid(-1, status, __WALL);
        if (stopped < 0)
            fatal("waitpid ptrace bridge");
        if (WIFEXITED(*status) || WIFSIGNALED(*status)) {
            record_root_exit(bridge, stopped, *status);
            continue;
        }
        if (WIFSTOPPED(*status))
            return stopped;
    }
}

static void release_exec_handler(pid_t tid, uintptr_t flag_address)
{
    if (ptrace(PTRACE_POKEDATA, tid, (void *)flag_address, (void *)1UL) < 0)
        fatal("PTRACE_POKEDATA exec release flag");
}

static void finish_exec_syscall(struct bridge_state *bridge, pid_t pid)
{
    int status;
    pid_t stopped;

    if (ptrace(PTRACE_SYSCALL, pid, NULL, NULL) < 0)
        fatal("PTRACE_SYSCALL after recursive exec event");
    stopped = wait_for_tracee(bridge, &status);
    if (stopped != pid || !WIFSTOPPED(status) ||
        WSTOPSIG(status) != (SIGTRAP | 0x80) ||
        (unsigned int)status >> 16 != 0)
        fatal_message("recursive exec did not reach syscall-exit stop");
}

static void print_exec_path(pid_t pid, unsigned int count, pid_t former_tid)
{
    char proc_path[64];
    char executable[4096];
    char command_line[4096];
    char descriptor_target[4096];
    int command_line_fd;
    ssize_t command_line_length;
    ssize_t length;

    snprintf(proc_path, sizeof(proc_path), "/proc/%d/exe", pid);
    length = readlink(proc_path, executable, sizeof(executable) - 1);
    if (length < 0) {
        snprintf(executable, sizeof(executable), "<readlink failed: %s>",
                 strerror(errno));
    } else {
        executable[length] = '\0';
    }
    printf("bridge: injected exec #%u pid=%d former_tid=%d exe=%s\n", count,
           pid, former_tid, executable);
    snprintf(proc_path, sizeof(proc_path), "/proc/%d/cmdline", pid);
    command_line_fd = open(proc_path, O_RDONLY | O_CLOEXEC);
    if (command_line_fd >= 0) {
        command_line_length =
            read(command_line_fd, command_line, sizeof(command_line) - 1);
        close(command_line_fd);
        if (command_line_length > 0) {
            for (ssize_t index = 0; index < command_line_length; index++) {
                if (command_line[index] == '\0')
                    command_line[index] = ' ';
            }
            command_line[command_line_length] = '\0';
            printf("bridge: exec argv #%u %s\n", count, command_line);
            if (strstr(command_line, "--type=zygote") != NULL) {
                snprintf(proc_path, sizeof(proc_path), "/proc/%d/fd/3", pid);
                length = readlink(proc_path, descriptor_target,
                                  sizeof(descriptor_target) - 1);
                if (length >= 0) {
                    descriptor_target[length] = '\0';
                    printf("bridge: zygote fd 3=%s\n", descriptor_target);
                } else {
                    printf("bridge: zygote fd 3=<readlink failed: %s>\n",
                           strerror(errno));
                }
            }
        }
    }
    fflush(stdout);
}

static void handle_exec_request(struct bridge_state *bridge,
                                const siginfo_t *request)
{
    const unsigned long options =
        PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL | PTRACE_O_TRACESYSGOOD;
    const pid_t requesting_tid = request->si_pid;
    const uintptr_t flag_address =
        (uintptr_t)request->si_value.sival_ptr;
    pid_t tracee;
    bool saw_exec_entry = false;
    int status;

    if (requesting_tid <= 0 || flag_address == 0)
        fatal_message("bridge received malformed exec request");
    if (ptrace(PTRACE_SEIZE, requesting_tid, NULL, (void *)options) < 0)
        fatal("PTRACE_SEIZE exec requester");
    if (ptrace(PTRACE_INTERRUPT, requesting_tid, NULL, NULL) < 0)
        fatal("PTRACE_INTERRUPT exec requester");
    tracee = wait_for_tracee(bridge, &status);
    if (tracee != requesting_tid || !WIFSTOPPED(status))
        fatal_message("unexpected initial ptrace bridge stop");
    release_exec_handler(tracee, flag_address);
    if (ptrace(PTRACE_SYSCALL, tracee, NULL, NULL) < 0)
        fatal("PTRACE_SYSCALL release exec requester");

    for (;;) {
        unsigned int event;
        pid_t stopped = wait_for_tracee(bridge, &status);
        struct user_regs_struct regs;

        event = (unsigned int)status >> 16;
        if (event == PTRACE_EVENT_EXEC) {
            unsigned long former_tid = 0;
            unsigned int count;

            if (ptrace(PTRACE_GETEVENTMSG, stopped, NULL, &former_tid) < 0)
                fatal("PTRACE_GETEVENTMSG recursive exec");
            finish_exec_syscall(bridge, stopped);
            inject_handler(stopped);
            count = atomic_fetch_add(&bridge->exec_count, 1) + 1;
            print_exec_path(stopped, count, (pid_t)former_tid);
            if (ptrace(PTRACE_DETACH, stopped, NULL, NULL) < 0)
                fatal("PTRACE_DETACH recursive exec");
            return;
        }
        if (WSTOPSIG(status) == (SIGTRAP | 0x80)) {
            long result;
            long number;

            if (ptrace_get_regs(stopped, &regs) < 0)
                fatal("PTRACE_GETREGSET recursive syscall stop");
            number = (long)regs.orig_rax;
            result = (long)regs.rax;
            if (number == SYS_execve || number == SYS_execveat) {
                if (result == -ENOSYS) {
                    saw_exec_entry = true;
                } else if (saw_exec_entry) {
                    atomic_fetch_add(&bridge->failed_exec_count, 1);
                    if (ptrace(PTRACE_DETACH, stopped, NULL, NULL) < 0)
                        fatal("PTRACE_DETACH failed exec");
                    return;
                }
            }
            if (ptrace(PTRACE_SYSCALL, stopped, NULL, NULL) < 0)
                fatal("PTRACE_SYSCALL recursive bridge loop");
            continue;
        }
        if (ptrace(PTRACE_SYSCALL, stopped, NULL,
                   (void *)(uintptr_t)WSTOPSIG(status)) < 0)
            fatal("PTRACE_SYSCALL forward bridge signal");
    }
}

static void *bridge_thread_main(void *opaque)
{
    struct bridge_state *bridge = opaque;
    sigset_t signal_set;

    sigemptyset(&signal_set);
    sigaddset(&signal_set, BRIDGE_SIGNAL);
    for (;;) {
        siginfo_t request;
        int signal_number = sigwaitinfo(&signal_set, &request);

        if (signal_number < 0) {
            if (errno == EINTR)
                continue;
            fatal("sigwaitinfo ptrace bridge");
        }
        if (atomic_load(&bridge->stopping) && request.si_code != SI_QUEUE)
            return NULL;
        if (request.si_code != SI_QUEUE)
            continue;
        handle_exec_request(bridge, &request);
    }
}

static void child_main(char **command)
{
    if (setpgid(0, 0) != 0)
        fatal("setpgid child");
    if (prctl(PR_SET_PDEATHSIG, SIGKILL) != 0)
        fatal("PR_SET_PDEATHSIG");
    install_initial_handler();
    install_filter();
    execvpe(command[0], command, environ);
    fatal("execvpe target");
}

int main(int argc, char **argv)
{
    struct bridge_state bridge;
    pthread_t bridge_thread;
    sigset_t signal_set;
    pid_t child;
    int pidfd;
    int status;
    int exit_code;

    if (argc < 2) {
        fprintf(stderr, "usage: %s command [arg ...]\n", argv[0]);
        return EXIT_FAILURE;
    }
    sigemptyset(&signal_set);
    sigaddset(&signal_set, BRIDGE_SIGNAL);
    if (pthread_sigmask(SIG_BLOCK, &signal_set, NULL) != 0)
        fatal("pthread_sigmask bridge signal");
    child = fork();
    if (child < 0)
        fatal("fork recursive target");
    if (child == 0)
        child_main(&argv[1]);

    cleanup_process_group = child;
    if (atexit(cleanup_children) != 0)
        fatal_message("atexit cleanup registration failed");
    memset(&bridge, 0, sizeof(bridge));
    bridge.root_pid = child;
    atomic_init(&bridge.stopping, false);
    atomic_init(&bridge.root_reaped, false);
    atomic_init(&bridge.root_status, 0);
    atomic_init(&bridge.exec_count, 0);
    atomic_init(&bridge.failed_exec_count, 0);
    if (pthread_create(&bridge_thread, NULL, bridge_thread_main, &bridge) != 0)
        fatal("pthread_create ptrace bridge");
    pidfd = (int)syscall(SYS_pidfd_open, child, 0);
    if (pidfd < 0)
        fatal("pidfd_open root target");
    for (;;) {
        struct pollfd descriptor = {.fd = pidfd, .events = POLLIN};
        int poll_result = poll(&descriptor, 1, -1);
        if (poll_result < 0 && errno == EINTR)
            continue;
        if (poll_result < 0)
            fatal("poll root pidfd");
        break;
    }
    close(pidfd);
    atomic_store(&bridge.stopping, true);
    if (pthread_kill(bridge_thread, BRIDGE_SIGNAL) != 0)
        fatal("pthread_kill bridge shutdown");
    if (pthread_join(bridge_thread, NULL) != 0)
        fatal("pthread_join ptrace bridge");
    if (atomic_load(&bridge.root_reaped)) {
        status = atomic_load(&bridge.root_status);
    } else if (waitpid(child, &status, 0) < 0) {
        fatal("waitpid root target");
    }
    cleanup_process_group = -1;
    printf("bridge: summary injected_execs=%u failed_execs=%u\n",
           atomic_load(&bridge.exec_count),
           atomic_load(&bridge.failed_exec_count));
    if (WIFEXITED(status)) {
        exit_code = WEXITSTATUS(status);
        printf("bridge: target exit status %d\n", exit_code);
        return exit_code;
    }
    if (WIFSIGNALED(status))
        fprintf(stderr, "bridge: target died from signal %d\n",
                WTERMSIG(status));
    else
        fprintf(stderr, "bridge: unexpected target status %#x\n", status);
    return EXIT_FAILURE;
}
