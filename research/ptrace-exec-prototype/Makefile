CC ?= cc
CFLAGS ?= -O2 -g -Wall -Wextra -Werror -std=gnu11

.PHONY: all check clean

all: injector target

injector: injector.c
	$(CC) $(CFLAGS) -o $@ $<

target: target.c
	$(CC) $(CFLAGS) -o $@ $<

check: all
	./injector "$$(realpath ./target)"

clean:
	rm -f injector target
