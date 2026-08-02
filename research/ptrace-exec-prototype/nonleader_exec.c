#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static const char *exec_target;

static void fatal(const char *message)
{
    perror(message);
    exit(EXIT_FAILURE);
}

static void *worker_exec(void *unused)
{
    char *const argv[] = {(char *)exec_target, NULL};
    (void)unused;
    syscall(SYS_execve, exec_target, argv, environ);
    _exit(111);
}

static void child_main(void)
{
    pthread_t worker;

    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) < 0)
        fatal("PTRACE_TRACEME");
    if (raise(SIGSTOP) != 0)
        fatal("raise SIGSTOP");
    if (pthread_create(&worker, NULL, worker_exec, NULL) != 0)
        fatal("pthread_create");

    /* A successful exec by the worker destroys this thread. */
    for (;;)
        pause();
}

int main(int argc, char **argv)
{
    const unsigned long options =
        PTRACE_O_TRACECLONE | PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL;
    pid_t child;
    int status;
    pid_t worker_tid = -1;

    if (argc != 2) {
        fprintf(stderr, "usage: %s /absolute/path/to/target\n", argv[0]);
        return EXIT_FAILURE;
    }
    exec_target = argv[1];
    child = fork();
    if (child < 0)
        fatal("fork");
    if (child == 0)
        child_main();

    if (waitpid(child, &status, 0) < 0)
        fatal("initial waitpid");
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        fprintf(stderr, "unexpected initial status %#x\n", status);
        return EXIT_FAILURE;
    }
    if (ptrace(PTRACE_SETOPTIONS, child, NULL, (void *)options) < 0)
        fatal("PTRACE_SETOPTIONS");
    if (ptrace(PTRACE_CONT, child, NULL, NULL) < 0)
        fatal("PTRACE_CONT initial");

    for (;;) {
        unsigned int event;
        pid_t stopped_tid = waitpid(-1, &status, __WALL);
        if (stopped_tid < 0)
            fatal("waitpid trace event");
        if (WIFEXITED(status) || WIFSIGNALED(status))
            continue;
        if (!WIFSTOPPED(status))
            continue;

        event = (unsigned int)status >> 16;
        if (event == PTRACE_EVENT_CLONE) {
            unsigned long message = 0;
            if (ptrace(PTRACE_GETEVENTMSG, stopped_tid, NULL, &message) < 0)
                fatal("PTRACE_GETEVENTMSG clone");
            worker_tid = (pid_t)message;
            printf("nonleader: clone event leader=%d worker=%d\n", child,
                   worker_tid);
            if (ptrace(PTRACE_CONT, stopped_tid, NULL, NULL) < 0)
                fatal("PTRACE_CONT clone parent");
            continue;
        }
        if (event == PTRACE_EVENT_EXEC) {
            unsigned long former_tid = 0;
            if (ptrace(PTRACE_GETEVENTMSG, stopped_tid, NULL, &former_tid) < 0)
                fatal("PTRACE_GETEVENTMSG exec");
            printf("nonleader: exec stop reported as tid=%d; former tid=%lu\n",
                   stopped_tid, former_tid);
            if (stopped_tid != child || former_tid != (unsigned long)worker_tid ||
                former_tid == (unsigned long)child) {
                fputs("FAIL: non-leader exec TID transition was unexpected\n",
                      stderr);
                return EXIT_FAILURE;
            }
            if (ptrace(PTRACE_DETACH, stopped_tid, NULL, NULL) < 0)
                fatal("PTRACE_DETACH exec");
            break;
        }

        /* Automatically attached clone children initially report SIGSTOP. */
        if (ptrace(PTRACE_CONT, stopped_tid, NULL,
                   WSTOPSIG(status) == SIGSTOP ? NULL
                                               : (void *)(uintptr_t)WSTOPSIG(status)) < 0 &&
            errno != ESRCH)
            fatal("PTRACE_CONT other stop");
    }

    if (waitpid(child, &status, 0) < 0)
        fatal("waitpid final target");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: target final status %#x\n", status);
        return EXIT_FAILURE;
    }
    puts("PASS: PTRACE_GETEVENTMSG preserved the non-leader's former TID");
    return EXIT_SUCCESS;
}
