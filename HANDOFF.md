# kortty porting handoff

Baseline verified on 2026-11-08 at commit `530331bcb99c5d09a59796db55dc3b003cbc7d65`.
This document captures the implementation request and evidence at handoff. Recheck
the current checkout and CI before acting. Historical results are not a substitute
for verification of subsequent changes.

## Continuation prompt

Continue the existing kortty Rust port on this server.

Treat this as an implementation task and an explicit request to establish a
persistent goal for completing the port. Continue from the existing implementation.
Do not restart the project or replace working subsystems without concrete evidence.

Communicate with me in German. All repository documentation, source comments, and
application text must be English. Preserve original fixture text needed for Unicode
and compatibility tests. Use lowercase `kortty` in source code and documentation.
The GitHub repository name is `korTTY_rust`.

## 1. Objective and priorities

Complete the native Rust port of the Java application, including SithTermFX Core
and UI parity across all 21 emulations.

Supported desktop platforms:

- macOS.
- Windows.
- Linux with native Wayland.
- Linux with X11.

The highest product priority is matching the Java application's window and dialog
design, especially icon bars, menu bars, configuration dialogs, tool windows,
complete AI Manager workflows, responsive layouts, and persistent window geometry.

The objective includes all remaining functional parity and acceptance work.
Completing one feature or obtaining green unit tests does not complete the goal.

Both Mosh modes are excluded for now. The Monaco replacement may have a deliberately
reduced feature set, but must be a usable native editor with the required structure
navigation.

Use native Rust crates and operating-system APIs. No WebUI, Electron, Java runtime,
browser renderer, or mandatory external product for core application functionality.
External services and programs are permitted as explicitly enabled integrations and
as clearly identified verification tools.

## 2. Repository and reference baseline

- Repository: https://github.com/chardonnay/korTTY_rust
- Last verified branch: `main`.
- Last verified implementation commit: `530331bcb99c5d09a59796db55dc3b003cbc7d65`,
  `Use English throughout the project documentation`.

The original checkout was clean before adding this handoff document. Check the
actual current branch, commit, remote, and working tree here. Preserve newer work
and user changes.

Pinned functional references:

- Java application 2.26.0 at commit `1ebbe724ee84`.
- SithTermFX v1.2.7 at commit `8b02de38b7c6`.

Original machine paths, for identification only:

- `/Users/daniel/Software-Projects/kortty_rust`
- `/Users/daniel/Software-Projects/kortty_java/korTTY`
- `/Users/daniel/Software-Projects/SithTermFX`

Discover the corresponding paths on this server. Do not assume those macOS paths
exist here. The separate SithTermFX checkout is the correct reference. The Java
repository's vendored SithTermFX checkout was v1.2.5 and must not silently replace
the v1.2.7 baseline.

Read:

- `AGENTS.md`.
- `Cargo.toml` and `Cargo.lock`.
- `README.md`, especially Verification and Release status.
- `.github/workflows/ci.yml`.
- The Java `app-docs/doc-manifest.yaml`.
- SithTermFX `CHANGELOG.md` and `docs/integration-guide.adoc`.
- Relevant source and tests before modifying a subsystem.

The port starts with a fresh profile. Do not copy personal profiles, secrets,
vaults, host keys, or credentials into the repository.

Old verification logs and package artifacts under `target/` are intentionally not
tracked. Their absence in a fresh clone is expected. Reference checkouts, local AI
services, credentials, and OS test machines require separate availability.

## 3. Verified baseline and immediate CI blockers

The project already contains a substantial native implementation. Workspace
modules include terminal core, PTY and process handling, IBM connectors, SSH and
SFTP, serial transport, windowing and macOS integration, diagrams, scheduler,
recording, vault, Control API and CLI, and native packaging.

The GUI uses Iced 0.14 with the tiny-skia renderer. Intentional vendor patches exist
for iced-tiny-skia, softbuffer, smithay-clipboard, ssh-key, and sunset. Inspect their
documentation and purpose before changing them.

The original plan proposed evaluating Iced first and GPUI only if required desktop
workflows could not be supported. Iced is now implemented. Do not repeat framework
selection without a demonstrated blocker.

