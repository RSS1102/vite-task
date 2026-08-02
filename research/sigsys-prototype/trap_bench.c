#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

/* fspy's intercepted syscalls currently have at most five real arguments. */
#define TRUST_MAGIC UINT64_C(0xf5f05ec0dec0de55)
#define ALTSTACK_SIZE (128U * 1024U)
#define BENCH_ITERS 200000
#define OPEN_BENCH_ITERS 100000
#define THREADS 4
#define THREAD_ITERS 50000

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

static _Atomic uint64_t total_traps;
static _Atomic uint64_t getpid_traps;
static _Atomic uint64_t openat_traps;
static _Atomic uint64_t sigaction_traps;
static _Atomic uint64_t sigprocmask_traps;
static _Atomic uint64_t handlers_on_altstack;
static _Atomic uint64_t handlers_on_normal_stack;
static _Atomic uint64_t lazy_altstacks_installed;
static volatile sig_atomic_t getpid_passthrough;
static volatile sig_atomic_t altstack_probe_enabled;
static volatile sig_atomic_t nested_test_pending;
static volatile sig_atomic_t nested_test_result;
static pid_t real_pid;
static struct kernel_sigaction virtual_sigsys_action;
static unsigned long virtual_sigsys_mask;

static long payload_direct_getpid(void);

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

static int bytes_equal(const char *left, const char *right)
{
    size_t index = 0;

    for (;;) {
        if (left[index] != right[index])
            return 0;
        if (left[index] == '\0')
            return 1;
        ++index;
    }
}

static void set_result(ucontext_t *context, long result)
{
#if defined(__aarch64__)
    context->uc_mcontext.regs[0] = (unsigned long)result;
#elif defined(__x86_64__)
    context->uc_mcontext.gregs[REG_RAX] = (greg_t)result;
#endif
}

static void get_arguments(ucontext_t *context, long arguments[6])
{
#if defined(__aarch64__)
    for (size_t index = 0; index < 6; ++index)
        arguments[index] = (long)context->uc_mcontext.regs[index];
#elif defined(__x86_64__)
    arguments[0] = context->uc_mcontext.gregs[REG_RDI];
    arguments[1] = context->uc_mcontext.gregs[REG_RSI];
    arguments[2] = context->uc_mcontext.gregs[REG_RDX];
    arguments[3] = context->uc_mcontext.gregs[REG_R10];
    arguments[4] = context->uc_mcontext.gregs[REG_R8];
    arguments[5] = context->uc_mcontext.gregs[REG_R9];
#endif
}

/*
 * New threads start with their alternate signal stack disabled. The first
 * trapped syscall safely arrives on the target stack, installs a private
 * stack with raw syscalls, and edits uc_stack so rt_sigreturn preserves it.
 */
