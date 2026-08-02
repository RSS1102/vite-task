#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include <reflect.h>

extern char **environ;

static void inherited_handler(int signal_number)
{
	(void)signal_number;
}

static void *background_thread(void *unused)
{
	(void)unused;
	for (;;)
		pause();
	return NULL;
}

static void prepare_compatibility_state(void)
{
	if (getenv("RUNNER_CLOEXEC")) {
		char fd_text[32];
		int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
		if (fd < 0) {
			perror("open(O_CLOEXEC)");
			exit(70);
		}
		snprintf(fd_text, sizeof(fd_text), "%d", fd);
		setenv("PROBE_CLOEXEC_FD", fd_text, 1);
	}

	if (getenv("RUNNER_SIGUSR1")) {
		struct sigaction action = {0};
		stack_t stack = {0};
		void *memory = mmap(NULL, 1024 * 1024, PROT_READ | PROT_WRITE,
				MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
		if (memory == MAP_FAILED) {
			perror("mmap(signal stack)");
			exit(71);
		}
		stack.ss_sp = memory;
		stack.ss_size = 1024 * 1024;
		if (sigaltstack(&stack, NULL) != 0) {
			perror("sigaltstack");
			exit(72);
		}
		action.sa_handler = inherited_handler;
		sigemptyset(&action.sa_mask);
		if (sigaction(SIGUSR1, &action, NULL) != 0) {
			perror("sigaction(SIGUSR1)");
			exit(73);
		}
	}

	if (getenv("RUNNER_BACKGROUND_THREAD")) {
		pthread_t thread;
		if (pthread_create(&thread, NULL, background_thread, NULL) != 0) {
			perror("pthread_create");
			exit(74);
		}
		pthread_detach(thread);
	}
}

int main(int argc, char **argv)
{
	struct stat status;
	unsigned char *elf;
	char **target_argv;
	const char *target;
	int fd;

	target = getenv("LIBREFLECT_TARGET");
	if (target) {
		/* Model a transformed child exec: the kernel starts this host, while
		 * metadata tells it which logical executable to map. */
		target_argv = argv;
		target_argv[0] = (char *)target;
		unsetenv("LIBREFLECT_TARGET");
	} else if (argc >= 2) {
		target = argv[1];
		target_argv = argv + 1;
	} else {
		fprintf(stderr, "usage: %s TARGET [ARG ...]\n", argv[0]);
		return 64;
	}

	fd = open(target, O_RDONLY);
	if (fd < 0 || fstat(fd, &status) != 0) {
		fprintf(stderr, "open target %s: %s\n", target, strerror(errno));
		return 65;
	}
	elf = mmap(NULL, status.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
	close(fd);
	if (elf == MAP_FAILED) {
		perror("mmap target");
		return 66;
	}

	prepare_compatibility_state();
	reflect_execve(elf, target_argv, environ);
	return 67;
}
