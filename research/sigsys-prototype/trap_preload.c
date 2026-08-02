#define _GNU_SOURCE

#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <ucontext.h>
#include <unistd.h>

#define TRUST_MAGIC UINT64_C(0xf5f05ec0dec0de55)
#define ALTSTACK_SIZE (256U * 1024U)

#if __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
#define U64_LO_OFFSET(field) offsetof(struct seccomp_data, field)
#define U64_HI_OFFSET(field) (offsetof(struct seccomp_data, field) + 4)
#else
#define U64_LO_OFFSET(field) (offsetof(struct seccomp_data, field) + 4)
#define U64_HI_OFFSET(field) offsetof(struct seccomp_data, field)
#endif

#if defined(__aarch64__)
#define EXPECTED_ARCH AUDIT_ARCH_AARCH64
#elif defined(__x86_64__)
#define EXPECTED_ARCH AUDIT_ARCH_X86_64
#else
#error This probe supports AArch64 and x86-64 only
#endif

extern long trusted_syscall6(long number, long arg0, long arg1, long arg2,
                             long arg3, long arg4, long arg5);

#if defined(__aarch64__)
__asm__(
    ".text\n"
    ".balign 16\n"
    ".global trusted_syscall6\n"
    ".type trusted_syscall6, %function\n"
    "trusted_syscall6:\n"
    "  mov x8, x0\n"
    "  mov x0, x1\n"
    "  mov x1, x2\n"
    "  mov x2, x3\n"
    "  mov x3, x4\n"
    "  mov x4, x5\n"
    "  mov x5, x6\n"
    "  svc #0\n"
    "  ret\n"
    ".size trusted_syscall6, .-trusted_syscall6\n");
#elif defined(__x86_64__)
__asm__(
    ".text\n"
    ".balign 16\n"
    ".global trusted_syscall6\n"
    ".type trusted_syscall6, @function\n"
    "trusted_syscall6:\n"
    "  mov %rdi, %rax\n"
    "  mov %rsi, %rdi\n"
    "  mov %rdx, %rsi\n"
    "  mov %rcx, %rdx\n"
    "  mov %r8, %r10\n"
    "  mov %r9, %r8\n"
    "  mov 8(%rsp), %r9\n"
    "  syscall\n"
    "  ret\n"
    ".size trusted_syscall6, .-trusted_syscall6\n");
#endif

struct kernel_sigaction {
    uintptr_t handler;
    unsigned long flags;
    uintptr_t restorer;
    unsigned long mask;
};

static uint64_t filesystem_traps;
static uint64_t getpid_traps;
static uint64_t sigaction_traps;
static uint64_t sigprocmask_traps;
static pid_t real_pid;
static struct kernel_sigaction virtual_sigsys_action;
static unsigned long virtual_sigsys_mask;

static inline int raw_failed(long result)
{
    return result < 0 && result >= -4095;
}

static long safe_copy_from_target(void *local, const void *remote, size_t size)
{
    struct iovec local_iov = {.iov_base = local, .iov_len = size};
    struct iovec remote_iov = {.iov_base = (void *)remote, .iov_len = size};
    return trusted_syscall6(SYS_process_vm_readv, real_pid,
                            (long)&local_iov, 1, (long)&remote_iov, 1, 0);
}

static long safe_copy_to_target(void *remote, const void *local, size_t size)
{
    struct iovec local_iov = {.iov_base = (void *)local, .iov_len = size};
    struct iovec remote_iov = {.iov_base = remote, .iov_len = size};
    return trusted_syscall6(SYS_process_vm_writev, real_pid,
                            (long)&local_iov, 1, (long)&remote_iov, 1, 0);
}

static void set_result(ucontext_t *context, long result)
{
#if defined(__aarch64__)
    context->uc_mcontext.regs[0] = (unsigned long)result;
#else
    context->uc_mcontext.gregs[REG_RAX] = (greg_t)result;
#endif
}

static void get_arguments(ucontext_t *context, long arguments[6])
{
#if defined(__aarch64__)
    for (size_t index = 0; index < 6; ++index)
        arguments[index] = (long)context->uc_mcontext.regs[index];
#else
    arguments[0] = context->uc_mcontext.gregs[REG_RDI];
    arguments[1] = context->uc_mcontext.gregs[REG_RSI];
    arguments[2] = context->uc_mcontext.gregs[REG_RDX];
    arguments[3] = context->uc_mcontext.gregs[REG_R10];
    arguments[4] = context->uc_mcontext.gregs[REG_R8];
    arguments[5] = context->uc_mcontext.gregs[REG_R9];
#endif
}

static void handle_rt_sigaction(ucontext_t *context, const long arguments[6])
{
    struct kernel_sigaction next;
    long result;

    if (arguments[2]) {
        result = safe_copy_to_target((void *)arguments[2],
                                     &virtual_sigsys_action,
                                     sizeof(virtual_sigsys_action));
        if (result != (long)sizeof(virtual_sigsys_action)) {
            set_result(context, -EFAULT);
            return;
        }
    }
    if (arguments[1]) {
        result = safe_copy_from_target(&next, (void *)arguments[1],
                                       sizeof(next));
        if (result != (long)sizeof(next)) {
            set_result(context, -EFAULT);
            return;
        }
        virtual_sigsys_action = next;
    }
    set_result(context, 0);
}

