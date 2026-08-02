#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

#if !defined(__aarch64__) && !defined(__x86_64__)
#error "This prototype supports native AArch64 and x86-64 only"
#endif

#define TRAPPED_RESULT UINT64_C(0x51515151)
#define ARRAY_LEN(values) (sizeof(values) / sizeof((values)[0]))

/*
 * This entire section is copied into the post-exec address space. It must be
 * position independent and must not reference the injector's GOT, PLT, TLS,
 * stack protector, or libc.
 *
 * The ucontext return-register offset is patched in the local copy before it
 * is written to the tracee. On x86-64 the raw kernel sigaction also points at
 * the copied rt_sigreturn restorer.
 */
#if defined(__aarch64__)
__asm__(
    ".pushsection .text.fspy_injected,\"ax\",@progbits\n"
    ".balign 16\n"
    ".global fspy_blob_start\n"
    ".global fspy_handler\n"
    ".global fspy_return_offset_slot\n"
    ".global fspy_blob_end\n"
    "fspy_blob_start:\n"
    "fspy_handler:\n"
    "  adr x3, fspy_return_offset_slot\n"
    "  ldr x3, [x3]\n"
    "  movz x4, #0x5151\n"
    "  movk x4, #0x5151, lsl #16\n"
    "  str x4, [x2, x3]\n"
    "  ret\n"
    ".balign 8\n"
    "fspy_return_offset_slot:\n"
    "  .quad 0\n"
    "fspy_blob_end:\n"
    ".popsection\n");
#elif defined(__x86_64__)
__asm__(
    ".pushsection .text.fspy_injected,\"ax\",@progbits\n"
    ".balign 16\n"
    ".global fspy_blob_start\n"
    ".global fspy_handler\n"
    ".global fspy_restorer\n"
    ".global fspy_return_offset_slot\n"
    ".global fspy_blob_end\n"
    "fspy_blob_start:\n"
    "fspy_handler:\n"
    "  lea fspy_return_offset_slot(%rip), %rcx\n"
    "  mov (%rcx), %rcx\n"
    "  mov $0x51515151, %eax\n"
    "  mov %rax, (%rdx,%rcx)\n"
    "  ret\n"
    ".balign 8\n"
    "fspy_restorer:\n"
    "  mov $15, %rax\n" /* __NR_rt_sigreturn */
    "  syscall\n"
    "  ud2\n"
    ".balign 8\n"
    "fspy_return_offset_slot:\n"
    "  .quad 0\n"
    "fspy_blob_end:\n"
    ".popsection\n");
#endif

extern const unsigned char fspy_blob_start[];
extern const unsigned char fspy_handler[];
extern const unsigned char fspy_return_offset_slot[];
extern const unsigned char fspy_blob_end[];
#if defined(__x86_64__)
extern const unsigned char fspy_restorer[];
#endif

struct kernel_sigaction_wire {
    uint64_t handler;
    uint64_t flags;
    uint64_t restorer;
    uint64_t mask;
};

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

static int ptrace_get_regs(pid_t pid, struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_GETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov);
}

static int ptrace_set_regs(pid_t pid, const struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = (void *)regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_SETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov);
}

static uintptr_t regs_pc(const struct user_regs_struct *regs)
{
#if defined(__aarch64__)
    return (uintptr_t)regs->pc;
#else
    return (uintptr_t)regs->rip;
#endif
}

static void prepare_remote_syscall(struct user_regs_struct *regs, long number,
                                   const uint64_t args[6])
{
#if defined(__aarch64__)
    regs->regs[0] = args[0];
    regs->regs[1] = args[1];
    regs->regs[2] = args[2];
    regs->regs[3] = args[3];
    regs->regs[4] = args[4];
    regs->regs[5] = args[5];
    regs->regs[8] = (uint64_t)number;
#else
    regs->rax = (uint64_t)number;
    regs->orig_rax = UINT64_MAX;
    regs->rdi = args[0];
    regs->rsi = args[1];
    regs->rdx = args[2];
    regs->r10 = args[3];
    regs->r8 = args[4];
    regs->r9 = args[5];
#endif
}