CI run for the baseline commit:
https://github.com/chardonnay/korTTY_rust/actions/runs/42430876231

Observed results on 2026-11-08:

- `macos-15` passed formatting, Clippy, workspace tests with zsh, release
  compilation, and package staging.
- `ubuntu-24.04` failed Check lints.
- `windows-2025` failed Check lints.
- Subsequent Linux and Windows test, build, and package steps were skipped.

Known diagnostics:

- Linux and Windows: unused method `has_standalone_openpgp` in
  `src/workspace/openpgp_window.rs` around line 149.
- Windows: a `large_enum_variant` diagnostic.
- Windows tests: unnecessary `mut` on a `DirBuilder` in
  `src/workspace/chrome.rs` around line 3454.

Read the full actual diagnostics before fixing them. Correct platform-specific
ownership or conditional compilation appropriately. Do not broadly suppress
warnings or weaken CI.

Green macOS CI is not proof of full functional parity, native GUI acceptance on all
systems, or an independent security review.

## 4. Existing behavior to preserve

Desktop:

- Native multiple windows, tabs, nested splits, focus navigation, and live tab
  movement.
- Session lifetime independent of window placement.
- Native drag paths for macOS, Windows, X11, and Wayland.
- Java-inspired toolbar/menu groups, customization, connection shortcuts, icon
  sizing, and overflow.
- Several tools can open as windows or hosted tool tabs.
- Native macOS menu integration.
- Geometry persistence and monitor-fitting logic.
- Project/session restoration and protection of unsaved drafts.

Terminal:

- Nineteen character profiles and two IBM profiles are exposed.
- Native parser, terminal state, scrollback, search, input encoding, and display.
- Kitty graphics, OSC clipboard handling, strict filtering, and idle scrollback
  compression.
- Local PTY, ConPTY, SSH workers, serial sessions, multi-exec, and independent
  broadcast handling.
- Native built-in terminal effects and diagnostics.

Other implemented areas include connection management, vault and key tools, SFTP,
tunnels, scheduler, journals, recording, backups, CLI, local Control API, native
editor, native diagrams, AI Manager, local AI workflows, snippet libraries,
variables, imports/exports, protected exports, and AI analysis.

These are implemented areas, not a declaration of complete Java parity. Derive
precise coverage from source and tests.

## 5. Complete terminal parity requirements

Implement and verify all 21 SithTermFX emulations, including Xterm, the DEC VT
family, Wyse, TeleVideo, HP, SCO ANSI, Sun CDE, DEC CTERM, PETSCII, IBM TN3270, and
IBM TN5250.

Use a shared screen and scrollback model with appropriate parser and input behavior
per family. Verify terminal identifiers, character sets, keyboard codes, modes,
and responses for each profile. GUI sessions and the scheduler must use the same
terminal core.

Preserve:

- Alternate screen, scroll regions, and reflow.
- Character and pixel sizes.
- Mouse protocols and typeahead.
- Colors and cursor behavior.
- Search, selection, copy/paste.
- Font zoom without disconnecting.
- OSC-8 links with kortty link rules.
- Scrollback limits by lines and memory.
- Lossless idle compression preserving styles, links, markers, and image anchors.

IBM requirements:

- Native TN3270E and TN5250E connectors.
- Telnet negotiation and record framing.
- EBCDIC, protected and numeric fields.
- Cursor movement, AID keys, and host replies.
- Explicit per-connection opt-in for unencrypted direct connections.
- SSH forwarding where applicable.
- Actual host interoperability as an acceptance requirement.

Kitty graphics must cover the v1.2.7 supported subset, including direct and chunked
RGB, RGBA, and PNG, compression, placement layers, and line-anchored images. Verify
scrolling, scrollback, reflow, and movement between windows.

Clipboard requirements include OSC 5522 MIME types, chunked transfers, and paste
events. Preserve access disabled by default, explicit consent, internal clipboard,
enterprise policy, and strict-mode restrictions. Preserve the desktop UTF-8 text
limit of 256 KiB unless reference-backed requirements justify a change. Core
binary/MIME handling does not automatically authorize desktop clipboard access.

Malformed, oversized, and incomplete escape sequences must not terminate a session.

