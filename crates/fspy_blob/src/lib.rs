//! Flatten a freestanding static-PIE ELF into a relocatable image the injector
//! maps into a process. Parse once with [`Blob::from_elf`], then [`Blob::bind`]
//! the image at a chosen base per target — one payload, many tasks.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ELF offsets and addends fit a 64-bit host's usize/u64"
)]

use anyhow::{Result, bail, ensure};
use goblin::elf::{Elf, header, program_header::PT_LOAD};

const R_X86_64_RELATIVE: u32 = 8;
const R_AARCH64_RELATIVE: u32 = 1027;

/// A flattened payload image and its base-relative relocations.
pub struct Blob {
    image: Vec<u8>,
    entry: usize,
    relocations: Vec<(usize, u64)>,
}

impl Blob {
    /// Parses a static-PIE ELF64 and flattens its `PT_LOAD` segments.
    ///
    /// # Errors
    ///
    /// Fails if the input is not a position-independent ELF for a supported
    /// machine, needs a dynamic loader, or carries a non-`RELATIVE` relocation.
    pub fn from_elf(bytes: &[u8]) -> Result<Self> {
        let elf = Elf::parse(bytes)?;
        ensure!(
            elf.header.e_type == header::ET_DYN,
            "payload must be position-independent (ET_DYN)"
        );
        ensure!(elf.interpreter.is_none(), "payload must not need a dynamic loader");
        let relative = match elf.header.e_machine {
            header::EM_X86_64 => R_X86_64_RELATIVE,
            header::EM_AARCH64 => R_AARCH64_RELATIVE,
            _ => bail!("unsupported machine"),
        };

        let loads = || elf.program_headers.iter().filter(|program| program.p_type == PT_LOAD);
        let len = loads()
            .map(|program| (program.p_vaddr + program.p_memsz) as usize)
            .max()
            .unwrap_or_default();
        let mut image = vec![0; len];
        for program in loads() {
            let offset = program.p_offset as usize;
            let address = program.p_vaddr as usize;
            let size = program.p_filesz as usize;
            image[address..address + size].copy_from_slice(&bytes[offset..offset + size]);
        }

        let mut relocations = Vec::new();
        for relocation in &elf.dynrelas {
            ensure!(
                relocation.r_type == relative && relocation.r_sym == 0,
                "unsupported relocation type"
            );
            relocations.push((
                relocation.r_offset as usize,
                relocation.r_addend.unwrap_or_default() as u64,
            ));
        }

        Ok(Self { image, entry: elf.entry as usize, relocations })
    }

    /// Bytes to reserve for the mapped image.
    #[must_use]
    pub const fn image_len(&self) -> usize {
        self.image.len()
    }

    /// Entry point as an offset from the mapped base.
    #[must_use]
    pub const fn entry(&self) -> usize {
        self.entry
    }

    /// How many relocations [`Blob::bind`] applies.
    #[must_use]
    pub const fn relocation_count(&self) -> usize {
        self.relocations.len()
    }

    /// Returns the image bytes relocated for `base`.
    #[must_use]
    pub fn bind(&self, base: u64) -> Vec<u8> {
        let mut image = self.image.clone();
        for &(offset, addend) in &self.relocations {
            image[offset..offset + 8].copy_from_slice(&base.wrapping_add(addend).to_le_bytes());
        }
        image
    }
}
