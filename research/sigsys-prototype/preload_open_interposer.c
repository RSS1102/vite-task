#define _GNU_SOURCE

#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdlib.h>

typedef int (*openat_function)(int, const char *, int, ...);
static openat_function next_openat;

__attribute__((constructor))
static void initialize_interposer(void)
{
    *(void **)(&next_openat) = dlsym(RTLD_NEXT, "openat");
    if (next_openat == NULL)
        abort();
}

int openat(int directory, const char *path, int flags, ...)
{
    mode_t mode = 0;
    if (flags & (O_CREAT | O_TMPFILE)) {
        va_list arguments;
        va_start(arguments, flags);
        mode = va_arg(arguments, mode_t);
        va_end(arguments);
    }
    return next_openat(directory, path, flags, mode);
}