## 6. Windowing, rendering, threading, and geometry

The defining desktop acceptance case is moving a tab containing a live session and
nested splits between windows while output is active. Preserve process identity,
scrollback, split arrangement, and focused pane.

Also verify modal ownership and focus, macOS system menu and Dock reopen, tool
windows, docking/hosted tabs, fullscreen, transparency, project/session restoration,
and correct handling of unsaved edits.

Persist user-adjusted sizes for all dialogs. Provide suitable initial sizes on a
fresh profile and fully reachable content and footer buttons on small screens.

Wayland requirements:

- Respect compositor ownership of window position.
- Restore saved dialog size even when Mutter auto-maximizes windows.
- Do not change global Mutter or desktop settings.
- Temporarily fit a large saved dialog to a smaller monitor.
- Preserve its preferred original size and restore it on the larger monitor.

Historical native evidence from the previous machine: Fedora ARM64 with Mutter
50.6 was tested with a private virtual monitor changing from 1600x1000 to 800x600
and back. Settings and AI Manager fit the small display and recovered their
original larger dimensions without changing saved geometry. This is historical
evidence for those cases. Repeat current-build checks and extend coverage to other
dialogs, scaling, and monitor arrangements.

macOS performance requirements:

- A retained window image and terminal damage-based redraw already exist.
- Preserve the vendor renderer work.
- Verify that unchanged content does not cause unnecessary full-window redraws.
- Mouse, keyboard, menus, selection, and resizing must remain responsive in a
  maximized 5K window.
- Compare optimized builds for performance and investigate development-build
  responsiveness separately.
- Existing `terminal_row_damage` tests compare partial and full rendering at
  Retina scale.

Threading requirements:

- UI thread owns presentation, layout, and input routing.
- Each session owns transport and terminal state independently of a window.
- Use bounded queues, ordered input, and batched updates.
- A blocked broadcast target must not stall other panes.
- Windows ConPTY requires independent read and write paths.
- Closing sessions must terminate and reap their owned processes.

## 7. SSH, security, and integrations

The original plan left SSH backend selection open. The implementation now includes
`vendor/sunset` and `vendor/ssh-key`. Audit the actual implementation and dependency
tree.

Do not reintroduce a backend whose C or assembly cryptography conflicts with the
agreed native Rust constraint. Operating-system interfaces remain allowed. Inspect
`vendor/sunset/KORTTY-PATCH.md` and verify dependencies per target.

The SSH RSA/SHA-2 provider has a specific exception for RustCrypto's `cmov`
inline assembly on AArch64 and x86-64. This permits the constant-time selection
primitives used by the RSA implementation patched for
[RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).

Complete Java parity for SSH authentication and host-key handling, SFTP, jump hosts,
tunnels, serial connections, vault and backups, process isolation, and enterprise
policies.

A required sandbox must be proven available before starting a session. Unsupported
policy must fail safely. Reject changed SSH host keys before authentication against
an actual SSH test server.

Independent security review of the SSH transport remains a release gate. Tests and
self-review cannot substitute for it.

## 8. Native editor and diagrams

The native editor may be simpler than Monaco, but must support the agreed editing
workflows and structure navigation for classes, functions, variables, and
placeholders. Follow Java behavior where included in scope.

Native diagrams must not depend on Mermaid CLI, browser execution, Graphviz, or an
external rendering service.

Required architecture:

- One Rust scene drives display, hit testing, SVG export, and PNG export.
- Scheduler dependency diagrams derive directly from job data.
- Local script flowcharts reference actual source lines.
- Zoom, alignment, color mode, and source navigation work locally.
- Full exports include content outside the current viewport.

Supported bounded Mermaid input families are flowchart, sequence, state, class,
entity relationship, mindmap, and simple architecture.

Unsupported syntax remains available as copyable source. Reject directives,
callbacks, and external resources. Bound input size, edge counts, and raster
dimensions. Retain a bounded Mermaid serializer for clipboard/report compatibility.

An explicitly enabled AI profile may generate the five snippet diagram types.
Allow at most one repair attempt for invalid output. For whole-script flowcharts,
retain the local generator as fallback.

## 9. AI Manager, journal, and snippet work

