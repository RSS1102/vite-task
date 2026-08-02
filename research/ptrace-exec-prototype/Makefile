CC ?= cc
CFLAGS ?= -O2 -g -Wall -Wextra -Werror -std=gnu11

.PHONY: all check clean

all: injector recursive_injector target nested_trap

injector: injector.c
	$(CC) $(CFLAGS) -o $@ $<

recursive_injector: recursive_injector.c
	$(CC) $(CFLAGS) -pthread -o $@ $<

target: target.c
	$(CC) $(CFLAGS) -o $@ $<

nested_trap: nested_trap.c
	$(CC) $(CFLAGS) -o $@ $<

check: all
	./injector "$$(realpath ./target)"
	./recursive_injector "$$(realpath ./nested_trap)"

clean:
	rm -f injector recursive_injector target nested_trap