static void handle_rt_sigprocmask(ucontext_t *context, const long arguments[6])
{
    const unsigned long sigsys_bit = 1UL << (SIGSYS - 1);
    unsigned long requested = 0;
    unsigned long sanitized = 0;
    unsigned long actual_old = 0;
    unsigned long visible_old;
    long result;

    if (arguments[3] < (long)sizeof(unsigned long)) {
        set_result(context, -EINVAL);
        return;
    }
    if (arguments[1]) {
        result = safe_copy_from_target(&requested, (void *)arguments[1],
                                       sizeof(requested));
        if (result != (long)sizeof(requested)) {
            set_result(context, -EFAULT);
            return;
        }
        sanitized = requested & ~sigsys_bit;
    }
    result = trusted_syscall6(SYS_rt_sigprocmask, arguments[0],
                              arguments[1] ? (long)&sanitized : 0,
                              (long)&actual_old, arguments[3], 0,
                              (long)TRUST_MAGIC);
    if (raw_failed(result)) {
        set_result(context, result);
        return;
    }
    visible_old = actual_old | virtual_sigsys_mask;
    if (arguments[2]) {
        result = safe_copy_to_target((void *)arguments[2], &visible_old,
                                     sizeof(visible_old));
        if (result != (long)sizeof(visible_old)) {
            set_result(context, -EFAULT);
            return;
        }
    }
    if (arguments[1]) {
        if (arguments[0] == SIG_BLOCK)
            virtual_sigsys_mask |= requested & sigsys_bit;
        else if (arguments[0] == SIG_UNBLOCK)
            virtual_sigsys_mask &= ~(requested & sigsys_bit);
        else if (arguments[0] == SIG_SETMASK)
            virtual_sigsys_mask = requested & sigsys_bit;
        else {
            set_result(context, -EINVAL);
            return;
        }
    }
    set_result(context, 0);
}

static char *append_text(char *cursor, const char *text)
{
    while (*text)
        *cursor++ = *text++;
    return cursor;
}

static char *append_decimal(char *cursor, uint64_t value)
{
    char reversed[32];
    size_t count = 0;
    do {
        reversed[count++] = (char)('0' + value % 10);
        value /= 10;
    } while (value);
    while (count)
        *cursor++ = reversed[--count];
    return cursor;
}

static void write_summary(void)
{
    char buffer[256];
    char *cursor = append_text(buffer, "fspy-sigsys-probe: fs=");
    cursor = append_decimal(cursor, __atomic_load_n(&filesystem_traps,
                                                    __ATOMIC_RELAXED));
    cursor = append_text(cursor, " getpid=");
    cursor = append_decimal(cursor, __atomic_load_n(&getpid_traps,
                                                    __ATOMIC_RELAXED));
    cursor = append_text(cursor, " sigaction=");
    cursor = append_decimal(cursor, __atomic_load_n(&sigaction_traps,
                                                    __ATOMIC_RELAXED));
    cursor = append_text(cursor, " sigprocmask=");
    cursor = append_decimal(cursor, __atomic_load_n(&sigprocmask_traps,
                                                    __ATOMIC_RELAXED));
    *cursor++ = '\n';
    trusted_syscall6(SYS_write, STDERR_FILENO, (long)buffer,
                     cursor - buffer, 0, 0, 0);
}

static void sigsys_handler(int signal_number, siginfo_t *info,
                           void *context_pointer)
{
    ucontext_t *context = context_pointer;
    long arguments[6];
    if (signal_number != SIGSYS)
        trusted_syscall6(SYS_exit_group, 90, 0, 0, 0, 0,
                         (long)TRUST_MAGIC);
    get_arguments(context, arguments);

    if (info->si_syscall == SYS_getpid) {
        __atomic_fetch_add(&getpid_traps, 1, __ATOMIC_RELAXED);
        set_result(context, trusted_syscall6(
            info->si_syscall, arguments[0], arguments[1], arguments[2],
            arguments[3], arguments[4], (long)TRUST_MAGIC));
        return;
    }
    if (info->si_syscall == SYS_rt_sigaction) {
        __atomic_fetch_add(&sigaction_traps, 1, __ATOMIC_RELAXED);
        handle_rt_sigaction(context, arguments);
        return;
    }
    if (info->si_syscall == SYS_rt_sigprocmask) {
        __atomic_fetch_add(&sigprocmask_traps, 1, __ATOMIC_RELAXED);
        handle_rt_sigprocmask(context, arguments);
        return;
    }
    if (info->si_syscall == SYS_exit_group) {
        write_summary();
        trusted_syscall6(SYS_exit_group, arguments[0], 0, 0, 0, 0,
                         (long)TRUST_MAGIC);
        return;
    }

    __atomic_fetch_add(&filesystem_traps, 1, __ATOMIC_RELAXED);
    set_result(context, trusted_syscall6(
        info->si_syscall, arguments[0], arguments[1], arguments[2],
        arguments[3], arguments[4], (long)TRUST_MAGIC));
}

