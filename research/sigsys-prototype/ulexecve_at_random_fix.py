#!/usr/bin/env python3
"""Run the reference ulexecve loader with a kernel-compatible AT_RANDOM.

The reference loader points AT_RANDOM into the synthetic initial stack and
uses a word index as a byte offset. Go 1.23+ overwrites the startup random
seed after consuming it, so that bug corrupts argv. This wrapper is only a
small experimental fix; it is not an endorsement of the loader's other
execve emulation choices.
"""

import ctypes
import importlib.util
import os
import sys


loader_path = os.environ.get("ULEXECVE_PATH")
if not loader_path:
    raise SystemExit("ULEXECVE_PATH must name the reference ulexecve.py")

spec = importlib.util.spec_from_file_location("ulexecve_reference", loader_path)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)

original_setup_auxv = module.Stack.setup_auxv


def setup_auxv_with_real_random(self, offset, executable):
    end = original_setup_auxv(self, offset, executable)
    random_bytes = ctypes.create_string_buffer(os.urandom(16))
    self.add_ref(random_bytes)

    cursor = offset
    while self.stack[cursor] != module.Stack.AT_NULL:
        if self.stack[cursor] == module.Stack.AT_RANDOM:
            self.stack[cursor + 1] = ctypes.addressof(random_bytes)
            break
        cursor += 2
    else:
        raise RuntimeError("synthetic auxiliary vector has no AT_RANDOM")
    return end


module.Stack.setup_auxv = setup_auxv_with_real_random
module.main()
