#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void sleep_millis(long millis) {
    struct timespec delay = {
        .tv_sec = millis / 1000,
        .tv_nsec = (millis % 1000) * 1000000,
    };
    while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {}
}

static bool process_has_argument(pid_t pid, const char *wanted) {
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/cmdline", pid);
    FILE *file = fopen(path, "re");
    if (!file) return false;

    char buffer[1024];
    size_t length = fread(buffer, 1, sizeof(buffer), file);
    fclose(file);

    size_t offset = 0;
    while (offset < length) {
        size_t remaining = length - offset;
        size_t argument_length = strnlen(buffer + offset, remaining);
        if (argument_length == remaining) break;
        if (!strcmp(buffer + offset, wanted)) return true;
        offset += argument_length + 1;
    }
    return false;
}

static pid_t find_process(const char *argument) {
    for (int attempt = 0; attempt < 400; ++attempt) {
        DIR *proc = opendir("/proc");
        if (!proc) return -1;
        struct dirent *entry;
        while ((entry = readdir(proc))) {
            char *end;
            long parsed = strtol(entry->d_name, &end, 10);
            if (*entry->d_name == '\0' || *end != '\0' || parsed <= 0 || parsed == getpid())
                continue;
            pid_t pid = (pid_t)parsed;
            if (process_has_argument(pid, argument)) {
                closedir(proc);
                return pid;
            }
        }
        closedir(proc);
        sleep_millis(25);
    }
    return -1;
}

static long status_number(pid_t pid, const char *field) {
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/status", pid);
    FILE *file = fopen(path, "re");
    if (!file) return -1;
    char *line = NULL;
    size_t capacity = 0;
    long result = -1;
    size_t field_length = strlen(field);
    while (getline(&line, &capacity, file) >= 0) {
        if (!strncmp(line, field, field_length)) {
            char *value = line + field_length;
            result = strtol(value, NULL, 10);
            break;
        }
    }
    free(line);
    fclose(file);
    return result;
}

static bool is_ancestor(pid_t possible_ancestor, pid_t process) {
    for (int depth = 0; depth < 128 && process > 0; ++depth) {
        if (process == possible_ancestor) return true;
        long parent = status_number(process, "PPid:");
        if (parent <= 0 || parent == process) break;
        process = (pid_t)parent;
    }
    return false;
}

static int run_target(bool opt_in) {
    pid_t tracer = find_process("sidecar-tracer");
    if (tracer < 0) {
        fprintf(stderr, "target could not find tracer in its PID namespace\n");
        return 2;
    }

    int prctl_rc = 0;
    int prctl_error = 0;
    if (opt_in) {
        errno = 0;
        prctl_rc = prctl(PR_SET_PTRACER, tracer, 0, 0, 0);
        prctl_error = errno;
    }
    printf("target pid=%d ppid=%ld uid=%u euid=%u tracer=%d opt_in=%s "
           "pr_set_ptracer=%s rc=%d errno=%d (%s)\n",
           getpid(), status_number(getpid(), "PPid:"), (unsigned)getuid(),
           (unsigned)geteuid(), tracer, opt_in ? "yes" : "no",
           opt_in ? (prctl_rc == 0 ? "success" : "failure") : "not-called",
           prctl_rc, prctl_error, prctl_error ? strerror(prctl_error) : "none");
    fflush(stdout);

    if (prctl(PR_SET_NAME, "fspy-ready", 0, 0, 0) < 0) return 3;
    sleep(5);
    return opt_in && prctl_rc < 0 ? 4 : 0;
}

static int run_tracer(bool expect_success) {
    pid_t target = find_process("sidecar-target");
    if (target < 0) {
        fprintf(stderr, "tracer could not find target in its PID namespace\n");
        return 2;
    }

    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/comm", target);
    bool ready = false;
    for (int attempt = 0; attempt < 400; ++attempt) {
        FILE *file = fopen(path, "re");
        char name[64] = {0};
        if (file) {
            (void)fgets(name, sizeof(name), file);
            fclose(file);
        }
        if (!strncmp(name, "fspy-ready", strlen("fspy-ready"))) {
            ready = true;
            break;
        }
        sleep_millis(25);
    }
    if (!ready) {
        fprintf(stderr, "tracer timed out waiting for target readiness\n");
        return 3;
    }

    errno = 0;
    int rc = ptrace(PTRACE_SEIZE, target, 0, 0);
    int error = errno;
    bool completed = rc == 0;
    if (completed) {
        completed = ptrace(PTRACE_INTERRUPT, target, 0, 0) == 0;
        int status = 0;
        if (completed) {
            while (waitpid(target, &status, __WALL) < 0 && errno == EINTR) {}
            completed = WIFSTOPPED(status);
        }
        if (completed) completed = ptrace(PTRACE_DETACH, target, 0, 0) == 0;
    }

    printf("tracer pid=%d ppid=%ld uid=%u euid=%u target=%d target_uid=%ld "
           "tracer_is_ancestor=%s seize_rc=%d errno=%d (%s) completed=%s "
           "expected=%s\n",
           getpid(), status_number(getpid(), "PPid:"), (unsigned)getuid(),
           (unsigned)geteuid(), target, status_number(target, "Uid:"),
           is_ancestor(getpid(), target) ? "yes" : "no", rc, error,
           error ? strerror(error) : "none", completed ? "yes" : "no",
           expect_success ? "success" : "EPERM");
    fflush(stdout);

    if (expect_success) return completed ? 0 : 5;
    return rc < 0 && error == EPERM ? 0 : 6;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr,
                "usage: %s sidecar-target opt-in|no-opt-in | "
                "sidecar-tracer expect-success|expect-eperm\n",
                argv[0]);
        return 1;
    }
    if (!strcmp(argv[1], "sidecar-target")) return run_target(!strcmp(argv[2], "opt-in"));
    if (!strcmp(argv[1], "sidecar-tracer"))
        return run_tracer(!strcmp(argv[2], "expect-success"));
    return 1;
}
