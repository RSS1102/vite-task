#define _GNU_SOURCE

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#define TRAPPED_RESULT 0x51515151L

static int tracer_pid(void)
{
    char line[256];
    FILE *status = fopen("/proc/self/status", "r");
    if (status == NULL)
        return -1;

    while (fgets(line, sizeof(line), status) != NULL) {
        int tracer;
        if (sscanf(line, "TracerPid:\t%d", &tracer) == 1) {
            fclose(status);
            return tracer;
        }
    }
    fclose(status);
    return -1;
}

int main(void)
{
    int tracer = tracer_pid();
    long result = syscall(SYS_getpid);

    printf("target: TracerPid=%d before trapped getpid\n", tracer);
    printf("target: trapped getpid returned %#lx (expected %#lx)\n", result,
           TRAPPED_RESULT);

    if (tracer != 0) {
        fputs("FAIL: target was still ptraced\n", stderr);
        return EXIT_FAILURE;
    }
    if (result != TRAPPED_RESULT) {
        fputs("FAIL: injected SIGSYS handler did not emulate getpid\n", stderr);
        return EXIT_FAILURE;
    }

    puts("PASS: post-exec handler ran entirely in-process after detach");
    return EXIT_SUCCESS;
}
