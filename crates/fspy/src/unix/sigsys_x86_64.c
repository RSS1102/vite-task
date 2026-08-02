#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

#if !defined(__linux__) || !defined(__x86_64__)
#error "the experimental fspy SIGSYS injector supports Linux x86-64 only"
#endif

#define ARRAY_LEN(values) (sizeof(values) / sizeof((values)[0]))
#define GATEWAY_MAGIC UINT64_C(0x4653505947415445)
#define GATEWAY_MAGIC_LOW UINT32_C(0x47415445)
#define FILTER_TAG UINT32_C(0x4653)
#define PATH_LIMIT 4096U
#define INSTALL_ACTION_OFFSET 0U

/*
 * This blob is copied into the tracee at its post-exec SIGTRAP stop. It has
 * no relocations, GOT/PLT references, TLS, libc calls, or writable code data.
 * The injector patches the slots at its tail before making the mapping RX.
 *
 * The record writer intentionally mirrors fspy_shared::ipc::channel::ShmWriter:
 *
 *   usize committed_end
 *   repeated { i32 frame_size; frame bytes; padding to 4-byte alignment }
 *
 * A negative frame size commits a complete frame. The frame bytes are the
 * existing wincode PathAccess encoding on 64-bit little-endian Unix:
 * AccessMode(u8), path length(u64), path bytes.
 */
