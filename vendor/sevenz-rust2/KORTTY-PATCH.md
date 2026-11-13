# korTTY changes to sevenz-rust2

This source comes from the Apache-2.0 `sevenz-rust2` 0.25.0 crate published
on crates.io. The source archive SHA-256 is
`e5e465701779cbc6d0d10cf907f175035cfdd50b8d3cb628d7f1f435ef02972f`.
The upstream license is in `LICENSE`.

The local archive writer uses an archive-scoped AES key with a fresh salt,
the 7-Zip work factor of `2^19`, and a fresh IV for each encrypted stream.
The writer takes explicit header encryption parameters, including for a
directory-only archive. Password and key storage zeroizes on drop. The
decoder derives keys without a process-wide cache and uses the declared
salt length.

The patched sources are `src/encoder.rs`, `src/encoder_options.rs`,
`src/encryption/aes.rs`, `src/encryption/password.rs`, `src/util/compress.rs`
and `src/writer.rs`. `ArchiveAes` replaces `AesEncoderOptions::new` for
encrypted writes. The usage example in `README.md` reflects this API.

The manifest enables zeroization for the AES, CBC and SHA-256 dependencies.
The LZMA dependency uses its standard Rust implementation with `std` and
`encoder`.
