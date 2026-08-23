#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <linux/elf.h>
#include <signal.h>
#include <stdbool.h>
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
#include <sys/wait.h>
#include <unistd.h>

static const char *suid_target = "/probe-suid";

static void report(const char *name, bool ok, int error, const char *detail) {
    printf("%-30s result=%-4s errno=%d (%s) %s\n", name, ok ? "PASS" : "FAIL",
           error, error ? strerror(error) : "none", detail ? detail : "");
    fflush(stdout);
}

static void kill_and_reap(pid_t pid) {
    kill(pid, SIGKILL);
    while (waitpid(pid, NULL, 0) < 0 && errno == EINTR) {}
}

static pid_t paused_child(int *ready_fd, volatile uint64_t **remote_word) {
    int pipefd[2];
    if (pipe(pipefd) < 0) abort();
    pid_t pid = fork();
    if (pid < 0) abort();
    if (pid == 0) {
        close(pipefd[0]);
        static volatile uint64_t word = UINT64_C(0x1122334455667788);
        uintptr_t address = (uintptr_t)&word;
        if (write(pipefd[1], &address, sizeof(address)) != sizeof(address)) _exit(121);
        close(pipefd[1]);
        for (;;) pause();
    }
    close(pipefd[1]);
    uintptr_t address;
    if (read(pipefd[0], &address, sizeof(address)) != sizeof(address)) abort();
    *ready_fd = pipefd[0];
    *remote_word = (volatile uint64_t *)address;
    return pid;
}

static void test_traceme_exec(const char *self) {
    pid_t pid = fork();
    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, 0, 0) < 0) {
            dprintf(STDOUT_FILENO, "traceme-child                  result=FAIL errno=%d (%s)\n",
                    errno, strerror(errno));
            _exit(120);
        }
        execl(self, self, "exit-zero", NULL);
        _exit(121);
    }
    int status;
    if (waitpid(pid, &status, 0) < 0) abort();
    bool stopped = WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP;
    int error = 0;
    unsigned char registers[1024];
    struct iovec iov = {.iov_base = registers, .iov_len = sizeof(registers)};
    if (stopped && ptrace(PTRACE_GETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov) < 0)
        error = errno;
    if (!error && stopped && ptrace(PTRACE_SETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS, &iov) < 0)
        error = errno;
    report("traceme+exec+regset", stopped && !error, error,
           stopped ? "SIGTRAP exec-stop" : "no exec-stop");
    if (stopped) {
        if (ptrace(PTRACE_CONT, pid, 0, 0) < 0) abort();
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
    } else if (!WIFEXITED(status) && !WIFSIGNALED(status)) {
        kill_and_reap(pid);
    }
}

static void test_attach(void) {
    int fd;
    volatile uint64_t *unused;
    pid_t pid = paused_child(&fd, &unused);
    close(fd);
    errno = 0;
    int rc = ptrace(PTRACE_ATTACH, pid, 0, 0);
    int error = errno;
    bool ok = rc == 0;
    if (ok) {
        int status;
        ok = waitpid(pid, &status, 0) == pid && WIFSTOPPED(status);
        if (ok) ok = ptrace(PTRACE_DETACH, pid, 0, 0) == 0;
    }
    report("attach-direct-child", ok, error, "");
    kill_and_reap(pid);
}

static void test_seize_and_vm_io(void) {
    int fd;
    volatile uint64_t *remote_word;
    pid_t pid = paused_child(&fd, &remote_word);
    close(fd);
    errno = 0;
    int rc = ptrace(PTRACE_SEIZE, pid, 0, (void *)(uintptr_t)PTRACE_O_TRACEEXEC);
    int error = errno;
    bool seize_ok = rc == 0;
    report("seize-direct-child", seize_ok, error, "");

    uint64_t replacement = UINT64_C(0xaabbccddeeff0011);
    uint64_t observed = 0;
    struct iovec local = {.iov_base = &replacement, .iov_len = sizeof(replacement)};
    struct iovec remote = {.iov_base = (void *)remote_word, .iov_len = sizeof(replacement)};
    errno = 0;
    ssize_t wrote = process_vm_writev(pid, &local, 1, &remote, 1, 0);
    int write_error = errno;
    report("process-vm-write", wrote == sizeof(replacement), write_error, "direct child");

    local.iov_base = &observed;
    errno = 0;
    ssize_t read = process_vm_readv(pid, &local, 1, &remote, 1, 0);
    int read_error = errno;
    bool read_ok = read == sizeof(observed) && observed == replacement;
    report("process-vm-read", read_ok, read_error, "direct child");

    if (seize_ok) {
        if (ptrace(PTRACE_INTERRUPT, pid, 0, 0) < 0) abort();
        int status;
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        if (ptrace(PTRACE_DETACH, pid, 0, 0) < 0) abort();
    }
    kill_and_reap(pid);
}

