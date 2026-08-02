#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
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
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

#if !defined(__aarch64__)
#error "The bounded esbuild experiment is AArch64-only"
#endif

#define ARRAY_LEN(values) (sizeof(values) / sizeof((values)[0]))
#define GATEWAY_MAGIC UINT64_C(0xf5f05ec0dec0de55)
#define GATEWAY_MAGIC_LOW UINT32_C(0xdec0de55)
#define GATEWAY_MAGIC_HIGH UINT32_C(0xf5f05ec0)
#define SIGSYS_MASK_BIT UINT64_C(0x40000000)

/*
 * Freestanding PIC handler copied into the post-exec image.
 *
 * x9 points at saved x0 in ucontext. The filter traps getpid, openat,
 * rt_sigaction, and rt_sigprocmask. The handler reissues the first two with a
 * sixth-argument marker. It keeps the physical fspy SIGSYS handler installed,
 * shadows the target's logical SIGSYS action, and strips SIGSYS from masks that
 * reach the kernel.
 *
 * This intentionally mutates target action/mask words around the gateway
 * syscall and uses an RWX page. Those shortcuts bound the compatibility
 * experiment; they are not production design choices.
 */
__asm__(
    ".pushsection .text.fspy_esbuild,\"ax\",@progbits\n"
    ".balign 16\n"
    ".global esbuild_blob_start\n"
    ".global esbuild_handler\n"
    ".global esbuild_return_offset_slot\n"
    ".global esbuild_blob_end\n"
    "esbuild_blob_start:\n"
    "esbuild_handler:\n"
    "  adr x9, esbuild_return_offset_slot\n"
    "  ldr x9, [x9]\n"
    "  add x9, x2, x9\n"
    "  ldr x10, [x9, #64]\n" /* saved x8 / syscall number */
    "  cmp x10, #172\n"       /* getpid */
    "  b.eq 1f\n"
    "  cmp x10, #56\n"        /* openat */
    "  b.eq 1f\n"
    "  cmp x10, #134\n"       /* rt_sigaction */
    "  b.eq 2f\n"
    "  cmp x10, #135\n"       /* rt_sigprocmask */
    "  b.eq 5f\n"
    "  mov x0, #-38\n"        /* -ENOSYS */
    "  str x0, [x9]\n"
    "  ret\n"

    /* Reissue getpid/openat using x5 as the seccomp gateway marker. */
    "1:\n"
    "  ldp x0, x1, [x9, #0]\n"
    "  ldp x2, x3, [x9, #16]\n"
    "  ldr x4, [x9, #32]\n"
    "  movz x5, #0xde55\n"
    "  movk x5, #0xdec0, lsl #16\n"
    "  movk x5, #0x5ec0, lsl #32\n"
    "  movk x5, #0xf5f0, lsl #48\n"
    "  mov x8, x10\n"
    "  svc #0\n"
    "  str x0, [x9]\n"
    "  ret\n"

    /* rt_sigaction: shadow SIGSYS; sanitize every other action mask. */
    "2:\n"
    "  ldr x11, [x9, #0]\n"  /* signum */
    "  cmp x11, #31\n"
    "  b.eq 4f\n"
    "  ldr x12, [x9, #8]\n"  /* new action */
    "  cbz x12, 3f\n"
    "  ldr x13, [x12, #24]\n"
    "  mov x14, #0x40000000\n"
    "  bic x15, x13, x14\n"
    "  str x15, [x12, #24]\n"
    "3:\n"
    "  ldp x0, x1, [x9, #0]\n"
    "  ldp x2, x3, [x9, #16]\n"
    "  movz x5, #0xde55\n"
    "  movk x5, #0xdec0, lsl #16\n"
    "  movk x5, #0x5ec0, lsl #32\n"
    "  movk x5, #0xf5f0, lsl #48\n"
    "  mov x8, #134\n"
    "  svc #0\n"
    "  cbz x12, 30f\n"
    "  str x13, [x12, #24]\n"
    "30:\n"
    "  str x0, [x9]\n"
    "  ret\n"

    /* Logical rt_sigaction(SIGSYS): copy a four-word kernel action. */
    "4:\n"
    "  adr x12, esbuild_sigsys_shadow\n"
    "  ldr x13, [x9, #16]\n" /* old action */
    "  cbz x13, 40f\n"
    "  ldp x14, x15, [x12, #0]\n"
    "  stp x14, x15, [x13, #0]\n"
    "  ldp x14, x15, [x12, #16]\n"
    "  stp x14, x15, [x13, #16]\n"
    "40:\n"
    "  ldr x13, [x9, #8]\n"  /* new action */
    "  cbz x13, 41f\n"
    "  ldp x14, x15, [x13, #0]\n"
    "  stp x14, x15, [x12, #0]\n"
    "  ldp x14, x15, [x13, #16]\n"
    "  stp x14, x15, [x12, #16]\n"
    "41:\n"
    "  str xzr, [x9]\n"
    "  ret\n"

    /* rt_sigprocmask: temporarily clear SIGSYS in the supplied set. */
    "5:\n"
    "  ldr x12, [x9, #8]\n"  /* new set */
    "  cbz x12, 6f\n"
    "  ldr x13, [x12]\n"
    "  mov x14, #0x40000000\n"
    "  bic x15, x13, x14\n"
    "  str x15, [x12]\n"
    "6:\n"
    "  ldp x0, x1, [x9, #0]\n"
    "  ldp x2, x3, [x9, #16]\n"
    "  movz x5, #0xde55\n"
    "  movk x5, #0xdec0, lsl #16\n"
    "  movk x5, #0x5ec0, lsl #32\n"
    "  movk x5, #0xf5f0, lsl #48\n"
    "  mov x8, #135\n"
    "  svc #0\n"
    "  cbz x12, 60f\n"
    "  str x13, [x12]\n"
    "60:\n"
    "  str x0, [x9]\n"
    "  ret\n"

    ".balign 8\n"
    "esbuild_sigsys_shadow:\n"
    "  .quad 0, 0, 0, 0\n"
    "esbuild_return_offset_slot:\n"
    "  .quad 0\n"
    "esbuild_blob_end:\n"
    ".popsection\n");

