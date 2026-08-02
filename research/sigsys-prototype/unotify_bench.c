#define _GNU_SOURCE

#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define BENCH_ITERS 100000

#if defined(__aarch64__)
#define EXPECTED_ARCH AUDIT_ARCH_AARCH64
#elif defined(__x86_64__)
#define EXPECTED_ARCH AUDIT_ARCH_X86_64
#else
#error This probe supports AArch64 and x86-64 only
#endif

enum response_mode {
    RESPONSE_EMULATE,
    RESPONSE_CONTINUE,
};

struct child_result {
    double baseline_ns;
    double notified_ns;
    uint64_t accumulator;
};

static long direct_getpid(void)
{
    long result;
#if defined(__aarch64__)
    register long syscall_number __asm__("x8") = SYS_getpid;
    register long return_value __asm__("x0");
    __asm__ volatile("svc #0"
                     : "=r"(return_value)
                     : "r"(syscall_number)
                     : "memory", "cc");
    result = return_value;
#elif defined(__x86_64__)
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(SYS_getpid)
                     : "rcx", "r11", "memory");
#endif
    return result;
}

static uint64_t monotonic_nanoseconds(void)
{
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0) {
        perror("clock_gettime");
        exit(2);
    }
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + value.tv_nsec;
}

static double benchmark_getpid(uint64_t *accumulator)
{
    uint64_t sum = 0;
    uint64_t start = monotonic_nanoseconds();
    for (int index = 0; index < BENCH_ITERS; ++index)
        sum += (uint64_t)direct_getpid();
    uint64_t elapsed = monotonic_nanoseconds() - start;
    *accumulator = sum;
    return (double)elapsed / BENCH_ITERS;
}

static void pin_to_cpu(int cpu)
{
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    if (sched_setaffinity(0, sizeof(set), &set) != 0) {
        perror("sched_setaffinity");
        exit(2);
    }
}

static int install_listener(void)
{
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, EXPECTED_ARCH, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof(instructions) / sizeof(instructions[0])),
        .filter = instructions,
    };

    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        return -1;
    return (int)syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER,
                        SECCOMP_FILTER_FLAG_NEW_LISTENER, &program);
}

static void send_listener(int socket_fd, int listener_fd)
{
    char byte = 'L';
    struct iovec iov = {.iov_base = &byte, .iov_len = 1};
    char control[CMSG_SPACE(sizeof(int))];
    memset(control, 0, sizeof(control));
    struct msghdr message = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = control,
        .msg_controllen = sizeof(control),
    };
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    header->cmsg_level = SOL_SOCKET;
    header->cmsg_type = SCM_RIGHTS;
    header->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(header), &listener_fd, sizeof(listener_fd));
    if (sendmsg(socket_fd, &message, 0) != 1) {
        perror("sendmsg listener");
        exit(3);
    }
}

static int receive_listener(int socket_fd)
{
    char byte;
    struct iovec iov = {.iov_base = &byte, .iov_len = 1};
    char control[CMSG_SPACE(sizeof(int))];
    memset(control, 0, sizeof(control));
    struct msghdr message = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = control,
        .msg_controllen = sizeof(control),
    };
    if (recvmsg(socket_fd, &message, 0) != 1) {
        perror("recvmsg listener");
        exit(3);
    }
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    if (header == NULL || header->cmsg_level != SOL_SOCKET ||
        header->cmsg_type != SCM_RIGHTS) {
        fprintf(stderr, "listener fd missing from control message\n");
        exit(3);
    }
    int listener_fd;
    memcpy(&listener_fd, CMSG_DATA(header), sizeof(listener_fd));
    return listener_fd;
}

static struct child_result run_benchmark(enum response_mode mode)
{
    int sockets[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sockets) != 0) {
        perror("socketpair");
        exit(2);
    }
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        exit(2);
    }

    if (child == 0) {
        close(sockets[0]);
        pin_to_cpu(0);
        struct child_result result;
        result.baseline_ns = benchmark_getpid(&result.accumulator);
        int listener = install_listener();
        if (listener < 0) {
            perror("seccomp user notification listener");
            _exit(3);
        }
        send_listener(sockets[1], listener);
        close(listener);
        char ready;
        if (read(sockets[1], &ready, 1) != 1)
            _exit(3);
        result.notified_ns = benchmark_getpid(&result.accumulator);
        if (write(sockets[1], &result, sizeof(result)) != sizeof(result))
            _exit(3);
        _exit(0);
    }

    close(sockets[1]);
    pin_to_cpu(1);
    int listener = receive_listener(sockets[0]);
    char ready = 'R';
    if (write(sockets[0], &ready, 1) != 1) {
        perror("write ready");
        exit(3);
    }

    struct seccomp_notif request;
    struct seccomp_notif_resp response;
    for (int index = 0; index < BENCH_ITERS; ++index) {
        memset(&request, 0, sizeof(request));
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_RECV, &request) != 0) {
            perror("SECCOMP_IOCTL_NOTIF_RECV");
            exit(3);
        }
        if (request.data.nr != SYS_getpid || request.pid != (uint32_t)child) {
            fprintf(stderr, "unexpected notification nr=%d pid=%u\n",
                    request.data.nr, request.pid);
            exit(3);
        }
        memset(&response, 0, sizeof(response));
        response.id = request.id;
        if (mode == RESPONSE_CONTINUE) {
            response.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
        } else {
            response.val = 424242;
        }
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_SEND, &response) != 0) {
            perror("SECCOMP_IOCTL_NOTIF_SEND");
            exit(3);
        }
    }

    struct child_result result;
    if (read(sockets[0], &result, sizeof(result)) != sizeof(result)) {
        perror("read result");
        exit(3);
    }
    close(listener);
    close(sockets[0]);
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        fprintf(stderr, "child failed: status=%#x\n", status);
        exit(3);
    }

    uint64_t expected = (uint64_t)(mode == RESPONSE_CONTINUE ? child : 424242)
                        * BENCH_ITERS;
    if (result.accumulator != expected) {
        fprintf(stderr, "bad accumulator: %llu expected %llu\n",
                (unsigned long long)result.accumulator,
                (unsigned long long)expected);
        exit(3);
    }
    return result;
}

int main(void)
{
    struct child_result emulated = run_benchmark(RESPONSE_EMULATE);
    struct child_result continued = run_benchmark(RESPONSE_CONTINUE);

    printf("environment: arch=%s iterations=%d cpus=child:0,supervisor:1\n",
#if defined(__aarch64__)
           "aarch64",
#else
           "x86_64",
#endif
           BENCH_ITERS);
    printf("user_notify_emulated: baseline_ns=%.1f notified_ns=%.1f "
           "overhead_x=%.2f\n",
           emulated.baseline_ns, emulated.notified_ns,
           emulated.notified_ns / emulated.baseline_ns);
    printf("user_notify_continue: baseline_ns=%.1f notified_ns=%.1f "
           "overhead_x=%.2f\n",
           continued.baseline_ns, continued.notified_ns,
           continued.notified_ns / continued.baseline_ns);
    printf("result: PASS\n");
    return 0;
}
