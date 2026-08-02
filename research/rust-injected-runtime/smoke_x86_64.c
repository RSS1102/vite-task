#define _GNU_SOURCE

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
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#if !defined(__x86_64__)
#error "this native smoke harness currently validates x86-64 only"
#endif

#ifndef SA_RESTORER
#define SA_RESTORER 0x04000000
#endif

enum {
    STATE_MAPPING_LEN = 17 * 4096,
    STATE_TRAP_COUNT_OFFSET = 32,
    STATE_LAST_SYSCALL_OFFSET = 40,
    STATE_ARENA_NEXT_OFFSET = 48,
    STATE_ARENA_OFFSET = 4096,
    PROBE_RESULT = 0x51515151,
};

struct kernel_sigaction {
    void (*handler)(int, siginfo_t *, void *);
    unsigned long flags;
    void (*restorer)(void);
    unsigned long mask;
};

typedef void *(*alloc_fn)(size_t, size_t);
typedef long (*syscall6_fn)(long, long, long, long, long, long, long);

static void fatal(const char *message) {
    perror(message);
    exit(1);
}

static uintptr_t parse_offset(const char *text) {
    errno = 0;
    char *end = NULL;
    unsigned long long value = strtoull(text, &end, 0);
    if (errno != 0 || end == text || *end != '\0') {
        fprintf(stderr, "invalid symbol offset: %s\n", text);
        exit(1);
    }
    return (uintptr_t)value;
}

static void *read_blob(const char *path, size_t *size_out) {
    FILE *file = fopen(path, "rb");
    if (file == NULL)
        fatal("fopen blob");
    if (fseek(file, 0, SEEK_END) != 0)
        fatal("fseek blob");
    long length = ftell(file);
    if (length <= 0)
        fatal("ftell blob");
    rewind(file);

    void *bytes = malloc((size_t)length);
    if (bytes == NULL)
        fatal("malloc blob");
    if (fread(bytes, 1, (size_t)length, file) != (size_t)length)
        fatal("fread blob");
    if (fclose(file) != 0)
        fatal("fclose blob");
    *size_out = (size_t)length;
    return bytes;
}

static void install_filter(void) {
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | 0x4653),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof(instructions) / sizeof(instructions[0])),
        .filter = instructions,
    };

    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        fatal("PR_SET_NO_NEW_PRIVS");
    if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) != 0)
        fatal("PR_SET_SECCOMP");
}

int main(int argc, char **argv) {
    if (argc != 7) {
        fprintf(stderr,
                "usage: %s BLOB STATE_PTR HANDLER RESTORER ALLOC RAW_SYSCALL\n",
                argv[0]);
        return 2;
    }

    const uintptr_t state_slot_offset = parse_offset(argv[2]);
    const uintptr_t handler_offset = parse_offset(argv[3]);
    const uintptr_t restorer_offset = parse_offset(argv[4]);
    const uintptr_t alloc_offset = parse_offset(argv[5]);
    const uintptr_t raw_syscall_offset = parse_offset(argv[6]);

    size_t blob_size = 0;
    void *blob = read_blob(argv[1], &blob_size);
    if (blob_size < sizeof(uintptr_t) ||
        state_slot_offset > blob_size - sizeof(uintptr_t) ||
        handler_offset >= blob_size || restorer_offset >= blob_size ||
        alloc_offset >= blob_size || raw_syscall_offset >= blob_size) {
        fprintf(stderr, "symbol offset lies outside the raw blob\n");
        return 1;
    }
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0)
        fatal("sysconf page size");
    size_t code_mapping_len =
        (blob_size + (size_t)page_size - 1) & ~((size_t)page_size - 1);

    unsigned char *state = mmap(NULL, STATE_MAPPING_LEN, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (state == MAP_FAILED)
        fatal("mmap state");
    unsigned char *code = mmap(NULL, code_mapping_len, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED)
        fatal("mmap code");

    memcpy(code, blob, blob_size);
    free(blob);
    memcpy(code + state_slot_offset, &state, sizeof(state));
    if (mprotect(code, code_mapping_len, PROT_READ | PROT_EXEC) != 0)
        fatal("mprotect code RX");

    alloc_fn allocate = (alloc_fn)(code + alloc_offset);
    void *first = allocate(32, 16);
    void *second = allocate(17, 64);
    if (first != state + STATE_ARENA_OFFSET ||
        second != state + STATE_ARENA_OFFSET + 64 ||
        *(uintptr_t *)(state + STATE_ARENA_NEXT_OFFSET) != 81) {
        fprintf(stderr, "fixed allocator returned unexpected ranges\n");
        return 1;
    }

    syscall6_fn raw_syscall = (syscall6_fn)(code + raw_syscall_offset);
    if (raw_syscall(SYS_getppid, 0, 0, 0, 0, 0, 0) <= 0) {
        fprintf(stderr, "raw Rust syscall gateway failed\n");
        return 1;
    }

    struct kernel_sigaction action = {
        .handler = (void (*)(int, siginfo_t *, void *))(code + handler_offset),
        .flags = SA_SIGINFO | SA_NODEFER | SA_RESTORER,
        .restorer = (void (*)(void))(code + restorer_offset),
        .mask = 0,
    };
    if (syscall(SYS_rt_sigaction, SIGSYS, &action, NULL,
                sizeof(action.mask)) != 0)
        fatal("rt_sigaction");

    install_filter();
    long result = syscall(SYS_getpid);
    if (result != PROBE_RESULT) {
        fprintf(stderr, "trapped getpid returned %#lx, expected %#x\n", result,
                PROBE_RESULT);
        return 1;
    }
    if (*(uintptr_t *)(state + STATE_TRAP_COUNT_OFFSET) != 1 ||
        *(uintptr_t *)(state + STATE_LAST_SYSCALL_OFFSET) != SYS_getpid) {
        fprintf(stderr, "Rust handler did not update its state ABI\n");
        return 1;
    }

    puts("PASS: Rust SIGSYS handler, restorer, syscall gateway, and allocator");
    return 0;
}
