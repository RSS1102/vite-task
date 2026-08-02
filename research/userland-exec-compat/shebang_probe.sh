#!/bin/sh
printf 'shebang=compat-v1 argv0=%s arg1=%s exe=%s\n' "$0" "${1-}" "$(readlink /proc/self/exe)"
/bin/echo shebang-child