static long remote_syscall_result(const struct user_regs_struct *regs)
{
#if defined(__aarch64__)
    return (long)regs->regs[0];
#else
    return (long)regs->rax;
#endif
}

static unsigned long syscall_breakpoint_word(unsigned long original)
{
#if defined(__aarch64__)
    (void)original;
    /* svc #0; brk #0, in little-endian instruction order. */
    return UINT64_C(0xd4200000d4000001);
#else
    unsigned long patched = original;
    unsigned char *bytes = (unsigned char *)&patched;
    bytes[0] = 0x0f; /* syscall */
    bytes[1] = 0x05;
    bytes[2] = 0xcc; /* int3 */
    return patched;
#endif
}

static int wait_for_injected_breakpoint(pid_t pid)
{
    int status;
    if (waitpid(pid, &status, 0) < 0)
        return -1;
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        fprintf(stderr, "unexpected status during remote syscall: 0x%x\n", status);
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
    const uint64_t args[6] = {a0, a1, a2, a3, a4, a5};
    unsigned long original_word;
    uintptr_t pc;
    long result;

    if (ptrace_get_regs(pid, &saved) < 0)
        fatal("PTRACE_GETREGSET");
    call_regs = saved;
    pc = regs_pc(&saved);

    errno = 0;
    original_word = (unsigned long)ptrace(PTRACE_PEEKTEXT, pid, (void *)pc, NULL);
    if (original_word == (unsigned long)-1 && errno != 0)
        fatal("PTRACE_PEEKTEXT");

    if (ptrace(PTRACE_POKETEXT, pid, (void *)pc,
               (void *)syscall_breakpoint_word(original_word)) < 0)
        fatal("PTRACE_POKETEXT syscall stub");

    prepare_remote_syscall(&call_regs, number, args);
    if (ptrace_set_regs(pid, &call_regs) < 0)
        fatal("PTRACE_SETREGSET syscall arguments");
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) < 0)
        fatal("PTRACE_CONT remote syscall");
    if (wait_for_injected_breakpoint(pid) < 0)
        fatal("waitpid remote syscall");
    if (ptrace_get_regs(pid, &stopped) < 0)
        fatal("PTRACE_GETREGSET result");
    result = remote_syscall_result(&stopped);

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
            fatal("PTRACE_POKEDATA");
        offset += chunk;
    }
}

static size_t return_register_offset(void)
{
#if defined(__aarch64__)
    return offsetof(ucontext_t, uc_mcontext) + offsetof(mcontext_t, regs[0]);
#else
    return offsetof(ucontext_t, uc_mcontext) +
           offsetof(mcontext_t, gregs[REG_RAX]);
#endif
}

static void inject_sigsys_handler(pid_t pid)
{
    const size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    const size_t blob_size = (size_t)(fspy_blob_end - fspy_blob_start);
    const size_t offset_slot =
        (size_t)(fspy_return_offset_slot - fspy_blob_start);
    const size_t handler_offset = (size_t)(fspy_handler - fspy_blob_start);
    const size_t action_offset = (blob_size + 15U) & ~((size_t)15U);
    unsigned char *local_blob;
    struct kernel_sigaction_wire action = {0};
    uintptr_t remote_page;
    long result;
    uint64_t context_offset;

    if (page_size == 0 || action_offset + sizeof(action) > page_size)
        fatal_message("injected blob does not fit in one page");

    result = remote_syscall(pid, SYS_mmap, 0, page_size, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, UINT64_MAX, 0);
    if (result < 0 && result >= -4095) {
        errno = (int)-result;
        fatal("remote mmap");
    }
    remote_page = (uintptr_t)result;

    local_blob = malloc(blob_size);
    if (local_blob == NULL)
        fatal("malloc local blob");
    memcpy(local_blob, fspy_blob_start, blob_size);
    context_offset = return_register_offset();
    memcpy(local_blob + offset_slot, &context_offset, sizeof(context_offset));
    remote_write(pid, remote_page, local_blob, blob_size);
    free(local_blob);

    action.handler = remote_page + handler_offset;
    action.flags = SA_SIGINFO;
#if defined(__x86_64__)
    action.flags |= 0x04000000UL; /* SA_RESTORER from Linux UAPI. */
    action.restorer = remote_page + (uintptr_t)(fspy_restorer - fspy_blob_start);
#endif
    remote_write(pid, remote_page + action_offset, &action, sizeof(action));

    result = remote_syscall(pid, SYS_rt_sigaction, SIGSYS,
                            remote_page + action_offset, 0, 8, 0, 0);
    if (result != 0) {
        if (result < 0 && result >= -4095)
            errno = (int)-result;
        fatal("remote rt_sigaction");
    }

    result = remote_syscall(pid, SYS_mprotect, remote_page, page_size,
                            PROT_READ | PROT_EXEC, 0, 0, 0);
    if (result != 0) {
        if (result < 0 && result >= -4095)
            errno = (int)-result;
        fatal("remote mprotect");
    }

    printf("injector: mapped handler at %#lx, handler=%#llx, blob=%zu bytes\n",
           (unsigned long)remote_page, (unsigned long long)action.handler, blob_size);
}