static int seize_from_child(pid_t target) {
    errno = 0;
    if (ptrace(PTRACE_SEIZE, target, 0, 0) < 0) return errno;
    if (ptrace(PTRACE_INTERRUPT, target, 0, 0) < 0) return errno;
    int status;
    while (waitpid(target, &status, __WALL) < 0) {
        if (errno != EINTR) return errno;
    }
    if (!WIFSTOPPED(status)) return EPROTO;
    if (ptrace(PTRACE_DETACH, target, 0, 0) < 0) return errno;
    return 0;
}

static void test_sibling(bool allow_with_prctl) {
    int tracer_pipe[2], ready_pipe[2];
    if (pipe(tracer_pipe) < 0 || pipe(ready_pipe) < 0) abort();
    pid_t tracer = fork();
    if (tracer == 0) {
        close(tracer_pipe[1]); close(ready_pipe[0]); close(ready_pipe[1]);
        pid_t target;
        if (read(tracer_pipe[0], &target, sizeof(target)) != sizeof(target)) _exit(122);
        _exit(seize_from_child(target) & 0xff);
    }
    close(tracer_pipe[0]);
    pid_t target = fork();
    if (target == 0) {
        close(tracer_pipe[1]); close(ready_pipe[0]);
        if (allow_with_prctl && prctl(PR_SET_PTRACER, tracer, 0, 0, 0) < 0) _exit(123);
        char ready = 'r';
        if (write(ready_pipe[1], &ready, 1) != 1) _exit(124);
        for (;;) pause();
    }
    close(ready_pipe[1]);
    char ready;
    if (read(ready_pipe[0], &ready, 1) != 1) abort();
    close(ready_pipe[0]);
    if (write(tracer_pipe[1], &target, sizeof(target)) != sizeof(target)) abort();
    close(tracer_pipe[1]);
    int status;
    while (waitpid(tracer, &status, 0) < 0 && errno == EINTR) {}
    int result = WIFEXITED(status) ? WEXITSTATUS(status) : 255;
    const char *name = allow_with_prctl ? "seize-sibling-pr-set-ptracer" : "seize-sibling";
    report(name, result == 0, result, allow_with_prctl ? "target opted in" : "same UID");
    kill_and_reap(target);
}

static void test_dumpable_zero(void) {
    int pipefd[2];
    if (pipe(pipefd) < 0) abort();
    pid_t pid = fork();
    if (pid == 0) {
        close(pipefd[0]);
        if (prctl(PR_SET_DUMPABLE, 0) < 0) _exit(120);
        char ready = 'r';
        write(pipefd[1], &ready, 1);
        for (;;) pause();
    }
    close(pipefd[1]);
    char ready;
    if (read(pipefd[0], &ready, 1) != 1) abort();
    close(pipefd[0]);
    errno = 0;
    int rc = ptrace(PTRACE_SEIZE, pid, 0, 0);
    int error = errno;
    report("seize-dumpable-zero-child", rc == 0, error, "");
    if (rc == 0) ptrace(PTRACE_DETACH, pid, 0, 0);
    kill_and_reap(pid);
}

static void test_grandchild(void) {
    int pipefd[2];
    if (pipe(pipefd) < 0) abort();
    pid_t middle = fork();
    if (middle == 0) {
        close(pipefd[0]);
        pid_t target = fork();
        if (target == 0) for (;;) pause();
        if (write(pipefd[1], &target, sizeof(target)) != sizeof(target)) _exit(121);
        for (;;) pause();
    }
    close(pipefd[1]);
    pid_t target;
    if (read(pipefd[0], &target, sizeof(target)) != sizeof(target)) abort();
    close(pipefd[0]);
    errno = 0;
    int rc = ptrace(PTRACE_SEIZE, target, 0, 0);
    int error = errno;
    report("seize-live-grandchild", rc == 0, error, "ancestor, not direct parent");
    if (rc == 0) ptrace(PTRACE_DETACH, target, 0, 0);
    kill_and_reap(target);
    kill_and_reap(middle);
}

