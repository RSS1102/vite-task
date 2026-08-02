#define _GNU_SOURCE

#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

#define NESTED_TAG 0x1234
#define NESTED_RESULT 0x12345678L
#define SECCOMP_SIGINFO_CODE 1

static volatile sig_atomic_t saw_nested_trap;

static void logical_sigsys_handler(int signal_number, siginfo_t *info,
                                   void *opaque_context)
{
    ucontext_t *context = opaque_context;

    if (signal_number != SIGSYS || info->si_code != SECCOMP_SIGINFO_CODE ||
        info->si_errno != NESTED_TAG || info->si_syscall != SYS_getppid)
        _exit(120);
    saw_nested_trap = 1;
    context->uc_mcontext.gregs[REG_RAX] = NESTED_RESULT;
}

static void install_nested_filter(void)
{
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getppid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | NESTED_TAG),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof(instructions) / sizeof(instructions[0])),
        .filter = instructions,
    };

    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        _exit(121);
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program) != 0)
        _exit(122);
}

int main(void)
{
    struct sigaction action = {0};
    long result;

    sigemptyset(&action.sa_mask);
    action.sa_sigaction = logical_sigsys_handler;
    action.sa_flags = SA_SIGINFO;
    if (sigaction(SIGSYS, &action, NULL) != 0) {
        perror("sigaction logical SIGSYS");
        return EXIT_FAILURE;
    }
    install_nested_filter();
    result = syscall(SYS_getppid);
    printf("nested: logical SIGSYS result=%#lx seen=%d\n", result,
           saw_nested_trap);
    if (result != NESTED_RESULT || !saw_nested_trap)
        return EXIT_FAILURE;
    puts("PASS: foreign seccomp TRAP reached the target's logical handler");
    return EXIT_SUCCESS;
}