__asm__(
    ".pushsection .text.fspy_sigsys_injected,\"ax\",@progbits\n"
    ".balign 16\n"
    ".global fspy_sigsys_blob_start\n"
    ".global fspy_sigsys_handler\n"
    ".global fspy_sigsys_restorer\n"
    ".global fspy_sigsys_slot_rax\n"
    ".global fspy_sigsys_slot_rdi\n"
    ".global fspy_sigsys_slot_rsi\n"
    ".global fspy_sigsys_slot_rdx\n"
    ".global fspy_sigsys_slot_r10\n"
    ".global fspy_sigsys_slot_r8\n"
    ".global fspy_sigsys_slot_shm\n"
    ".global fspy_sigsys_slot_shm_len\n"
    ".global fspy_sigsys_slot_magic\n"
    ".global fspy_sigsys_blob_end\n"
    "fspy_sigsys_blob_start:\n"
    "fspy_sigsys_handler:\n"
    "  push %rbp\n"
    "  mov %rsp, %rbp\n"
    "  push %rbx\n"
    "  push %r12\n"
    "  push %r13\n"
    "  push %r14\n"
    "  push %r15\n"
    "  mov %rsi, %r12\n" /* siginfo_t * */
    "  mov %rdx, %r13\n" /* ucontext_t * */

    /* Only dispatch traps produced by fspy's tagged filter. */
    "  cmpl $0x4653, 4(%r12)\n"
    "  jne .Lfspy_sigsys_return\n"
    "  mov 24(%r12), %eax\n"
    "  cmp $2, %eax\n" /* __NR_open */
    "  je .Lfspy_sigsys_open\n"
    "  cmp $257, %eax\n" /* __NR_openat */
    "  je .Lfspy_sigsys_openat\n"
    "  mov $-38, %rax\n" /* -ENOSYS */
    "  jmp .Lfspy_sigsys_store_result\n"

    ".Lfspy_sigsys_open:\n"
    "  mov fspy_sigsys_slot_rdi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r15\n" /* path */
    "  mov fspy_sigsys_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rbx\n" /* flags */
    "  jmp .Lfspy_sigsys_record\n"

    ".Lfspy_sigsys_openat:\n"
    "  mov fspy_sigsys_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r15\n" /* path */
    "  mov fspy_sigsys_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rbx\n" /* flags */

    ".Lfspy_sigsys_record:\n"
    /* Convert O_ACCMODE into fspy's AccessMode bits. */
    "  and $3, %ebx\n"
    "  cmp $1, %ebx\n"
    "  je .Lfspy_sigsys_write_mode\n"
    "  cmp $2, %ebx\n"
    "  je .Lfspy_sigsys_read_write_mode\n"
    "  mov $1, %ebx\n"
    "  jmp .Lfspy_sigsys_scan_path\n"
    ".Lfspy_sigsys_write_mode:\n"
    "  mov $2, %ebx\n"
    "  jmp .Lfspy_sigsys_scan_path\n"
    ".Lfspy_sigsys_read_write_mode:\n"
    "  mov $3, %ebx\n"

    /* Minimal experiment: paths are valid C strings, capped at PATH_MAX. */
    ".Lfspy_sigsys_scan_path:\n"
    "  xor %ecx, %ecx\n"
    ".Lfspy_sigsys_scan_path_loop:\n"
    "  cmp $4096, %ecx\n"
    "  jae .Lfspy_sigsys_passthrough\n"
    "  cmpb $0, (%r15,%rcx)\n"
    "  je .Lfspy_sigsys_path_ready\n"
    "  inc %rcx\n"
    "  jmp .Lfspy_sigsys_scan_path_loop\n"

    ".Lfspy_sigsys_path_ready:\n"
    "  mov fspy_sigsys_slot_shm(%rip), %r14\n"
    "  lea 9(%rcx), %r8\n" /* encoded PathAccess size */
    "  mov (%r14), %rax\n"  /* current committed_end */
    ".Lfspy_sigsys_claim_loop:\n"
    "  lea 7(%rax,%r8), %rdx\n" /* old + 4-byte header + frame + 3 */
    "  and $-4, %rdx\n"
    "  mov fspy_sigsys_slot_shm_len(%rip), %rsi\n"
    "  sub $8, %rsi\n"
    "  cmp %rsi, %rdx\n"
    "  ja .Lfspy_sigsys_passthrough\n"
    "  lock cmpxchgq %rdx, (%r14)\n"
    "  jne .Lfspy_sigsys_claim_loop\n"

    "  lea 8(%r14,%rax), %rdx\n" /* claimed frame header */
    "  mov %r8d, (%rdx)\n"        /* positive: write in progress */
    "  mov %bl, 4(%rdx)\n"
    "  mov %rcx, 5(%rdx)\n"
    "  lea 13(%rdx), %rdi\n"
    "  mov %r15, %rsi\n"
    "  rep movsb\n"
    /* x86 TSO publishes the bytes before this aligned atomic-sized store. */
    "  neg %r8d\n"
    "  mov %r8d, (%rdx)\n" /* negative: committed */

    ".Lfspy_sigsys_passthrough:\n"
    "  mov 24(%r12), %ebx\n"
    "  mov fspy_sigsys_slot_rdi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdi\n"
    "  mov fspy_sigsys_slot_rsi(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rsi\n"
    "  mov fspy_sigsys_slot_rdx(%rip), %rcx\n"
    "  mov (%r13,%rcx), %rdx\n"
    "  mov fspy_sigsys_slot_r10(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r10\n"
    "  mov fspy_sigsys_slot_r8(%rip), %rcx\n"
    "  mov (%r13,%rcx), %r8\n"
    "  mov fspy_sigsys_slot_magic(%rip), %r9\n"
    "  mov %ebx, %eax\n"
    "  syscall\n"

    ".Lfspy_sigsys_store_result:\n"
    "  mov fspy_sigsys_slot_rax(%rip), %rcx\n"
    "  mov %rax, (%r13,%rcx)\n"
    ".Lfspy_sigsys_return:\n"
    "  pop %r15\n"
    "  pop %r14\n"
    "  pop %r13\n"
    "  pop %r12\n"
    "  pop %rbx\n"
    "  pop %rbp\n"
    "  ret\n"

    ".balign 8\n"
    "fspy_sigsys_restorer:\n"
    "  mov $15, %eax\n" /* __NR_rt_sigreturn */
    "  syscall\n"
    "  ud2\n"
    ".balign 8\n"
    "fspy_sigsys_slot_rax: .quad 0\n"
    "fspy_sigsys_slot_rdi: .quad 0\n"
    "fspy_sigsys_slot_rsi: .quad 0\n"
    "fspy_sigsys_slot_rdx: .quad 0\n"
    "fspy_sigsys_slot_r10: .quad 0\n"
    "fspy_sigsys_slot_r8: .quad 0\n"
    "fspy_sigsys_slot_shm: .quad 0\n"
    "fspy_sigsys_slot_shm_len: .quad 0\n"
    "fspy_sigsys_slot_magic: .quad 0\n"
    "fspy_sigsys_blob_end:\n"
    ".popsection\n");

extern const unsigned char fspy_sigsys_blob_start[];
extern const unsigned char fspy_sigsys_handler[];
extern const unsigned char fspy_sigsys_restorer[];
extern const unsigned char fspy_sigsys_slot_rax[];
extern const unsigned char fspy_sigsys_slot_rdi[];
extern const unsigned char fspy_sigsys_slot_rsi[];
extern const unsigned char fspy_sigsys_slot_rdx[];
extern const unsigned char fspy_sigsys_slot_r10[];
extern const unsigned char fspy_sigsys_slot_r8[];
extern const unsigned char fspy_sigsys_slot_shm[];
extern const unsigned char fspy_sigsys_slot_shm_len[];
extern const unsigned char fspy_sigsys_slot_magic[];
extern const unsigned char fspy_sigsys_blob_end[];

