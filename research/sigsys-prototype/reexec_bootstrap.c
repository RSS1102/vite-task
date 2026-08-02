#define _GNU_SOURCE

#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

#define TRUST_MAGIC UINT64_C(0xf5f05ec0dec0de55)

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
#error Unsupported architecture
#endif

extern long trusted_syscall6(long number, long arg0, long arg1, long arg2,
                             long arg3, long arg4, long arg5);

#if defined(__aarch64__)
__asm__(
    ".text\n"
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
    "  ret\n");
#else
__asm__(
    ".text\n"
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
    ".global sigreturn_restorer\n"
    "sigreturn_restorer:\n"
    "  mov $15, %rax\n"
    "  syscall\n");
extern void sigreturn_restorer(void);
#endif

struct kernel_sigaction {
    uintptr_t handler;
    unsigned long flags;
    uintptr_t restorer;
    unsigned long mask;
};

static volatile sig_atomic_t trap_count;

static void handler(int signal_number, siginfo_t *info, void *opaque)
{
    ucontext_t *context = opaque;
    if (signal_number != SIGSYS || info->si_syscall != SYS_getpid)
        trusted_syscall6(SYS_exit_group, 90, 0, 0, 0, 0, 0);
    ++trap_count;
#if defined(__aarch64__)
    context->uc_mcontext.regs[0] = 424242;
#else
    context->uc_mcontext.gregs[REG_RAX] = 424242;
#endif
}

static long direct_getpid(void)
{
    long result;
#if defined(__aarch64__)
    register long number __asm__("x8") = SYS_getpid;
    register long sixth __asm__("x5") = 0;
    register long returned __asm__("x0");
    __asm__ volatile("svc #0" : "=r"(returned)
                     : "r"(number), "r"(sixth) : "memory", "cc");
    result = returned;
#else
    register long sixth __asm__("r9") = 0;
    __asm__ volatile("syscall" : "=a"(result)
                     : "a"(SYS_getpid), "r"(sixth)
                     : "rcx", "r11", "memory");
#endif
    return result;
}

static int install_filter(void)
{
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, EXPECTED_ARCH, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 3, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_rt_sigaction, 2, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_execve, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, U64_HI_OFFSET(args[5])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K,
                 (uint32_t)(TRUST_MAGIC >> 32), 0, 3),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, U64_LO_OFFSET(args[5])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K,
                 (uint32_t)TRUST_MAGIC, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP),
    };
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof(instructions) / sizeof(instructions[0])),
        .filter = instructions,
    };
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        return -1;
    return (int)syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program);
}

static void install_initial_handler(void)
{
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_sigaction = handler;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_SIGINFO | SA_NODEFER;
    if (sigaction(SIGSYS, &action, NULL) != 0)
        exit(2);
}

static void bootstrap_handler_with_raw_syscall(void)
{
    struct kernel_sigaction action = {
        .handler = (uintptr_t)handler,
        .flags = SA_SIGINFO | SA_NODEFER,
        .restorer = 0,
        .mask = 0,
    };
#if defined(__x86_64__)
    action.flags |= 0x04000000UL; /* SA_RESTORER */
    action.restorer = (uintptr_t)sigreturn_restorer;
#endif
    long result = trusted_syscall6(SYS_rt_sigaction, SIGSYS, (long)&action,
                                   0, sizeof(unsigned long), 0,
                                   (long)TRUST_MAGIC);
    if (result != 0)
        trusted_syscall6(SYS_exit_group, 3, 0, 0, 0, 0, 0);
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "stage2") == 0) {
        bootstrap_handler_with_raw_syscall();
        if (direct_getpid() != 424242 || trap_count != 1)
            return 4;
        static const char passed[] =
            "reexec-bootstrap: handler reinstalled before trapped syscall PASS\n";
        trusted_syscall6(SYS_write, STDERR_FILENO, (long)passed,
                         sizeof(passed) - 1, 0, 0, 0);
        return 0;
    }

    install_initial_handler();
    if (install_filter() != 0)
        return 5;
    char *next_argv[] = {argv[0], "stage2", NULL};
    char *next_env[] = {"PATH=/usr/bin:/bin", NULL};
    long result = trusted_syscall6(SYS_execve, (long)argv[0],
                                   (long)next_argv, (long)next_env,
                                   0, 0, (long)TRUST_MAGIC);
    (void)result;
    return 6;
}