static void test_orphan(bool subreaper) {
    if (prctl(PR_SET_CHILD_SUBREAPER, subreaper ? 1 : 0, 0, 0, 0) < 0) abort();
    int pipefd[2];
    if (pipe(pipefd) < 0) abort();
    pid_t middle = fork();
    if (middle == 0) {
        close(pipefd[0]);
        pid_t target = fork();
        if (target == 0) for (;;) pause();
        write(pipefd[1], &target, sizeof(target));
        _exit(0);
    }
    close(pipefd[1]);
    pid_t target;
    if (read(pipefd[0], &target, sizeof(target)) != sizeof(target)) abort();
    close(pipefd[0]);
    waitpid(middle, NULL, 0);
    usleep(10000);
    errno = 0;
    int rc = ptrace(PTRACE_SEIZE, target, 0, 0);
    int error = errno;
    report(subreaper ? "seize-orphan-subreaper" : "seize-orphan-no-subreaper",
           rc == 0, error, subreaper ? "reparented to supervisor" : "reparented away");
    if (rc == 0) ptrace(PTRACE_DETACH, target, 0, 0);
    kill(target, SIGKILL);
    waitpid(target, NULL, 0);
    prctl(PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0);
}

static int run_suid_target(void) {
    printf("suid-target ruid=%u euid=%u\n", (unsigned)getuid(), (unsigned)geteuid());
    fflush(stdout);
    return geteuid() == 0 ? 0 : 42;
}

static void test_suid_exec(bool traced) {
    pid_t pid = fork();
    if (pid == 0) {
        if (traced && ptrace(PTRACE_TRACEME, 0, 0, 0) < 0) {
            dprintf(STDOUT_FILENO, "suid-traceme errno=%d (%s)\n", errno, strerror(errno));
            _exit(120);
        }
        execl(suid_target, suid_target, "suid-target", NULL);
        _exit(121);
    }
    int status;
    waitpid(pid, &status, 0);
    if (traced && WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP) {
        ptrace(PTRACE_CONT, pid, 0, 0);
        waitpid(pid, &status, 0);
    }
    int code = WIFEXITED(status) ? WEXITSTATUS(status) : 255;
    bool ok = traced ? code == 42 : code == 0;
    report(traced ? "setuid-exec-traced" : "setuid-exec-untraced", ok, 0,
           traced ? "expected euid unchanged" : "expected euid root");
}

static void print_proc_field(const char *prefix) {
    FILE *file = fopen("/proc/self/status", "re");
    if (!file) return;
    char *line = NULL;
    size_t size = 0;
    while (getline(&line, &size, file) >= 0) {
        if (!strncmp(line, prefix, strlen(prefix))) fputs(line, stdout);
    }
    free(line);
    fclose(file);
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "exit-zero")) return 0;
    if (argc > 1 && !strcmp(argv[1], "suid-target")) return run_suid_target();
    if (argc > 1 && !strcmp(argv[1], "suid-tests")) {
        test_suid_exec(false);
        test_suid_exec(true);
        return 0;
    }

    printf("pid=%d uid=%u euid=%u\n", getpid(), (unsigned)getuid(), (unsigned)geteuid());
    print_proc_field("CapEff:");
    print_proc_field("NoNewPrivs:");
    print_proc_field("Seccomp:");
    FILE *yama = fopen("/proc/sys/kernel/yama/ptrace_scope", "re");
    if (yama) {
        char value[32];
        if (fgets(value, sizeof(value), yama)) printf("YamaScope:\t%s", value);
        fclose(yama);
    } else {
        printf("YamaScope:\tunavailable (%s)\n", strerror(errno));
    }
    fflush(stdout);

    test_traceme_exec(argv[0]);
    test_attach();
    test_seize_and_vm_io();
    test_grandchild();
    test_sibling(false);
    test_sibling(true);
    test_dumpable_zero();
    test_orphan(false);
    test_orphan(true);
    return 0;
}
