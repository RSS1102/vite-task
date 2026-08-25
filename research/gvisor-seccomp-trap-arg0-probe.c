#define _GNU_SOURCE

#include <errno.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

#ifndef SYS_SECCOMP
#define SYS_SECCOMP 1
#endif

// close(2) has a known first argument and no useful side effects for this
// deliberately invalid descriptor. It makes the signal-frame check independent
// of filesystem state.
static const uintptr_t kExpectedArg0 = 0x1234;
static volatile sig_atomic_t handled;
static volatile uintptr_t observed_arg0;

static void handle_sigsys(int signal, siginfo_t *info, void *context) {
  if (signal != SIGSYS || info == NULL || context == NULL ||
      info->si_code != SYS_SECCOMP || info->si_syscall != SYS_close) {
    _exit(2);
  }

  ucontext_t *uc = context;
#if defined(__x86_64__)
  observed_arg0 = (uintptr_t)uc->uc_mcontext.gregs[REG_RDI];
  uc->uc_mcontext.gregs[REG_RAX] = -EBADF;
#elif defined(__aarch64__)
  observed_arg0 = (uintptr_t)uc->uc_mcontext.regs[0];
  uc->uc_mcontext.regs[0] = (uint64_t)-EBADF;
#else
#error "unsupported architecture"
#endif
  handled = 1;
}

static void install_filter(void) {
  struct sock_filter instructions[] = {
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
               offsetof(struct seccomp_data, nr)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_close, 0, 1),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
  };
  struct sock_fprog program = {
      .len = (unsigned short)(sizeof(instructions) / sizeof(instructions[0])),
      .filter = instructions,
  };

  if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 ||
      prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) != 0) {
    perror("install seccomp filter");
    exit(2);
  }
}

int main(void) {
  struct sigaction action = {
      .sa_sigaction = handle_sigsys,
      .sa_flags = SA_SIGINFO,
  };
  sigemptyset(&action.sa_mask);
  if (sigaction(SIGSYS, &action, NULL) != 0) {
    perror("sigaction");
    return 2;
  }

  install_filter();

  errno = 0;
  long result = syscall(SYS_close, kExpectedArg0);
  int syscall_errno = errno;
  printf("expected-arg0=%#lx observed-arg0=%#lx result=%ld errno=%d\n",
         (unsigned long)kExpectedArg0, (unsigned long)observed_arg0, result,
         syscall_errno);

  if (!handled || observed_arg0 != kExpectedArg0 || result != -1 ||
      syscall_errno != EBADF) {
    puts("seccomp-trap-arg0 result=FAIL");
    return 1;
  }
  puts("seccomp-trap-arg0 result=PASS");
  return 0;
}
