ENTRY(fspy_sigsys_handler)

SECTIONS
{
  . = 0;
  .fspy_blob : ALIGN(16)
  {
    HIDDEN(FSPY_BLOB_START = .);
    KEEP(*(.text.fspy_entry))
    KEEP(*(.text.fspy_restorer))
    KEEP(*(.text.fspy_alloc))
    KEEP(*(.text.fspy_raw_syscall6))
    *(.text .text.*)
    *(.rodata .rodata.*)
    . = ALIGN(8);
    HIDDEN(FSPY_STATE_PTR = .);
    QUAD(0);
    HIDDEN(FSPY_BLOB_END = .);
  }

  .forbidden_writable :
  {
    *(.data .data.*)
    *(.bss .bss.* COMMON)
    *(.data.rel.ro .data.rel.ro.*)
    *(.got .got.*)
    *(.plt .plt.*)
    *(.tdata .tdata.*)
    *(.tbss .tbss.*)
    *(.init_array .init_array.*)
    *(.fini_array .fini_array.*)
  }

  /DISCARD/ :
  {
    *(.comment)
    *(.debug_frame .debug_frame.*)
    *(.eh_frame .eh_frame.*)
    *(.note .note.*)
    *(.gnu.attributes)
  }
}

ASSERT(SIZEOF(.forbidden_writable) == 0,
       "injected runtime contains writable, GOT, PLT, TLS, or init data")
ASSERT((FSPY_BLOB_END - FSPY_BLOB_START) < 0x100000,
       "AArch64 ADR cannot reach FSPY_STATE_PTR")
