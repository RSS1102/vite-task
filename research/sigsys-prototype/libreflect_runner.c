#define _GNU_SOURCE

#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include <reflect.h>

extern char **environ;

int main(int argc, char **argv)
{
    struct stat status;
    unsigned char *elf;
    int descriptor;

    if (argc < 2) {
        fprintf(stderr, "usage: %s TARGET [ARG ...]\n", argv[0]);
        return 64;
    }
    descriptor = open(argv[1], O_RDONLY);
    if (descriptor < 0 || fstat(descriptor, &status) != 0)
        return 65;
    elf = mmap(NULL, (size_t)status.st_size, PROT_READ, MAP_PRIVATE,
               descriptor, 0);
    close(descriptor);
    if (elf == MAP_FAILED)
        return 66;
    reflect_execve(elf, &argv[1], environ);
}