static void ensure_thread_altstack(ucontext_t *context, const void *frame)
{
    stack_t current;
    long result = trusted_syscall6(SYS_sigaltstack, 0, (long)&current,
                                   0, 0, 0, 0);
    uintptr_t frame_address = (uintptr_t)frame;
    int on_altstack = 0;

    if (result == 0 && !(current.ss_flags & SS_DISABLE)) {
        uintptr_t low = (uintptr_t)current.ss_sp;
        uintptr_t high = low + current.ss_size;
        on_altstack = frame_address >= low && frame_address < high;
    }

    if (on_altstack) {
        atomic_fetch_add_explicit(&handlers_on_altstack, 1,
                                  memory_order_relaxed);
        return;
    }

    atomic_fetch_add_explicit(&handlers_on_normal_stack, 1,
                              memory_order_relaxed);
    if (result != 0 || !(current.ss_flags & SS_DISABLE))
        return;

    long mapping = trusted_syscall6(
        SYS_mmap, 0, ALTSTACK_SIZE, PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    if (raw_failed(mapping))
        return;

    stack_t replacement = {
        .ss_sp = (void *)mapping,
        .ss_size = ALTSTACK_SIZE,
        .ss_flags = 0,
    };
    result = trusted_syscall6(SYS_sigaltstack, (long)&replacement, 0,
                              0, 0, 0, 0);
    if (result == 0) {
        context->uc_stack = replacement;
        atomic_fetch_add_explicit(&lazy_altstacks_installed, 1,
                                  memory_order_relaxed);
    }
}

static void handle_openat(ucontext_t *context, const long arguments[6])
{
    static const char virtual_path[] = "/virtual/fspy-hostname";
    static const char real_path[] = "/etc/hostname";
    char path[128];
    const char *effective_path = (const char *)arguments[1];
    long copied = safe_copy_from_target(path, effective_path, sizeof(path));

    if (copied > 0) {
        path[sizeof(path) - 1] = '\0';
        if (bytes_equal(path, virtual_path))
            effective_path = real_path;
    }

    long result = trusted_syscall6(SYS_openat, arguments[0],
                                   (long)effective_path, arguments[2],
                                   arguments[3], arguments[4],
                                   (long)TRUST_MAGIC);
    set_result(context, result);
}

static void handle_rt_sigaction(ucontext_t *context, const long arguments[6])
{
    struct kernel_sigaction next;
    long result;

    if (arguments[2] != 0) {
        result = safe_copy_to_target((void *)arguments[2],
                                     &virtual_sigsys_action,
                                     sizeof(virtual_sigsys_action));
        if (result != (long)sizeof(virtual_sigsys_action)) {
            set_result(context, -EFAULT);
            return;
        }
    }

    if (arguments[1] != 0) {
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
    if (arguments[1] != 0) {
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
    if (arguments[2] != 0) {
        result = safe_copy_to_target((void *)arguments[2], &visible_old,
                                     sizeof(visible_old));
        if (result != (long)sizeof(visible_old)) {
            set_result(context, -EFAULT);
            return;
        }
    }

    if (arguments[1] != 0) {
        switch (arguments[0]) {
        case SIG_BLOCK:
            virtual_sigsys_mask |= requested & sigsys_bit;
            break;
        case SIG_UNBLOCK:
            virtual_sigsys_mask &= ~(requested & sigsys_bit);
            break;
        case SIG_SETMASK:
            virtual_sigsys_mask = requested & sigsys_bit;
            break;
        default:
            set_result(context, -EINVAL);
            return;
        }
    }
    set_result(context, 0);
}

static void sigsys_handler(int signal_number, siginfo_t *info,
                           void *context_pointer)
{
    char frame_byte;
    long arguments[6];
    ucontext_t *context = context_pointer;

    if (signal_number != SIGSYS)
        trusted_syscall6(SYS_exit_group, 90, 0, 0, 0, 0, 0);

    if (altstack_probe_enabled)
        ensure_thread_altstack(context, &frame_byte);
    atomic_fetch_add_explicit(&total_traps, 1, memory_order_relaxed);
    get_arguments(context, arguments);

    switch (info->si_syscall) {
    case SYS_getpid:
        atomic_fetch_add_explicit(&getpid_traps, 1, memory_order_relaxed);
        if (getpid_passthrough) {
            set_result(context, trusted_syscall6(
                SYS_getpid, 0, 0, 0, 0, 0, (long)TRUST_MAGIC));
        } else {
            set_result(context, 424242);
        }
        if (nested_test_pending) {
            nested_test_pending = 0;
            nested_test_result = (sig_atomic_t)payload_direct_getpid();
        }
        return;
    case SYS_openat:
        atomic_fetch_add_explicit(&openat_traps, 1, memory_order_relaxed);
        handle_openat(context, arguments);
        return;
    case SYS_rt_sigaction:
        atomic_fetch_add_explicit(&sigaction_traps, 1,
                                  memory_order_relaxed);
        handle_rt_sigaction(context, arguments);
        return;
    case SYS_rt_sigprocmask:
        atomic_fetch_add_explicit(&sigprocmask_traps, 1,
                                  memory_order_relaxed);
        handle_rt_sigprocmask(context, arguments);
        return;
    default:
        trusted_syscall6(SYS_exit_group, 91, 0, 0, 0, 0, 0);
    }
}

static long payload_direct_getpid(void)
{
    long result;
#if defined(__aarch64__)
    register long syscall_number __asm__("x8") = SYS_getpid;
    register long sixth_argument __asm__("x5") = 0;
    register long return_value __asm__("x0");
    __asm__ volatile("svc #0"
                     : "=r"(return_value)
                     : "r"(syscall_number), "r"(sixth_argument)
                     : "memory", "cc");
    result = return_value;
#elif defined(__x86_64__)
    register long sixth_argument __asm__("r9") = 0;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(SYS_getpid), "r"(sixth_argument)
                     : "rcx", "r11", "memory");
#endif
    return result;
}

static uint64_t monotonic_nanoseconds(void)
{
    struct timespec time;
    if (clock_gettime(CLOCK_MONOTONIC, &time) != 0)
        abort();
    return (uint64_t)time.tv_sec * UINT64_C(1000000000) + time.tv_nsec;
}

static double benchmark_getpid(int iterations, long expected)
{
    volatile uint64_t accumulator = 0;
    uint64_t start = monotonic_nanoseconds();

    for (int index = 0; index < iterations; ++index)
        accumulator += (uint64_t)payload_direct_getpid();

    uint64_t elapsed = monotonic_nanoseconds() - start;
    if (accumulator != (uint64_t)expected * (uint64_t)iterations) {
        fprintf(stderr, "unexpected accumulator: %llu\n",
                (unsigned long long)accumulator);
        exit(2);
    }
    return (double)elapsed / iterations;
}

static double benchmark_open_close(int iterations)
{
    uint64_t start = monotonic_nanoseconds();
    for (int index = 0; index < iterations; ++index) {
        int descriptor = (int)syscall(SYS_openat, AT_FDCWD, "/dev/null",
                                      O_RDONLY, 0);
        if (descriptor < 0 || close(descriptor) != 0) {
            perror("benchmark openat/close");
            exit(2);
        }
    }
    uint64_t elapsed = monotonic_nanoseconds() - start;
    return (double)elapsed / iterations;
}

static int install_filter(void)
{
    const uint32_t magic_low = (uint32_t)TRUST_MAGIC;
    const uint32_t magic_high = (uint32_t)(TRUST_MAGIC >> 32);
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, EXPECTED_ARCH, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 6, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_openat, 5, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_rt_sigprocmask, 4, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_rt_sigaction, 0, 2),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 U64_LO_OFFSET(args[0])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SIGSYS, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 U64_HI_OFFSET(args[5])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, magic_high, 0, 3),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 U64_LO_OFFSET(args[5])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, magic_low, 0, 1),
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

