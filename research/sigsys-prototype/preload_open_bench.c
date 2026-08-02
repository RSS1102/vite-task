#define _GNU_SOURCE

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#define ITERATIONS 500000

static uint64_t monotonic_nanoseconds(void)
{
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0)
        abort();
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + value.tv_nsec;
}

int main(void)
{
    uint64_t start = monotonic_nanoseconds();
    for (int index = 0; index < ITERATIONS; ++index) {
        int descriptor = openat(AT_FDCWD, "/dev/null", O_RDONLY, 0);
        if (descriptor < 0 || close(descriptor) != 0)
            abort();
    }
    uint64_t elapsed = monotonic_nanoseconds() - start;
    printf("openat_close: iterations=%d ns_per_call=%.1f\n", ITERATIONS,
           (double)elapsed / ITERATIONS);
    return 0;
}
