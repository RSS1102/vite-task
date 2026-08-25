#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
#include <linux/elf.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <unistd.h>

#if defined(__aarch64__)
#include <asm/ptrace.h>
typedef struct user_pt_regs Registers;
#elif defined(__x86_64__)
typedef struct user_regs_struct Registers;
#else
#error "This probe supports only aarch64 and x86_64"
#endif

static void fail(const char *operation) {
    fprintf(stderr, "%s: %s\n", operation, strerror(errno));
    exit(1);
}

static void fail_status(const char *operation, int status) {
    fprintf(stderr, "%s: unexpected wait status %#x\n", operation, status);
    exit(1);
}

static void get_registers(pid_t pid, Registers *registers) {
    struct iovec iov = {.iov_base = registers, .iov_len = sizeof(*registers)};
    if (ptrace(PTRACE_GETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov) < 0)
        fail("PTRACE_GETREGSET");
    if (iov.iov_len != sizeof(*registers)) {
        fprintf(stderr, "PTRACE_GETREGSET: returned %zu bytes, expected %zu\n",
                iov.iov_len, sizeof(*registers));
        exit(1);
    }
}

static void set_registers(pid_t pid, const Registers *registers) {
    struct iovec iov = {
        .iov_base = (void *)registers,
        .iov_len = sizeof(*registers),
    };
    if (ptrace(PTRACE_SETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov) < 0)
        fail("PTRACE_SETREGSET");
}

static uintptr_t program_counter(const Registers *registers) {
#if defined(__aarch64__)
    return (uintptr_t)registers->pc;
#else
    return (uintptr_t)registers->rip;
#endif
}

static void configure_mmap(Registers *registers, size_t length) {
#if defined(__aarch64__)
    registers->regs[8] = 222;
    registers->regs[0] = 0;
    registers->regs[1] = length;
    registers->regs[2] = PROT_READ | PROT_WRITE | PROT_EXEC;
    registers->regs[3] = MAP_PRIVATE | MAP_ANONYMOUS;
    registers->regs[4] = UINT64_MAX;
    registers->regs[5] = 0;
#else
    registers->orig_rax = 9;
    registers->rax = 9;
    registers->rdi = 0;
    registers->rsi = length;
    registers->rdx = PROT_READ | PROT_WRITE | PROT_EXEC;
    registers->r10 = MAP_PRIVATE | MAP_ANONYMOUS;
    registers->r8 = UINT64_MAX;
    registers->r9 = 0;
#endif
}

static uintptr_t syscall_result(const Registers *registers) {
#if defined(__aarch64__)
    return (uintptr_t)registers->regs[0];
#else
    return (uintptr_t)registers->rax;
#endif
}

static unsigned long syscall_and_trap_word(unsigned long original) {
#if defined(__aarch64__)
    // svc #0; brk #0
    (void)original;
    return UINT64_C(0xd4200000d4000001);
#else
    // syscall; int3. Preserve the remaining bytes in the ptrace word.
    return (original & ~UINT64_C(0xffffff)) | UINT64_C(0xcc050f);
#endif
}

static void wait_for_traceexec(pid_t child) {
    int status;
    while (waitpid(child, &status, __WALL) < 0) {
        if (errno != EINTR) fail("waitpid TRACEEXEC");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP ||
        ((unsigned)status >> 16) != PTRACE_EVENT_EXEC)
        fail_status("PTRACE_EVENT_EXEC", status);

    unsigned long former_tid = 0;
    if (ptrace(PTRACE_GETEVENTMSG, child, 0, &former_tid) < 0)
        fail("PTRACE_GETEVENTMSG");
    printf("traceexec-event               result=PASS former_tid=%lu\n", former_tid);
}