extern const unsigned char esbuild_blob_start[];
extern const unsigned char esbuild_handler[];
extern const unsigned char esbuild_return_offset_slot[];
extern const unsigned char esbuild_blob_end[];
extern char **environ;

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
    fprintf(stderr, "%s\n", message);
    exit(EXIT_FAILURE);
}

static int get_regs(pid_t pid, struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_GETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov);
}

static int set_regs(pid_t pid, const struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = (void *)regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_SETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov);
}

static int wait_for_trap(pid_t pid)
{
    int status;
    if (waitpid(pid, &status, 0) < 0)
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
    struct user_regs_struct call;
    struct user_regs_struct stopped;
    unsigned long original;
    const unsigned long stub = UINT64_C(0xd4200000d4000001); /* svc; brk */
    long result;

    if (get_regs(pid, &saved) < 0)
        fatal("PTRACE_GETREGSET");
    call = saved;
    errno = 0;
    original = (unsigned long)ptrace(PTRACE_PEEKTEXT, pid, (void *)saved.pc, NULL);
    if (original == (unsigned long)-1 && errno != 0)
        fatal("PTRACE_PEEKTEXT");
    if (ptrace(PTRACE_POKETEXT, pid, (void *)saved.pc, (void *)stub) < 0)
        fatal("PTRACE_POKETEXT stub");

    call.regs[0] = a0;
    call.regs[1] = a1;
    call.regs[2] = a2;
    call.regs[3] = a3;
    call.regs[4] = a4;
    call.regs[5] = a5;
    call.regs[8] = (uint64_t)number;
    if (set_regs(pid, &call) < 0)
        fatal("PTRACE_SETREGSET call");
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) < 0)
        fatal("PTRACE_CONT remote syscall");
    if (wait_for_trap(pid) < 0)
        fatal("wait remote syscall");
    if (get_regs(pid, &stopped) < 0)
        fatal("PTRACE_GETREGSET result");
    result = (long)stopped.regs[0];

    if (ptrace(PTRACE_POKETEXT, pid, (void *)saved.pc, (void *)original) < 0)
        fatal("PTRACE_POKETEXT restore");
    if (set_regs(pid, &saved) < 0)
        fatal("PTRACE_SETREGSET restore");
    return result;
}

static void remote_write(pid_t pid, uintptr_t destination, const void *source,
                         size_t length)
{
    const unsigned char *bytes = source;
    for (size_t offset = 0; offset < length; offset += sizeof(unsigned long)) {
        unsigned long word = 0;
        size_t chunk = length - offset;
        if (chunk > sizeof(word))
            chunk = sizeof(word);
        memcpy(&word, bytes + offset, chunk);
        if (ptrace(PTRACE_POKEDATA, pid, (void *)(destination + offset),
                   (void *)word) < 0)
            fatal("PTRACE_POKEDATA");
    }
}