static void install_handler_and_main_altstack(void)
{
    void *mapping = mmap(NULL, ALTSTACK_SIZE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    if (mapping == MAP_FAILED) {
        perror("mmap altstack");
        exit(2);
    }
    stack_t stack = {.ss_sp = mapping, .ss_size = ALTSTACK_SIZE, .ss_flags = 0};
    if (sigaltstack(&stack, NULL) != 0) {
        perror("sigaltstack");
        exit(2);
    }

    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_sigaction = sigsys_handler;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_NODEFER;
    if (sigaction(SIGSYS, &action, NULL) != 0) {
        perror("sigaction");
        exit(2);
    }
}

static void dummy_target_sigsys_handler(int signal_number)
{
    (void)signal_number;
}

static void test_sigaction_virtualization(void)
{
    struct sigaction action;
    struct sigaction old_action;
    struct sigaction queried_action;

    memset(&action, 0, sizeof(action));
    action.sa_handler = dummy_target_sigsys_handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSYS, &action, &old_action) != 0) {
        perror("virtual sigaction install");
        exit(3);
    }
    if (sigaction(SIGSYS, NULL, &queried_action) != 0) {
        perror("virtual sigaction query");
        exit(3);
    }
    if (old_action.sa_handler != SIG_DFL ||
        queried_action.sa_handler != dummy_target_sigsys_handler) {
        fprintf(stderr, "virtual sigaction state mismatch\n");
        exit(3);
    }

    errno = 0;
    long invalid = syscall(SYS_rt_sigaction, SIGSYS, (void *)1, NULL,
                           sizeof(unsigned long));
    if (invalid != -1 || errno != EFAULT) {
        fprintf(stderr, "invalid sigaction pointer was not EFAULT: %ld/%d\n",
                invalid, errno);
        exit(3);
    }

    if (payload_direct_getpid() != 424242) {
        fprintf(stderr, "host SIGSYS handler was replaced\n");
        exit(3);
    }

    memset(&action, 0, sizeof(action));
    action.sa_handler = SIG_IGN;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSYS, &action, NULL) != 0 ||
        payload_direct_getpid() != 424242) {
        fprintf(stderr, "virtual SIG_IGN disabled host handler\n");
        exit(3);
    }
}

static void test_sigprocmask_virtualization(void)
{
    sigset_t requested;
    sigset_t visible;

    sigemptyset(&requested);
    sigaddset(&requested, SIGSYS);
    if (sigprocmask(SIG_BLOCK, &requested, NULL) != 0 ||
        sigprocmask(SIG_BLOCK, NULL, &visible) != 0 ||
        sigismember(&visible, SIGSYS) != 1) {
        fprintf(stderr, "SIGSYS block was not virtualized\n");
        exit(3);
    }
    if (payload_direct_getpid() != 424242) {
        fprintf(stderr, "virtual SIGSYS block reached the kernel\n");
        exit(3);
    }
    if (sigprocmask(SIG_UNBLOCK, &requested, NULL) != 0 ||
        sigprocmask(SIG_BLOCK, NULL, &visible) != 0 ||
        sigismember(&visible, SIGSYS) != 0) {
        fprintf(stderr, "SIGSYS unblock was not virtualized\n");
        exit(3);
    }
}

static void test_nested_trap(void)
{
    nested_test_result = -1;
    nested_test_pending = 1;
    if (payload_direct_getpid() != 424242 || nested_test_result != 424242) {
        fprintf(stderr, "nested SIGSYS trap failed\n");
        exit(3);
    }
}

