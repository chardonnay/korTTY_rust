# Sunset source and local patch

This copy contains the Rust library sources, build script, normalized manifest, README and license from the crates.io `sunset` 0.6.0 archive. The archive identifies upstream revision `cb7fbe2c7ba0258a2e9c853668495cc6fc93d502` and has SHA-256 checksum `3b52312b804ac95f10d3963f6ef5889117ad678bd6ce977be4aa4cf491bad78f`. The upstream license is in [LICENSE](LICENSE). The upstream test workspace and hosted adapters are outside this copy. [Published package](https://crates.io/crates/sunset/0.6.0) · [Upstream source](https://github.com/mkj/sunset/tree/cb7fbe2c7ba0258a2e9c853668495cc6fc93d502)

The first two library changes are in `src/kex.rs`. After X25519, call the existing dalek `SharedSecret::was_contributory` validator and abort with `BadKex` on an all-zero shared secret. Both classic and hybrid exchange use this function. [RFC 8731 §3](https://www.rfc-editor.org/rfc/rfc8731.html#section-3)

```diff
-        Ok(self.ours.take().trap()?.diffie_hellman(&theirs))
+        let secret = self.ours.take().trap()?.diffie_hellman(&theirs);
+        // RFC 8731 requires aborting if X25519 produces an all-zero secret.
+        // The hybrid key exchange uses this same X25519 validation path.
+        if !secret.was_contributory() {
+            return Err(Error::BadKex);
+        }
+        Ok(secret)
```

On the initial client exchange, require the server to negotiate OpenSSH strict KEX before sending the client's ephemeral key. This activates the upstream sequence-number and packet-order protections for Terrapin before user authentication. [OpenSSH 9.6 security changes](https://www.openssh.org/txt/release-9.6) · [Terrapin paper and affected modes](https://terrapin-attack.com/)

```diff
+        // Require the OpenSSH Terrapin mitigation for every client session.
+        if CS::is_client() && first_kex && !algos.strict_kex {
+            return Err(Error::AlgoNoMatch { algo: "strict key exchange" });
+        }
```

When a peer guesses the wrong key exchange or host-key algorithm, the receive
path drops the next payload before decoding it or emitting a host-key event.
The key and signature in a discarded reply cannot reach the trust decision.
Ed25519 verification uses Dalek's strict verifier, which rejects small-order
public keys and signature points. [RFC 4253 §7.1](https://www.rfc-editor.org/rfc/rfc4253.html#section-7.1) · [Dalek strict verification](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/struct.VerifyingKey.html#method.verify_strict)

In `src/config.rs`, the existing `larger` feature now uses a 256 KiB channel receive window and a 32 KiB maximum channel data packet. Embedded defaults remain 1,000 bytes. The existing 35,000-byte protocol buffer can hold a 32 KiB packet and its framing. The adapter queues up to sixteen 8 KiB writes per channel. Each receive window accounts for exact payload bytes from both ordinary and extended data. The desktop adapter drains packets into dynamically allocated byte queues and returns receive credit only when a reader consumes or explicitly discards those bytes. Each channel can buffer at most its own receive window across its streams, so a blocked channel cannot hold the shared decrypted packet buffer. [RFC 4253 §6.1](https://www.rfc-editor.org/rfc/rfc4253.html#section-6.1) · [RFC 4254 §5.2](https://www.rfc-editor.org/rfc/rfc4254.html#section-5.2)

`src/channel.rs` and `src/runner.rs` add a tracked session request and a one-way channel EOF. The client sets `want_reply` on the tracked exec or subsystem request and accepts one success or failure for its channel. Unsolicited replies leave request state unchanged. The adapter waits for acceptance before returning the session. Sending EOF stops local writes while the other direction remains open. Received EOF no longer causes an automatic EOF response. [RFC 4254 §§5.3–5.4](https://www.rfc-editor.org/rfc/rfc4254.html#section-5.3) · [RFC 4254 §6.5](https://www.rfc-editor.org/rfc/rfc4254.html#section-6.5)

For an exec with a PTY, the client requests confirmation of `pty-req` first. A successful reply permits a separate confirmed `exec` on the same channel. A refused PTY ends setup before any command request. [RFC 4254 §6.2](https://www.rfc-editor.org/rfc/rfc4254.html#section-6.2)

The desktop adapter can submit one zeroizing input line and EOF on an owned
non-PTY exec channel. The scheduler uses it for the fixed sudo challenge.
`src/traffic.rs` encrypts the packet in place and wipes the writable packet
suffix if encoding or encryption fails.

Those files also add outbound `direct-tcpip` opens and retain the server's open status for the adapter. A locally closed channel number remains reserved until the peer responds, then becomes available again. The desktop `larger` configuration allows sixteen channels, with fourteen forwarding slots and two reserved for the primary session and attached SFTP. The embedded configuration allows four channels. Each direct channel keeps independent half-close and bounded I/O state. The adapter uses the same channel as a byte transport for SSH jump hosts, with separate credentials and host-key verification at each hop. [RFC 4254 §7.2](https://www.rfc-editor.org/rfc/rfc4254.html#section-7.2) · [OpenSSH ProxyJump](https://man.openbsd.org/ssh_config#ProxyJump)

`src/channel.rs` and `src/runner.rs` also expose a tracked second session for the `sftp` subsystem on an authenticated terminal connection. The adapter waits for channel and subsystem confirmation and bounds each terminal to one active lease. Closing or refusing a lease only affects that channel. Session-exit events carry a channel number so a subsystem exit cannot overwrite the shell's status. [RFC 4254 §5](https://www.rfc-editor.org/rfc/rfc4254.html#section-5) · [RFC 4254 §6.5](https://www.rfc-editor.org/rfc/rfc4254.html#section-6.5)

`src/packets.rs`, `src/conn.rs` and `src/runner.rs` support one outstanding `tcpip-forward` or `cancel-tcpip-forward` global request. Requests ask for a reply. The adapter validates that a successful port-zero bind contains one allocated port and that a fixed-port bind or cancellation has an empty reply. `src/channel.rs` and `src/event.rs` expose incoming `forwarded-tcpip` as a client event that rejects by default. The desktop SSH adapter accepts only the exact numeric address and port of a confirmed listener, with four listeners per connection and the existing shared channel limit. Cancellation revokes the listener and its channels locally, then waits for the server's confirmation. A timeout or refused cancellation closes the transport. The server controls which interfaces actually listen under `GatewayPorts`. [RFC 4254 §4](https://www.rfc-editor.org/rfc/rfc4254.html#section-4) · [RFC 4254 §7](https://www.rfc-editor.org/rfc/rfc4254.html#section-7) · [OpenSSH GatewayPorts](https://man.openbsd.org/sshd_config#GatewayPorts)

The same global request slot can carry a reply-bearing `keepalive@kortty.de` request. Its success or failure goes to the liveness detector. If the reply is late, the probe keeps the slot until the reply arrives or the transport closes. SSH global replies carry no request identity. [RFC 4254 §4](https://www.rfc-editor.org/rfc/rfc4254.html#section-4)

The client auth state machine, event API and `src/packets/interactive.rs` add opt-in keyboard-interactive authentication. The method starts only when the server offers it. The client recognizes informational rounds with zero prompts, ordered UTF-8 answers and independent echo flags. Each endpoint allows eight rounds, sixteen prompts per round, 4,096 UTF-8 bytes per text field and 16 KiB of text per packet. The Rust SSH adapter calls the prompt handler after host-key verification and allows a total of 120 seconds for its answers. An endpoint may use keyboard-interactive alone or alongside one explicitly supplied key or password. Prompt answers use their own zeroized values and never draw from a saved password. [RFC 4256](https://www.rfc-editor.org/rfc/rfc4256.html)

## Desktop cryptography providers

The desktop RSA path uses the corrected `rsa` 0.10.0 provider with its
`sha2` 0.11 implementation and the `rand_core` 0.10 OS RNG interface supplied
by `getrandom::SysRng`. It offers `rsa-sha2-512` and `rsa-sha2-256` for
host signatures and user authentication. Public key blobs retain `ssh-rsa`;
the SHA-1 `ssh-rsa` signature algorithm is neither offered nor accepted. RSA
private operations use `sign_with_rng` with OS randomness for blinding and
the provider's checked CRT operation. Private key storage is cleared by the
provider on drop. This release replaces the affected variable-time bigint
implementation described by [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).
The optional OpenSSH reader uses the same vendored `ssh-key` 0.6.7 as the
desktop adapter, converting bounded serialized components directly into
the new provider without enabling the reader's RSA 0.9 signing implementation.

RSA wire integers are bounded before allocation. Imported host keys and the
`SignKey::rsa_private_key` constructor require a 2048–8192-bit modulus; private
keys must have two distinct factors and pass the provider's consistency checks.
Secret integers use storage precision derived from the public modulus. The
provider's normal balanced-factor policy applies; unbalanced factors are not
automatically retried through a different private-operation policy.
RSA pins prefer both SHA-2 signature names while retaining their public-key
identity. A user-auth request retains its selected hash through the unsigned
probe, server confirmation and signed request. Advertised SHA-512 is preferred;
a rejected unsigned probe can retry once with advertised SHA-256. RSA user
authentication requires an RSA SHA-2 offer in `server-sig-algs`. Signature
verification binds the selected algorithm to the signature wrapper and checks
the signed transcript.

The desktop adapter can defer local signing until it receives the server's
public-key confirmation. The client then emits the existing signature event
with the negotiated hash. The adapter checks the startup authorization before
requesting the local signature and checks cancellation and the deadline before
sending it. The unsigned RSA fallback preserves the deferred signing state.

The `rsa_keys` unit tests cover SHA-2 signing and wire round trips, altered
transcripts, signature/hash substitution, bounded RSA key decoding and auth
probe handling. Actual OpenSSH interoperability remains covered separately
by the desktop adapter's opt-in tests; unit coverage alone is not evidence of
a completed platform acceptance run.

The transport remains based on Sunset 0.6.0. The desktop build uses the classical provider APIs from Sunset 0.5.0 (revision `449daa7c4c97f0517946a57363c9c0c04808b919`, archive SHA-256 `65092d44c73e41086a523bea2087a9856fb41760b1b616b307f9895a8da0d35f`). The locked providers are `aes` 0.8.4, `ctr` 0.9.2, `chacha20` 0.9.1, `poly1305` 0.8.0, `sha2` 0.10.9, `hmac` 0.12.1, `ed25519-dalek` 2.2.0 and `x25519-dalek` 2.0.1. The desktop build uses `std`, `rsa` and `ecdsa-userkeys`, which enables `ecdsa-hostkeys`. The upstream `ecdsa256` and `openssh-key` features remain opt-in. [Sunset 0.5.0 source](https://github.com/mkj/sunset/tree/449daa7c4c97f0517946a57363c9c0c04808b919)

The hybrid exchange uses ML-KEM-768 from `fips203` 0.5.0 (revision `5564559d13accac0a428adc496186d072aa61c58`, archive SHA-256 `602e37d80ffd53e1b6037a91bbc471d94bd18a6d1f86493b721b7ef4609a013c`). Only the ML-KEM-768 feature is enabled. The adapter checks encoded lengths and uses the crate's encapsulation-key validation. It supplies OS randomness and preserves the wire order, shared-secret combiner, strict KEX and X25519 contributory check. The crate implements the cryptographic operations. [FIPS 203](https://csrc.nist.gov/pubs/fips/203/final) · [fips203 0.5.0 source](https://github.com/integritychain/fips203/tree/5564559d13accac0a428adc496186d072aa61c58)

The desktop adapter uses `ssh-key` 0.6.7 (revision `6ae2c0850c0febb485fa53678cd622aa0e5f3db5`, archive SHA-256 `3b86f5297f0f04d08cabaa0f6bff7cb6aec4d9c3b49d87990d63da9d9156a8c3`) to parse host keys and decrypt OpenSSH authentication keys. The vendored decoder accepts short ECDSA scalars as positive SSH mpints. The `ed25519` 2.2.3 dependency parses unencrypted Ed25519 PKCS#8. ECDSA uses the existing portable RustCrypto curve providers to sign and derives the public point from the scalar before accepting an included point. PKCS#8 and SEC1 ECDSA PEM require named curve parameters. [Local ssh-key patch](../ssh-key/KORTTY-PATCH.md) [ssh-key 0.6.7 source](https://github.com/RustCrypto/SSH/tree/6ae2c0850c0febb485fa53678cd622aa0e5f3db5/ssh-key) · [RFC 8410 §7](https://www.rfc-editor.org/rfc/rfc8410.html#section-7)

The corrected RSA provider activates `crypto-bigint` and `ctutils`/`cmov`.
The user-approved exception is limited to `cmov`'s constant-time conditional
move/select inline assembly. The default SHA-2 0.11 acceleration on x86-64
and AArch64 uses Rust `core::arch` intrinsics. Existing AES ARM assembly cfgs
and SHA-2 0.10/Keccak assembly features remain disabled. Other transport
providers retain their existing versions. These statements apply to the
default desktop features and toolchain flags; optional upstream features
and other target architectures require separate review. The source and
interoperability checks do not constitute an independent security audit.
[RustCrypto cmov backend](https://github.com/RustCrypto/utils/blob/master/cmov/src/backends.rs)

The desktop client verifies Ed25519, RSA/SHA-2 and ECDSA P-256/P-384/P-521 host keys. The ECDSA path uses RustCrypto `p256` 0.13.2, `p384` 0.13.1 and `p521` 0.13.3 for host signature verification. It binds the negotiated algorithm, signature algorithm and public key to the same curve. An existing pin prioritizes its algorithm at the start of the exchange. A different key or algorithm still requires the normal host-key trust decision before user authentication. [RFC 5656 §§3.1 and 6.2](https://www.rfc-editor.org/rfc/rfc5656.html#section-3.1)

This build offers ML-KEM-768 with X25519 and SHA-256, the Curve25519 SHA-256 exchange names, ChaCha20-Poly1305 and AES-256-CTR with HMAC-SHA-256. A connection requires strict KEX and a supported host key. The terminal session accepts passwords, keyboard-interactive and Ed25519, RSA or ECDSA private keys. The supported ECDSA curves are P-256, P-384 and P-521. The desktop adapter decrypts passphrase-protected OpenSSH files locally after host verification. It accepts bcrypt KDF rounds from 1 to 128 with 16–64 bytes of salt. [OpenSSH private key format](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.key)

Run the adversarial regressions with `cargo test --locked -p kortty-ssh --test kex`. For the separate OpenSSH smoke on macOS or Linux, run `cargo test --locked -p kortty-ssh --test openssh -- --ignored` with `ssh-keygen` and `/usr/sbin/sshd` installed and a local account allowed to authenticate to an unprivileged test daemon.

The test daemon can force either supported algorithm pair. The hybrid case requires an OpenSSH server with `mlkem768x25519-sha256`. Run both cases with the default test parallelism.

```sh
KORTTY_OPENSSH_TEST_CRYPTO=hybrid cargo test --release --locked -p kortty-ssh --test openssh -- --ignored
KORTTY_OPENSSH_TEST_CRYPTO=classic cargo test --release --locked -p kortty-ssh --test openssh -- --ignored
cargo tree --locked -p kortty-ssh --edges normal,build --target aarch64-apple-darwin
```

The separate keyboard-interactive wire test uses a loopback Paramiko 4.0.0 server and fresh keys from `ssh-keygen`. Its credential checks and command output are deterministic test doubles. The test uses real SSH key exchange, host signatures, a signed public-key factor, prompt responses and channel packets. Run it with a Python environment containing Paramiko 4.0.0 or use the isolated command below.

```sh
uv run --isolated --with 'paramiko==4.0.0' -- cargo test --locked -p kortty-ssh --test keyboard_interactive -- --ignored
```