static void test_seize_traceexec_and_injection(const char *self) {
    int ready[2];
    int start[2];
    if (pipe(ready) < 0 || pipe(start) < 0) fail("pipe");

    pid_t child = fork();
    if (child < 0) fail("fork");
    if (child == 0) {
        close(ready[0]);
        close(start[1]);
        if (write(ready[1], "r", 1) != 1) _exit(120);
        char byte;
        if (read(start[0], &byte, 1) != 1) _exit(121);
        execl(self, self, "target", NULL);
        _exit(122);
    }

    close(ready[1]);
    close(start[0]);
    char byte;
    if (read(ready[0], &byte, 1) != 1) fail("read ready");
    close(ready[0]);

    long options = PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL;
    if (ptrace(PTRACE_SEIZE, child, 0, (void *)(uintptr_t)options) < 0)
        fail("PTRACE_SEIZE TRACEEXEC");
    if (write(start[1], "x", 1) != 1) fail("write start");
    close(start[1]);

    wait_for_traceexec(child);

    Registers original_registers;
    get_registers(child, &original_registers);
    set_registers(child, &original_registers);
    printf("get-set-regset                result=PASS bytes=%zu\n",
           sizeof(original_registers));

    uintptr_t pc = program_counter(&original_registers);
    errno = 0;
    unsigned long original_word = (unsigned long)ptrace(PTRACE_PEEKTEXT, child,
                                                        (void *)pc, 0);
    if (original_word == ULONG_MAX && errno != 0) fail("PTRACE_PEEKTEXT");
    if (ptrace(PTRACE_POKETEXT, child, (void *)pc,
               (void *)syscall_and_trap_word(original_word)) < 0)
        fail("PTRACE_POKETEXT syscall+trap");

    Registers syscall_registers = original_registers;
    const size_t mapping_length = 4096;
    configure_mmap(&syscall_registers, mapping_length);
    set_registers(child, &syscall_registers);
    if (ptrace(PTRACE_CONT, child, 0, 0) < 0) fail("PTRACE_CONT remote mmap");

    int status;
    while (waitpid(child, &status, __WALL) < 0) {
        if (errno != EINTR) fail("waitpid remote mmap");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP)
        fail_status("syscall+trap stop", status);

    Registers result_registers;
    get_registers(child, &result_registers);
    uintptr_t mapped = syscall_result(&result_registers);
    if ((intptr_t)mapped < 0 && (intptr_t)mapped >= -4095) {
        fprintf(stderr, "remote mmap: returned errno %ld\n", -(intptr_t)mapped);
        exit(1);
    }
    printf("remote-mmap-syscall-trap      result=PASS address=%#lx\n",
           (unsigned long)mapped);

    const uint64_t sent = UINT64_C(0x1020304050607080);
    uint64_t received = 0;
    struct iovec local = {.iov_base = (void *)&sent, .iov_len = sizeof(sent)};
    struct iovec remote = {.iov_base = (void *)mapped, .iov_len = sizeof(sent)};
    if (process_vm_writev(child, &local, 1, &remote, 1, 0) != (ssize_t)sizeof(sent))
        fail("process_vm_writev injected mapping");
    local.iov_base = &received;
    if (process_vm_readv(child, &local, 1, &remote, 1, 0) != (ssize_t)sizeof(received))
        fail("process_vm_readv injected mapping");
    if (received != sent) {
        fprintf(stderr, "process_vm_readv: value mismatch\n");
        exit(1);
    }
    printf("process-vm-injected-mapping   result=PASS\n");

    if (ptrace(PTRACE_POKETEXT, child, (void *)pc, (void *)original_word) < 0)
        fail("PTRACE_POKETEXT restore");
    set_registers(child, &original_registers);
    if (ptrace(PTRACE_DETACH, child, 0, 0) < 0) fail("PTRACE_DETACH");

    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) fail("waitpid target exit");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
        fail_status("target exit", status);
    printf("detach-resume-target          result=PASS\n");
}

static void test_singlestep(const char *self) {
    pid_t child = fork();
    if (child < 0) fail("fork singlestep");
    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, 0, 0) < 0) _exit(120);
        execl(self, self, "target", NULL);
        _exit(121);
    }

    int status;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) fail("waitpid singlestep exec");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP)
        fail_status("singlestep exec stop", status);
    if (ptrace(PTRACE_SINGLESTEP, child, 0, 0) < 0) fail("PTRACE_SINGLESTEP");
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) fail("waitpid singlestep result");
    }
    bool stopped = WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP;
    printf("single-step                   result=%s status=%#x\n",
           stopped ? "PASS" : "UNSUPPORTED", status);
    if (stopped) {
        if (ptrace(PTRACE_DETACH, child, 0, 0) < 0) fail("detach singlestep");
        while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
    }
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "target") == 0) return 0;
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("pid=%d uid=%u euid=%u\n", getpid(), (unsigned)getuid(),
           (unsigned)geteuid());
    test_seize_traceexec_and_injection(argv[0]);
    test_singlestep(argv[0]);
    return 0;
}