static void install_trap_filter(void)
{
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | 0x4653),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)ARRAY_LEN(instructions),
        .filter = instructions,
    };

    if (syscall(SYS_prctl, 38 /* PR_SET_NO_NEW_PRIVS */, 1, 0, 0, 0) != 0)
        fatal("PR_SET_NO_NEW_PRIVS");
    if (syscall(SYS_prctl, 22 /* PR_SET_SECCOMP */, SECCOMP_MODE_FILTER,
                &program, 0, 0) != 0)
        fatal("PR_SET_SECCOMP");
}

static void child_main(const char *target)
{
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) < 0)
        fatal("PTRACE_TRACEME");
    if (raise(SIGSTOP) != 0)
        fatal("raise SIGSTOP");

    install_trap_filter();
    execl(target, target, NULL);
    fatal("execl target");
}

static void wait_for_exec_stop(pid_t child)
{
    int status;

    for (;;) {
        if (waitpid(child, &status, 0) < 0)
            fatal("waitpid exec");
        if (WIFEXITED(status) || WIFSIGNALED(status))
            fatal_message("child exited before PTRACE_EVENT_EXEC");
        if (!WIFSTOPPED(status))
            continue;

        if ((unsigned int)status >> 16 == PTRACE_EVENT_EXEC)
            return;

        if (ptrace(PTRACE_CONT, child, NULL,
                   (void *)(uintptr_t)WSTOPSIG(status)) < 0)
            fatal("PTRACE_CONT forwarding signal");
    }
}

int main(int argc, char **argv)
{
    pid_t child;
    int status;
    unsigned long options = PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL;

    if (argc != 2) {
        fprintf(stderr, "usage: %s /absolute/path/to/target\n", argv[0]);
        return EXIT_FAILURE;
    }

    child = fork();
    if (child < 0)
        fatal("fork");
    if (child == 0)
        child_main(argv[1]);

    if (waitpid(child, &status, 0) < 0)
        fatal("waitpid initial stop");
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP)
        fatal_message("child did not enter its initial SIGSTOP");

    if (ptrace(PTRACE_SETOPTIONS, child, NULL, (void *)options) < 0)
        fatal("PTRACE_SETOPTIONS");
    if (ptrace(PTRACE_CONT, child, NULL, NULL) < 0)
        fatal("PTRACE_CONT to exec");

    wait_for_exec_stop(child);
    puts("injector: caught PTRACE_EVENT_EXEC before target entry");
    inject_sigsys_handler(child);

    if (ptrace(PTRACE_DETACH, child, NULL, NULL) < 0)
        fatal("PTRACE_DETACH");
    puts("injector: detached; target's trapped syscall now has no tracer");
    fflush(stdout);

    if (waitpid(child, &status, 0) < 0)
        fatal("waitpid target");
    if (!WIFEXITED(status)) {
        if (WIFSIGNALED(status))
            fprintf(stderr, "target died from signal %d\n", WTERMSIG(status));
        else
            fprintf(stderr, "unexpected final status: 0x%x\n", status);
        return EXIT_FAILURE;
    }
    printf("injector: target exit status %d\n", WEXITSTATUS(status));
    return WEXITSTATUS(status) == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