static size_t append_instruction(struct sock_filter *instructions,
                                 size_t *length, unsigned short code,
                                 unsigned char jump_true,
                                 unsigned char jump_false, uint32_t value)
{
    size_t index = (*length)++;
    instructions[index] = (struct sock_filter){
        .code = code,
        .jt = jump_true,
        .jf = jump_false,
        .k = value,
    };
    return index;
}

static int install_filter(void)
{
    struct sock_filter instructions[64];
    size_t length = 0;
    size_t magic_jumps[32];
    size_t magic_jump_count = 0;

    append_instruction(instructions, &length, BPF_LD | BPF_W | BPF_ABS,
                       0, 0, offsetof(struct seccomp_data, arch));
    append_instruction(instructions, &length, BPF_JMP | BPF_JEQ | BPF_K,
                       1, 0, EXPECTED_ARCH);
    append_instruction(instructions, &length, BPF_RET | BPF_K, 0, 0,
                       SECCOMP_RET_KILL_PROCESS);
    append_instruction(instructions, &length, BPF_LD | BPF_W | BPF_ABS,
                       0, 0, offsetof(struct seccomp_data, nr));

#define INTERCEPT(syscall_name)                                                \
    do {                                                                       \
        magic_jumps[magic_jump_count++] = append_instruction(                  \
            instructions, &length, BPF_JMP | BPF_JEQ | BPF_K, 0, 0,           \
            SYS_##syscall_name);                                               \
    } while (0)
    INTERCEPT(getpid);
    INTERCEPT(openat);
#ifdef SYS_openat2
    INTERCEPT(openat2);
#endif
#ifdef SYS_newfstatat
    INTERCEPT(newfstatat);
#elif defined(SYS_fstatat)
    INTERCEPT(fstatat);
#endif
    INTERCEPT(statx);
    INTERCEPT(getdents64);
    INTERCEPT(faccessat);
#ifdef SYS_faccessat2
    INTERCEPT(faccessat2);
#endif
    INTERCEPT(rt_sigprocmask);
    INTERCEPT(exit_group);
#undef INTERCEPT

    size_t sigaction_jump = append_instruction(
        instructions, &length, BPF_JMP | BPF_JEQ | BPF_K, 0, 2,
        SYS_rt_sigaction);
    (void)sigaction_jump;
    append_instruction(instructions, &length, BPF_LD | BPF_W | BPF_ABS,
                       0, 0, U64_LO_OFFSET(args[0]));
    size_t sigsys_jump = append_instruction(
        instructions, &length, BPF_JMP | BPF_JEQ | BPF_K, 0, 0, SIGSYS);
    append_instruction(instructions, &length, BPF_RET | BPF_K, 0, 0,
                       SECCOMP_RET_ALLOW);

    size_t magic_check = length;
    for (size_t index = 0; index < magic_jump_count; ++index)
        instructions[magic_jumps[index]].jt =
            (unsigned char)(magic_check - magic_jumps[index] - 1);
    instructions[sigsys_jump].jt =
        (unsigned char)(magic_check - sigsys_jump - 1);

    append_instruction(instructions, &length, BPF_LD | BPF_W | BPF_ABS,
                       0, 0, U64_HI_OFFSET(args[5]));
    append_instruction(instructions, &length, BPF_JMP | BPF_JEQ | BPF_K,
                       0, 3, (uint32_t)(TRUST_MAGIC >> 32));
    append_instruction(instructions, &length, BPF_LD | BPF_W | BPF_ABS,
                       0, 0, U64_LO_OFFSET(args[5]));
    append_instruction(instructions, &length, BPF_JMP | BPF_JEQ | BPF_K,
                       0, 1, (uint32_t)TRUST_MAGIC);
    append_instruction(instructions, &length, BPF_RET | BPF_K, 0, 0,
                       SECCOMP_RET_ALLOW);
    append_instruction(instructions, &length, BPF_RET | BPF_K, 0, 0,
                       SECCOMP_RET_TRAP);

    struct sock_fprog program = {
        .len = (unsigned short)length,
        .filter = instructions,
    };
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        return -1;
    return (int)syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program);
}

__attribute__((constructor))
static void initialize_probe(void)
{
    unsetenv("LD_PRELOAD");
    real_pid = getpid();
    void *mapping = mmap(NULL, ALTSTACK_SIZE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    if (mapping == MAP_FAILED)
        _exit(80);
    stack_t stack = {.ss_sp = mapping, .ss_size = ALTSTACK_SIZE, .ss_flags = 0};
    if (sigaltstack(&stack, NULL) != 0)
        _exit(81);
    struct sigaction action = {0};
    action.sa_sigaction = sigsys_handler;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_NODEFER;
    if (sigaction(SIGSYS, &action, NULL) != 0)
        _exit(82);
    if (install_filter() != 0)
        _exit(83);
}
