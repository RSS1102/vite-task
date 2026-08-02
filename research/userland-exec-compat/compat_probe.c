#define _GNU_SOURCE

#include <dirent.h>
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <spawn.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PR_GET_AUXV
#define PR_GET_AUXV 0x41555856
#endif

extern char **environ;

static __thread int tls_value;

static void print_file(const char *label, const char *path, bool replace_nuls)
{
	char buffer[4096];
	ssize_t length;
	int fd = open(path, O_RDONLY);
	if (fd < 0) {
		printf("%s=<error:%s>\n", label, strerror(errno));
		return;
	}
	length = read(fd, buffer, sizeof(buffer) - 1);
	close(fd);
	if (length < 0) {
		printf("%s=<error:%s>\n", label, strerror(errno));
		return;
	}
	for (ssize_t i = 0; replace_nuls && i < length; i++)
		if (buffer[i] == '\0')
			buffer[i] = '|';
	while (length > 0 && (buffer[length - 1] == '\n' || buffer[length - 1] == '\0'))
		length--;
	buffer[length] = '\0';
	printf("%s=%s\n", label, buffer);
}

static int task_count(void)
{
	DIR *directory = opendir("/proc/self/task");
	struct dirent *entry;
	int count = 0;
	if (!directory)
		return -1;
	while ((entry = readdir(directory)))
		if (entry->d_name[0] != '.')
			count++;
	closedir(directory);
	return count;
}

static void *thread_worker(void *argument)
{
	long value = (long)argument;
	int fd;
	tls_value = (int)value;
	fd = open("/etc/hostname", O_RDONLY);
	if (fd >= 0)
		close(fd);
	return (void *)(long)tls_value;
}

static void test_threads(void)
{
	pthread_t threads[4];
	long sum = 0;
	for (long i = 0; i < 4; i++)
		if (pthread_create(&threads[i], NULL, thread_worker, (void *)(i + 1)) != 0) {
			printf("threads=creation-failed\n");
			return;
		}
	for (int i = 0; i < 4; i++) {
		void *result = NULL;
		pthread_join(threads[i], &result);
		sum += (long)result;
	}
	printf("threads=ok tls_sum=%ld tasks_after=%d\n", sum, task_count());
}

static void test_subprocess(void)
{
	char *child_argv[] = {"echo", "probe-child", NULL};
	pid_t child;
	int status;
	int error = posix_spawn(&child, "/bin/echo", NULL, NULL, child_argv, environ);
	if (error != 0) {
		printf("subprocess=spawn-error:%s\n", strerror(error));
		return;
	}
	if (waitpid(child, &status, 0) < 0) {
		printf("subprocess=wait-error:%s\n", strerror(errno));
		return;
	}
	printf("subprocess=exit:%d\n", WIFEXITED(status) ? WEXITSTATUS(status) : -1);
}

static void print_auxv(void)
{
	unsigned long execfn = getauxval(AT_EXECFN);
	unsigned long base = getauxval(AT_BASE);
	Elf64_auxv_t kernel_auxv[64];
	long copied = prctl(PR_GET_AUXV, kernel_auxv, sizeof(kernel_auxv), 0, 0);
	unsigned long kernel_execfn = 0;
	if (copied >= 0)
		for (size_t i = 0; i < sizeof(kernel_auxv) / sizeof(kernel_auxv[0]); i++) {
			if (kernel_auxv[i].a_type == AT_EXECFN)
				kernel_execfn = kernel_auxv[i].a_un.a_val;
			if (kernel_auxv[i].a_type == AT_NULL)
				break;
		}
	printf("auxv_execfn=%s\n", execfn ? (char *)execfn : "<null>");
	printf("auxv_base=0x%lx\n", base);
	printf("kernel_auxv=%s kernel_execfn=%s\n", copied < 0 ? strerror(errno) : "ok",
		kernel_execfn ? (char *)kernel_execfn : "<null>");
}

static void print_signal_state(void)
{
	struct sigaction usr1 = {0};
	struct sigaction sys = {0};
	stack_t stack = {0};
	sigaction(SIGUSR1, NULL, &usr1);
	sigaction(SIGSYS, NULL, &sys);
	sigaltstack(NULL, &stack);
	printf("signals=usr1_%s sigsys_%s altstack_%s\n",
		usr1.sa_handler == SIG_DFL ? "default" : usr1.sa_handler == SIG_IGN ? "ignored" : "caught",
		sys.sa_handler == SIG_DFL ? "default" : sys.sa_handler == SIG_IGN ? "ignored" : "caught",
		(stack.ss_flags & SS_DISABLE) ? "disabled" : "enabled");
}

int main(int argc, char **argv)
{
	char executable[PATH_MAX];
	ssize_t executable_length = readlink("/proc/self/exe", executable, sizeof(executable) - 1);
	const char *cloexec_text = getenv("PROBE_CLOEXEC_FD");
	long raw_pid = syscall(SYS_getpid);

	if (executable_length >= 0)
		executable[executable_length] = '\0';
	else
		strcpy(executable, "<readlink-error>");

	printf("probe=compat-v1 pid=%ld libc_pid=%ld ppid=%ld tasks_before=%d\n",
		raw_pid, (long)getpid(), (long)getppid(), task_count());
	printf("proc_exe=%s\n", executable);
	print_file("proc_cmdline", "/proc/self/cmdline", true);
	print_file("proc_comm", "/proc/self/comm", false);
	printf("argc=%d", argc);
	for (int i = 0; i < argc; i++)
		printf(" argv%d=%s", i, argv[i]);
	printf("\n");
	print_auxv();
	print_signal_state();
	if (cloexec_text) {
		int fd = atoi(cloexec_text);
		printf("cloexec_fd=%d state=%s\n", fd,
			fcntl(fd, F_GETFD) < 0 && errno == EBADF ? "closed" : "open");
	} else {
		printf("cloexec_fd=not-provided\n");
	}
	test_threads();
	test_subprocess();
	return 0;
}
