CC ?= cc
CFLAGS ?= -O2 -g -Wall -Wextra -Werror -std=gnu11

.PHONY: all check clean

all: injector recursive_injector target

injector: injector.c
	$(CC) $(CFLAGS) -o $@ $<

recursive_injector: recursive_injector.c
	$(CC) $(CFLAGS) -pthread -o $@ $<

target: target.c
	$(CC) $(CFLAGS) -o $@ $<

check: all
	./injector "$$(realpath ./target)"

clean:
	rm -f injector recursive_injector target