struct kernel_sigaction_wire {
    uint64_t handler;
    uint64_t flags;
    uint64_t restorer;
    uint64_t mask;
};

static size_t blob_offset(const unsigned char *symbol)
{
    return (size_t)(symbol - fspy_sigsys_blob_start);
}

static void patch_u64(unsigned char *blob, const unsigned char *slot,
                      uint64_t value)
{
    memcpy(blob + blob_offset(slot), &value, sizeof(value));
}

static int ptrace_get_regs(pid_t pid, struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_GETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS,
                       &iov);
}

static int ptrace_set_regs(pid_t pid, const struct user_regs_struct *regs)
{
    struct iovec iov = {.iov_base = (void *)regs, .iov_len = sizeof(*regs)};
    return (int)ptrace(PTRACE_SETREGSET, pid, (void *)(uintptr_t)NT_PRSTATUS,
                       &iov);
}

static int wait_for_breakpoint(pid_t pid)
{
    int status;

    if (waitpid(pid, &status, __WALL) < 0)
        return -1;
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        errno = EPROTO;
        return -1;
    }
    return 0;
}

static int remote_syscall(pid_t pid, long number, uint64_t a0, uint64_t a1,
                          uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5,
                          long *result_out)
{
    struct user_regs_struct saved;
    struct user_regs_struct call_regs;
    struct user_regs_struct stopped;
    unsigned long original_word = 0;
    unsigned long patched_word;
    uintptr_t pc;
    int saved_errno;
    int have_regs = 0;
    int have_word = 0;

    if (ptrace_get_regs(pid, &saved) < 0)
        return -1;
    have_regs = 1;
    call_regs = saved;
    pc = (uintptr_t)saved.rip;
    errno = 0;
    original_word =
        (unsigned long)ptrace(PTRACE_PEEKTEXT, pid, (void *)pc, NULL);
    if (original_word == (unsigned long)-1 && errno != 0)
        goto fail;
    have_word = 1;
    patched_word = original_word;
    ((unsigned char *)&patched_word)[0] = 0x0f; /* syscall */
    ((unsigned char *)&patched_word)[1] = 0x05;
    ((unsigned char *)&patched_word)[2] = 0xcc; /* int3 */
    if (ptrace(PTRACE_POKETEXT, pid, (void *)pc, (void *)patched_word) < 0)
        goto fail;
    call_regs.rax = (uint64_t)number;
    call_regs.orig_rax = UINT64_MAX;
    call_regs.rdi = a0;
    call_regs.rsi = a1;
    call_regs.rdx = a2;
    call_regs.r10 = a3;
    call_regs.r8 = a4;
    call_regs.r9 = a5;
    if (ptrace_set_regs(pid, &call_regs) < 0)
        goto fail;
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) < 0)
        goto fail;
    if (wait_for_breakpoint(pid) < 0)
        goto fail;
    if (ptrace_get_regs(pid, &stopped) < 0)
        goto fail;
    *result_out = (long)stopped.rax;
    if (ptrace(PTRACE_POKETEXT, pid, (void *)pc, (void *)original_word) < 0)
        goto fail;
    have_word = 0;
    if (ptrace_set_regs(pid, &saved) < 0)
        goto fail;
    have_regs = 0;
    return 0;

fail:
    saved_errno = errno;
    if (have_word)
        (void)ptrace(PTRACE_POKETEXT, pid, (void *)pc,
                     (void *)original_word);
    if (have_regs)
        (void)ptrace_set_regs(pid, &saved);
    errno = saved_errno;
    return -1;
}

static int remote_write(pid_t pid, uintptr_t destination, const void *source,
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
            return -1;
        offset += chunk;
    }
    return 0;
}

static int raw_syscall_failed(long result)
{
    return result < 0 && result >= -4095;
}

static int raw_syscall_ok(long result)
{
    if (!raw_syscall_failed(result))
        return 0;
    errno = (int)-result;
    return -1;
}

#define FILTER_GATEWAY_BLOCK(syscall_number)                                  \
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (syscall_number), 0, 4),              \
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,                                    \
                 offsetof(struct seccomp_data, args[5])),                     \
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, GATEWAY_MAGIC_LOW, 1, 0),         \
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | FILTER_TAG),             \
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW)