static void test_filesystem_interception(void)
{
    char buffer[256];
    int descriptor = (int)syscall(SYS_openat, AT_FDCWD,
                                  "/virtual/fspy-hostname", O_RDONLY, 0);
    if (descriptor < 0) {
        perror("rewritten openat");
        exit(4);
    }
    ssize_t length = read(descriptor, buffer, sizeof(buffer) - 1);
    close(descriptor);
    if (length <= 0) {
        perror("read rewritten openat");
        exit(4);
    }
    buffer[length] = '\0';

    errno = 0;
    descriptor = (int)syscall(SYS_openat, AT_FDCWD,
                              "/definitely/not/present/fspy", O_RDONLY, 0);
    if (descriptor != -1 || errno != ENOENT) {
        fprintf(stderr, "openat errno propagation failed: %d/%d\n",
                descriptor, errno);
        exit(4);
    }
    printf("filesystem: rewrite=/virtual/fspy-hostname->/etc/hostname "
           "bytes=%zd errno_passthrough=ENOENT\n", length);
}

struct worker_result {
    int failures;
};

static void *worker_main(void *opaque)
{
    struct worker_result *result = opaque;
    for (int index = 0; index < THREAD_ITERS; ++index) {
        if (payload_direct_getpid() != 424242)
            ++result->failures;
    }
    return NULL;
}

static void test_multithreading(void)
{
    pthread_t threads[THREADS];
    struct worker_result results[THREADS] = {{0}};

    getpid_passthrough = 0;
    altstack_probe_enabled = 1;
    uint64_t start = monotonic_nanoseconds();
    for (int index = 0; index < THREADS; ++index) {
        if (pthread_create(&threads[index], NULL, worker_main,
                           &results[index]) != 0) {
            perror("pthread_create");
            exit(5);
        }
    }
    for (int index = 0; index < THREADS; ++index) {
        pthread_join(threads[index], NULL);
        if (results[index].failures != 0) {
            fprintf(stderr, "thread %d had %d failures\n",
                    index, results[index].failures);
            exit(5);
        }
    }
    uint64_t elapsed = monotonic_nanoseconds() - start;
    printf("multithreading: threads=%d calls=%d failures=0 "
           "wall_ns_per_call=%.1f\n",
           THREADS, THREADS * THREAD_ITERS,
           (double)elapsed / (THREADS * THREAD_ITERS));
    altstack_probe_enabled = 0;
}

int main(void)
{
    real_pid = getpid();
    printf("environment: pid=%d arch=%s dynamic_probe=yes\n", real_pid,
#if defined(__aarch64__)
           "aarch64"
#else
           "x86_64"
#endif
    );

    double baseline = benchmark_getpid(BENCH_ITERS, real_pid);
    double open_baseline = benchmark_open_close(OPEN_BENCH_ITERS);
    install_handler_and_main_altstack();
    if (install_filter() != 0) {
        perror("seccomp(SECCOMP_RET_TRAP)");
        return 1;
    }

    getpid_passthrough = 0;
    double trapped_emulated = benchmark_getpid(BENCH_ITERS, 424242);
    getpid_passthrough = 1;
    double trapped_reissued = benchmark_getpid(BENCH_ITERS, real_pid);
    getpid_passthrough = 0;
    double open_trapped = benchmark_open_close(OPEN_BENCH_ITERS);

    test_filesystem_interception();
    test_sigaction_virtualization();
    test_sigprocmask_virtualization();
    test_nested_trap();
    test_multithreading();

    printf("benchmark: iterations=%d baseline_ns=%.1f "
           "trap_emulated_ns=%.1f trap_reissued_ns=%.1f "
           "emulated_overhead_x=%.2f reissued_overhead_x=%.2f\n",
           BENCH_ITERS, baseline, trapped_emulated, trapped_reissued,
           trapped_emulated / baseline, trapped_reissued / baseline);
    printf("openat_benchmark: iterations=%d baseline_open_close_ns=%.1f "
           "trap_reissued_open_close_ns=%.1f overhead_x=%.2f\n",
           OPEN_BENCH_ITERS, open_baseline, open_trapped,
           open_trapped / open_baseline);
    printf("traps: total=%llu getpid=%llu openat=%llu sigaction=%llu "
           "sigprocmask=%llu\n",
           (unsigned long long)atomic_load(&total_traps),
           (unsigned long long)atomic_load(&getpid_traps),
           (unsigned long long)atomic_load(&openat_traps),
           (unsigned long long)atomic_load(&sigaction_traps),
           (unsigned long long)atomic_load(&sigprocmask_traps));
    printf("altstack: on_alt=%llu on_normal=%llu lazy_installed=%llu\n",
           (unsigned long long)atomic_load(&handlers_on_altstack),
           (unsigned long long)atomic_load(&handlers_on_normal_stack),
           (unsigned long long)atomic_load(&lazy_altstacks_installed));
    printf("result: PASS\n");
    return 0;
}
