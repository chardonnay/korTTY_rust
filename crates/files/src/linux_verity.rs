//! Descriptor-based Linux fs-verity ioctls.

use std::fs::File;
use std::io;

use rustix::io::Errno;
use rustix::ioctl::{self, Opcode, Setter, Updater};

// Linux UAPI include/uapi/linux/fsverity.h. The enable argument is 128 bytes
// on both Linux ABIs. The measure opcode encodes only the four-byte header,
// while its flexible array is covered by the larger output buffer below.
const FS_VERITY_SHA256: u32 = 1;
const FS_VERITY_SHA512: u16 = 2;

#[repr(C)]
struct EnableArg {
    version: u32,
    hash_algorithm: u32,
    block_size: u32,
    salt_size: u32,
    salt_ptr: u64,
    sig_size: u32,
    reserved1: u32,
    sig_ptr: u64,
    reserved2: [u64; 11],
}

#[repr(C)]
struct DigestHeader {
    algorithm: u16,
    size: u16,
}

#[repr(C)]
struct DigestBuffer {
    header: DigestHeader,
    digest: [u8; 64],
}

const _: () = assert!(size_of::<EnableArg>() == 128);
const _: () = assert!(size_of::<DigestHeader>() == 4 && size_of::<DigestBuffer>() == 68);
const ENABLE_VERITY: Opcode = ioctl::opcode::write::<EnableArg>(b'f', 133);
const MEASURE_VERITY: Opcode = ioctl::opcode::read_write::<DigestHeader>(b'f', 134);

/// Check whether the kernel has enabled fs-verity for an open file.
pub fn has_verity(file: &File) -> io::Result<bool> {
    let mut result = DigestBuffer {
        header: DigestHeader {
            algorithm: 0,
            size: 64,
        },
        digest: [0; 64],
    };
    // SAFETY: The ioctl receives the UAPI header followed by the indicated
    // writable digest capacity. It uses the supplied descriptor and does not
    // retain the pointer after the call.
    match unsafe { ioctl::ioctl(file, Updater::<MEASURE_VERITY, _>::new(&mut result)) } {
        Ok(()) => match (result.header.algorithm, result.header.size) {
            (1, 32) | (FS_VERITY_SHA512, 64) => Ok(true),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The kernel returned an invalid fs-verity digest",
            )),
        },
        Err(Errno::NODATA) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Ask the kernel to enable SHA-256 fs-verity on an open file.
pub fn enable_verity(file: &File, block_size: u32) -> Result<(), Errno> {
    let arg = EnableArg {
        version: 1,
        hash_algorithm: FS_VERITY_SHA256,
        block_size,
        salt_size: 0,
        salt_ptr: 0,
        sig_size: 0,
        reserved1: 0,
        sig_ptr: 0,
        reserved2: [0; 11],
    };
    // SAFETY: The setter passes a correctly sized and initialized UAPI
    // record. No fields refer to additional user memory.
    unsafe { ioctl::ioctl(file, Setter::<ENABLE_VERITY, _>::new(arg)) }
}