Complete the AI Manager to the Java application's functional scope. Current
sections are General, Profiles, Local Models, Knowledge Stores, AI Skills, AI
Memory, and Local Preferences.

Current implementation includes local endpoint/model tests, setup wizard behavior,
profile selection, skill editing/import/export, memory management, and local
embedding/search flows.

Important limits to audit:

- Local model management operates against an already running service.
- Knowledge indexing described in README remains in dialog memory.
- Full provider, lifecycle, persistence, and Java workflow parity is not established.
- The Skill Markdown editor must resize dynamically with the AI Manager window.
- Preserve preview-before-apply behavior and unsaved-change handling.

Journal screenshot analysis must be enabled by default as a user preference and
remain user-disableable. This does not bypass AI enablement, consent, endpoint
approval, or enterprise restrictions.

Latest snippet implementation:

- Single-snippet and folder AI analysis.
- Private persistent analysis histories.
- Stable identifiers, pins, differences, and source jumps.
- Selection and source-scope protections.
- Explicit local AI request consent.
- PRE analysis report exports in HTML, Markdown, and JSON.
- Single, combined, and ZIP report packaging.

Relevant source:

- `src/snippet_analysis.rs`
- `src/snippet_analysis/model.rs`
- `src/snippet_analysis/store.rs`
- `src/snippet_analysis/request.rs`
- `src/snippet_analysis/report.rs`
- `src/workspace/snippet_access/analysis/reports.rs`

Next identified unfinished snippet parity:

- Native PDF analysis reports.
- Persisted analysis diagrams.
- Applying proposed changes with appropriate review.
- POST analysis and verification workflows.
- Remaining Java report options and branding behavior.

The journal PDF implementation under `src/session_journal/export/pdf` may provide
reusable primitives. Do not assume snippet PDF export exists because the journal
supports PDF. Compare Java `SnippetAnalysisExportController`,
`SnippetAnalysisBatchExport`, and `SnippetAnalysisReports` where available.

## 10. Explicit remaining release gaps

At the baseline commit, README records these incomplete or unverified areas:

- Additional HP hardware functions and real HP application/device verification.
- Real IBM host interoperability.
- Audio verification on Windows and Linux.
- Physical serial break, modem-line, and flow-control verification.
- Serial character-set selection and enterprise integration.
- SFTP transfer resume.
- Concurrent external replacement race between final SFTP check and rename.
- SSH forwarding currently limited to three simultaneous streams per pane.
- Tiled recording playback, IBM recording, and video export.
- Additional scheduler action types.
- Local-shell sandbox on macOS and Windows.
- Additional SSH authentication flows and host-key algorithms.
- Independent SSH security review.
- Native Windows x64 and macOS Intel sandbox acceptance.
- Full native window behavior matrix.
- Remaining editor, AI, and diagram parity.

This list is not exhaustive. Build a reference-backed feature matrix covering Java
menus, dialogs, settings, tool flows, and all SithTermFX profiles. Record
implementation status and verification status separately.

Native replacements are required for supplied terminal effects and diagnostic
capabilities previously exposed through JAR/JMX mechanisms. Do not introduce a
mandatory Java runtime.

## 11. Reproducible verification

Start with the documented checks:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
```

Run tests through the appropriate cleanup wrapper on Unix. On macOS:

```sh
bash scripts/with-scratch.sh --zsh-tests cargo test --locked --workspace --all-targets
```

On Linux:

```sh
bash scripts/with-scratch.sh cargo test --locked --workspace --all-targets
```

Read the actual script before use. Use isolated profiles and preserve the user's
real settings.

Use JDK 17 in the separate pinned SithTermFX checkout:

```sh
mvn -pl sithtermfx-core,sithtermfx-ui -am test
```

Historical documented result: 313 core and 49 UI tests passed on macOS.

Rust reference replay:

```sh
cargo test --locked -p kortty-terminal --test reference_replay
```

Existing coverage includes 18 vttest and Midnight Commander captures in Xterm and
VT100 with three byte chunk sizes. This is not coverage of all 21 profiles. Extend
checks for cells, styles, cursor, modes, responses, event ordering, and per-profile
input encoding.

Run Java application smoke tasks in an isolated pinned checkout:

```sh
./gradlew terminalNavigationKeysSmoke terminalLinksSmoke terminalSplitTransparencySmoke \
    multiExecSmoke mermaidRendererSmoke snippetDiagramZoomSmoke