static void inject_handler(pid_t pid)
{
    const size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    const size_t blob_size = (size_t)(esbuild_blob_end - esbuild_blob_start);
    const size_t handler_offset = (size_t)(esbuild_handler - esbuild_blob_start);
    const size_t slot_offset =
        (size_t)(esbuild_return_offset_slot - esbuild_blob_start);
    const size_t action_offset = (blob_size + 15U) & ~((size_t)15U);
    const uint64_t context_offset =
        offsetof(ucontext_t, uc_mcontext) + offsetof(mcontext_t, regs[0]);
    struct kernel_sigaction_wire action = {0};
    unsigned char *blob;
    long result;
    uintptr_t remote_page;

    if (action_offset + sizeof(action) > page_size)
        fatal_message("handler blob is larger than one page");
    result = remote_syscall(pid, SYS_mmap, 0, page_size,
                            PROT_READ | PROT_WRITE | PROT_EXEC,
                            MAP_PRIVATE | MAP_ANONYMOUS, UINT64_MAX, 0);
    if (result < 0 && result >= -4095) {
        errno = (int)-result;
        fatal("remote mmap RWX");
    }
    remote_page = (uintptr_t)result;

    blob = malloc(blob_size);
    if (blob == NULL)
        fatal("malloc blob");
    memcpy(blob, esbuild_blob_start, blob_size);
    memcpy(blob + slot_offset, &context_offset, sizeof(context_offset));
    remote_write(pid, remote_page, blob, blob_size);
    free(blob);

    action.handler = remote_page + handler_offset;
    action.flags = SA_SIGINFO;
    remote_write(pid, remote_page + action_offset, &action, sizeof(action));
    result = remote_syscall(pid, SYS_rt_sigaction, SIGSYS,
                            remote_page + action_offset, 0, 8, 0,
                            GATEWAY_MAGIC);
    if (result != 0) {
        if (result < 0 && result >= -4095)
            errno = (int)-result;
        fatal("remote rt_sigaction");
    }
    printf("esbuild-injector: handler=%#lx blob=%zu bytes (RWX experiment)\n",
           (unsigned long)(remote_page + handler_offset), blob_size);
}

static void install_filter(void)
{
    struct sock_filter insns[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 4, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_openat, 3, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_rt_sigaction, 2, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_rt_sigprocmask, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, args[5])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, GATEWAY_MAGIC_LOW, 0, 3),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, args[5]) + 4),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, GATEWAY_MAGIC_HIGH, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | 0x4653),
    };
    struct sock_fprog program = {
        .len = (unsigned short)ARRAY_LEN(insns),
        .filter = insns,
    };

    if (syscall(SYS_prctl, 38, 1, 0, 0, 0) != 0)
        fatal("PR_SET_NO_NEW_PRIVS");
    if (syscall(SYS_prctl, 22, SECCOMP_MODE_FILTER, &program, 0, 0) != 0)
        fatal("PR_SET_SECCOMP");
}

static void child_main(char **target_argv)
{
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) < 0)
        fatal("PTRACE_TRACEME");
    if (raise(SIGSTOP) != 0)
        fatal("raise SIGSTOP");
    install_filter();
    syscall(SYS_execve, target_argv[0], target_argv, environ);
    fatal("execve target");
}

static void wait_for_exec(pid_t child)
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
            fatal("PTRACE_CONT signal");
    }
}

static uint64_t elapsed_ns(const struct timespec *start, const struct timespec *end)
{
    return (uint64_t)(end->tv_sec - start->tv_sec) * UINT64_C(1000000000) +
           (uint64_t)(end->tv_nsec - start->tv_nsec);
}

int main(int argc, char **argv)
{
    pid_t child;
    int status;
    struct timespec start;
    struct timespec end;

    if (argc < 2) {
        fprintf(stderr, "usage: %s /path/to/esbuild [arguments...]\n", argv[0]);
        return EXIT_FAILURE;
    }
    child = fork();
    if (child < 0)
        fatal("fork");
    if (child == 0)
        child_main(&argv[1]);

    if (waitpid(child, &status, 0) < 0)
        fatal("waitpid initial");
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP)
        fatal_message("child did not stop before filter installation");
    if (ptrace(PTRACE_SETOPTIONS, child, NULL,
               (void *)(PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL)) < 0)
        fatal("PTRACE_SETOPTIONS");
    if (ptrace(PTRACE_CONT, child, NULL, NULL) < 0)
        fatal("PTRACE_CONT exec");

    wait_for_exec(child);
    if (clock_gettime(CLOCK_MONOTONIC_RAW, &start) != 0)
        fatal("clock_gettime start");
    inject_handler(child);
    if (ptrace(PTRACE_DETACH, child, NULL, NULL) < 0)
        fatal("PTRACE_DETACH");
    if (clock_gettime(CLOCK_MONOTONIC_RAW, &end) != 0)
        fatal("clock_gettime end");
    printf("esbuild-injector: exec-stop to detach %.3f us\n",
           (double)elapsed_ns(&start, &end) / 1000.0);
    fflush(stdout);

    if (waitpid(child, &status, 0) < 0)
        fatal("waitpid target");
    if (WIFEXITED(status)) {
        printf("esbuild-injector: target exit=%d\n", WEXITSTATUS(status));
        return WEXITSTATUS(status) == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
    }
    if (WIFSIGNALED(status))
        fprintf(stderr, "esbuild-injector: target signal=%d\n", WTERMSIG(status));
    else
        fprintf(stderr, "esbuild-injector: target status=%#x\n", status);
    return EXIT_FAILURE;
}