int fspy_sigsys_prepare(int shm_fd)
{
    int descriptor_flags = fcntl(shm_fd, F_GETFD);
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 offsetof(struct seccomp_data, nr)),
        FILTER_GATEWAY_BLOCK(SYS_open),
        FILTER_GATEWAY_BLOCK(SYS_openat),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)ARRAY_LEN(instructions),
        .filter = instructions,
    };

    if (descriptor_flags < 0)
        return -1;
    if (fcntl(shm_fd, F_SETFD, descriptor_flags & ~FD_CLOEXEC) < 0)
        return -1;
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) < 0)
        return -1;
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0)
        return -1;
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program) < 0)
        return -1;
    return 0;
}

int fspy_sigsys_inject(pid_t pid, int shm_fd, size_t shm_len)
{
    const size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    const size_t blob_size =
        (size_t)(fspy_sigsys_blob_end - fspy_sigsys_blob_start);
    unsigned char *blob = NULL;
    struct kernel_sigaction_wire action = {0};
    uintptr_t remote_code;
    uintptr_t remote_action;
    uintptr_t remote_shm;
    long result;
    int status;
    int saved_errno;

    if (page_size == 0 || blob_size > page_size ||
        INSTALL_ACTION_OFFSET + sizeof(action) > page_size || shm_len < 8) {
        errno = EINVAL;
        return -1;
    }

    if (waitpid(pid, &status, __WALL) < 0)
        return -1;
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        errno = EPROTO;
        return -1;
    }

    if (remote_syscall(pid, SYS_mmap, 0, shm_len, PROT_READ | PROT_WRITE,
                       MAP_SHARED, (uint64_t)shm_fd, 0, &result) < 0 ||
        raw_syscall_ok(result) < 0)
        goto fail;
    remote_shm = (uintptr_t)result;

    if (remote_syscall(pid, SYS_mmap, 0, page_size * 2,
                       PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, UINT64_MAX, 0, &result) <
            0 ||
        raw_syscall_ok(result) < 0)
        goto fail;
    remote_code = (uintptr_t)result;
    remote_action = remote_code + page_size + INSTALL_ACTION_OFFSET;

    blob = malloc(blob_size);
    if (blob == NULL)
        goto fail;
    memcpy(blob, fspy_sigsys_blob_start, blob_size);
    patch_u64(blob, fspy_sigsys_slot_rax,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RAX]));
    patch_u64(blob, fspy_sigsys_slot_rdi,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RDI]));
    patch_u64(blob, fspy_sigsys_slot_rsi,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RSI]));
    patch_u64(blob, fspy_sigsys_slot_rdx,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_RDX]));
    patch_u64(blob, fspy_sigsys_slot_r10,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_R10]));
    patch_u64(blob, fspy_sigsys_slot_r8,
              offsetof(ucontext_t, uc_mcontext) +
                  offsetof(mcontext_t, gregs[REG_R8]));
    patch_u64(blob, fspy_sigsys_slot_shm, remote_shm);
    patch_u64(blob, fspy_sigsys_slot_shm_len, shm_len);
    patch_u64(blob, fspy_sigsys_slot_magic, GATEWAY_MAGIC);
    if (remote_write(pid, remote_code, blob, blob_size) < 0)
        goto fail;
    free(blob);
    blob = NULL;

    action.handler = remote_code + blob_offset(fspy_sigsys_handler);
    action.flags = SA_SIGINFO | SA_NODEFER | UINT64_C(0x04000000);
    action.restorer = remote_code + blob_offset(fspy_sigsys_restorer);
    if (remote_write(pid, remote_action, &action, sizeof(action)) < 0)
        goto fail;

    if (remote_syscall(pid, SYS_mprotect, remote_code, page_size,
                       PROT_READ | PROT_EXEC, 0, 0, 0, &result) < 0 ||
        raw_syscall_ok(result) < 0 || result != 0)
        goto fail;
    if (remote_syscall(pid, SYS_rt_sigaction, SIGSYS, remote_action, 0, 8, 0,
                       GATEWAY_MAGIC, &result) < 0 ||
        raw_syscall_ok(result) < 0 || result != 0)
        goto fail;
    if (remote_syscall(pid, SYS_close, (uint64_t)shm_fd, 0, 0, 0, 0, 0,
                       &result) < 0 ||
        raw_syscall_ok(result) < 0 || result != 0)
        goto fail;

    if (ptrace(PTRACE_DETACH, pid, NULL, NULL) < 0)
        return -1;
    return 0;

fail:
    saved_errno = errno;
    free(blob);
    (void)ptrace(PTRACE_KILL, pid, NULL, NULL);
    errno = saved_errno;
    return -1;
}
