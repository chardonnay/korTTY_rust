# Local ssh-key change

This directory contains the library source, normalized manifest, README and dual
license files from the published `ssh-key` 0.6.7 package at revision
`6ae2c0850c0febb485fa53678cd622aa0e5f3db5`. The package archive has SHA-256
`3b86f5297f0f04d08cabaa0f6bff7cb6aec4d9c3b49d87990d63da9d9156a8c3`.
Upstream integration test declarations and their fixtures are omitted.
[Published package](https://crates.io/crates/ssh-key/0.6.7) ·
[Upstream source](https://github.com/RustCrypto/SSH/tree/6ae2c0850c0febb485fa53678cd622aa0e5f3db5/ssh-key)

The ECDSA private key decoder accepts positive SSH `mpint` scalars shorter than
the curve width and right-aligns them. It retains compatibility with fixed-width
scalars written by the original library. Negative, empty and oversized values
are rejected. The SSH adapter still validates the scalar and its derived public
point with the selected curve provider before signing.
[RFC 4251 §5](https://www.rfc-editor.org/rfc/rfc4251.html#section-5) ·
[OpenSSH format](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.key)
