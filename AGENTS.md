# Repository language

Use English for source comments, project documentation, and application text. Keep original text in fixtures that verify Unicode, protocol behavior, or compatibility.

# Build and test artifacts

- Use one shared Cargo target directory per working tree. For cross-builds, use the target-triple subdirectories of the same Cargo target directory. Use alternate profiles or separate caches only for a specific verification need and remove them afterward.
- Use the compact development profiles defined in `Cargo.toml`. Use the default debug and incremental settings for routine checks.
- Keep temporary test profiles, extracted packages, and source copies in one run directory. On macOS and Linux, use `bash scripts/with-scratch.sh <command>`. Store all temporary files under `$TMPDIR` and start test programs directly. Record the processes that belong to the test before starting. The wrapper stops its own process group and removes the run directory on failure or interruption as well as on success. Explicitly stop any separately launched processes and wait for them to exit. For VM tests, also remove copies from the shared folder and the guest. On Windows, use a private run directory and `try/finally` with `TEMP` and `TMP`.
- After a series of checks, inspect any temporary directories you created outside the checkout as well. Keep compact verification records and required package archives with checksums. Remove test binaries, extracted package copies, and unused build caches. Preserve sources, user profiles, shared toolchains, and unrelated running processes.
- Run `cargo clean --profile dev` after checks and `cargo clean --release` after package verification, once other builds have finished using those profiles. Specify the relevant triple with `--target` for cross-builds. Keep the most recent verified package and its checksum for each target in `target/dist/`. Keep older packages only when a specific retention need exists. Check the remaining disk usage with `du -sh . target` and check any external test directories separately.