```

Historical macOS execution passed the tasks, but the links smoke skipped its focus
assertion because the window lacked keyboard focus. Preserve this qualification.

Native desktop acceptance:

- Tab transfer with nested splits under load.
- Blocked broadcast target.
- Session and focus continuity.
- Restart geometry and fresh-profile small screens.
- Large-to-small-to-large monitor changes.
- Unsaved-change cancellation.
- Modal dialogs, menus, fullscreen, transparency, and reopen.
- Kitty graphics over both PTY and SSH.
- Seven diagram families, job-status changes, clicks after zoom, complete SVG/PNG.
- Real ConPTY on Windows.
- Real PTY with zsh on macOS.
- Native Wayland and native X11 separately.

Run ignored integration tests from README with their actual prerequisites. A local
protocol test double is not proof of real IBM interoperability. Cross-compilation
and headless tests are not native GUI acceptance.

Installation and performance:

- Start packaged releases on clean target systems.
- Verify runtime dependencies for each target.
- Compare Java and Rust on the same machine under equivalent workloads.
- Require improved startup time or lower idle memory with no worse p95 input latency.
- Include the maximized 5K macOS interaction case.
- Record commit, platform, architecture, session type, scaling, commands, results,
  and limitations.

## 12. Resource and repository discipline

Follow `AGENTS.md` exactly. Use one shared Cargo target directory per working tree.
Cross-builds use its target-triple subdirectories. Use the compact development
profiles in `Cargo.toml`. Do not override debug/incremental settings for ordinary
verification.

On macOS and Linux:

- Use `bash scripts/with-scratch.sh` for temporary test environments.
- Keep temporary profiles, source copies, and extracted packages under its `TMPDIR`.
- Track test-owned processes.
- Explicitly terminate and wait for separately launched processes.

On Windows, use a private temporary run directory, set `TEMP` and `TMP` for the test,
and clean up using `try/finally`. For VM work, remove temporary copies from the
shared folder and guest.

After verification, when no other build depends on the artifacts:

- Run `cargo clean --profile dev`.
- Run `cargo clean --release` after package validation.
- Add `--target` for the corresponding cross-build target.

Keep only the latest verified package and checksum per target in `target/dist`
unless there is a specific retention reason. Check `du -sh . target` and external
scratch directories.

Never delete user profiles, source checkouts, shared toolchains, or unrelated
processes. Do not commit builds, debug files, logs, temporary profiles, credentials,
or caches. Keep necessary source assets, licenses, patches, and test fixtures.

## 13. Working method and first actions

Do not invent endpoints, versions, configuration schemas, test results, or
completion claims. Explicitly distinguish implemented and verified, implemented
but not verified on a target, partial, missing, and blocked by external prerequisites.

Mocks and fakes belong only in tests and must be identified as test doubles. Use
current supported approaches, but avoid speculative dependency upgrades. Preserve
`Cargo.lock` and vendor patches unless a concrete change requires updating them.
Do not add features beyond this porting scope.

Begin by:

1. Inspecting this checkout, `AGENTS.md`, toolchain, platform, available display
   sessions, and reference checkout availability.
2. Checking current GitHub CI and correcting the known Linux/Windows blockers if
   still present.
3. Creating or updating an English parity checklist with reference locations and
   reproducible acceptance steps.
4. Continuing the highest-priority window/dialog and AI Manager gaps.
5. Completing unfinished snippet analysis/report workflows.
6. Progressively closing the remaining terminal, transport, security, and
   cross-platform release gaps.

Ask for genuinely missing access, hardware, reference sources, or ambiguous
requirements. Continue independent work while waiting. Do not guess missing facts.

Keep progress updates concise. Each completed change must state what changed, how
it was verified, and what remains unverified.

Do not mark the overall goal complete until the agreed functionality and acceptance
criteria are satisfied. If external hardware or independent review prevents full
completion, name the remaining gate precisely and provide a concrete validation plan.
