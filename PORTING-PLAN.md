# Remaining Java-to-Rust porting plan

Source audit and implementation checkpoint updated on 2026-11-13. This is the
current backlog for the scope in [HANDOFF.md](HANDOFF.md). Implementation and
execution history stays in [PORTING-PARITY.md](PORTING-PARITY.md). Its baseline tables describe the
original handoff and must not be read as the current missing-feature list.
Detailed verification files linked under `target/verification/` are local
build records. The verification summaries in this plan are available in Git.

The D1/A1 shell integration and Agent recognition checkpoint retains its
acceptance and distribution work. Step 2 records the D2, A5 and P1
implementation for connection input settings and managed policy. Step 3 records
SFTP transfer settings, replacement hardening, browser image and external-file
editing, server-only archive creation, Unix rsync upload, and typed sudo file
actions. Step 4 records interactive Swarm persistence, reviewed provider
routes and tools, selectable tokenizers, and larger knowledge and journal
indexes.
The [reviewed HTTPS workflow checkpoint](PORTING-PARITY.md#reviewed-https-profiles-for-swarm-scheduler-and-journal-summaries)
extends that provider support to interactive Swarm, scheduled Agent and Swarm,
and journal summaries.
The [external skills and editor checkpoint](PORTING-PARITY.md#external-skills-python-structure-and-java-effect-exchange)
adds reviewed provider discovery and updates, Python bindings, and
fractional effect interchange.
The [command summaries, Lua tools, and backup checkpoint](PORTING-PARITY.md#command-summaries-lua-editor-tools-and-ai-profile-backup)
adds the release-specific editor and journal workflows, AI data backup, and
the personal startup password setting.
The [Scheduler replacement, directory rsync, and MiniMax CLI checkpoint](PORTING-PARITY.md#scheduled-changed-file-downloads-rsync-directories-and-minimax-cli)
extends the one-use Windows download workflow and adds the Unix directory and
explicitly reviewed CLI paths.
The [reviewed CLI workflow checkpoint](PORTING-PARITY.md#reviewed-cli-profiles-for-agent-swarm-scheduler-and-journals)
adds CLI execution to Agent Ask/Run, interactive and scheduled Swarm, scheduled
Agent and manual and automatic journal summaries.
The [CLI catalogue and profile preferences checkpoint](PORTING-PARITY.md#cli-catalogue-lm-studio-model-loading-and-profile-preferences)
adds the named provider catalogue, passive installation suggestions, reviewed
LM Studio model loading and eligible CLI profile preselection.
The [Swarm draft and command review checkpoint](PORTING-PARITY.md#named-swarm-drafts-and-command-batch-review)
adds named target setups, named completed rounds and one confirmation for the
exact commands currently ready across several targets.
The [scheduled Swarm capacity checkpoint](PORTING-PARITY.md#scheduled-swarm-target-capacity)
adds parallelism up to sixteen and up to 32 reviewed SSH targets for both
read-only advice and iterative runs.
The [reviewed snippet split checkpoint](PORTING-PARITY.md#reviewed-https-and-cli-snippet-splits)
adds masked HTTPS and CLI planning and generation for saved analyses, with a
fresh review for each outbound module or repair.
The [reviewed CLI answers and analyses checkpoint](PORTING-PARITY.md#reviewed-cli-journal-answers-and-snippet-analyses)
adds journal and cross-journal answers, single-snippet and folder analyses,
and a separately reviewed split after the outbound analysis is saved.
The [model-family prompt checkpoint](PORTING-PARITY.md#model-family-prompt-presets)
adds saved profile presets and effective prompts in the main reviewed workflows.
The [autonomous knowledge checkpoint](PORTING-PARITY.md#reviewed-autonomous-knowledge)
adds per-store opt-in and separately reviewed retrieval for Agent, Planning,
Swarm and the Scheduler.
The [AI setup and scheduled Qdrant checkpoint](PORTING-PARITY.md#ai-setup-and-scheduled-qdrant)
adds role/RAM-based GGUF recommendations, cloud presets, loopback model-server
discovery and offline reuse of approved Qdrant excerpts in the Scheduler.
The [guided local AI and knowledge scale checkpoint](PORTING-PARITY.md#guided-local-ai-and-knowledge-scale)
adds the managed GGUF setup and raises the explicitly reviewed index limits.
The [signed catalog, Swarm and journal checkpoint](PORTING-PARITY.md#signed-catalog-swarm-renaming-and-journal-status)
adds catalog refresh, completed Swarm renaming and classified journal totals.
The [manual, chat, memory and editor checkpoint](PORTING-PARITY.md#manual-cli-chat-previews-memory-cleanup-and-editor-tools)
adds the reviewed manual CLI path, native chat previews, reviewed AI Memory
cleanup, shared editor formatters and saved SFTP archive defaults.
The [Swarm, file comparison, table and Linux badge checkpoint](PORTING-PARITY.md#swarm-scheduling-file-comparison-ai-tables-and-the-linux-badge)
adds a disabled Scheduler handoff draft, reviewed comparison requests and
local file drafts, native Markdown tables, and the persistent LauncherEntry
counter.
The [Swarm controls, remote drafts and desktop exports checkpoint](PORTING-PARITY.md#swarm-controls-remote-drafts-and-desktop-exports)
adds one-target scheduled Swarms, per-target and whole-Swarm controls, a
reviewed SFTP new-file workflow, desktop export handoff and native chat math
and raster clipboard actions.
The [Scheduler groups, Swarm command sequences and browser journals checkpoint](PORTING-PARITY.md#scheduler-groups-swarm-command-sequences-and-browser-journals)
adds persisted group selection with fresh route validation, ordered exact
command batches and reviewed standalone journal HTML.
The [outbound editor and connection checkpoint](PORTING-PARITY.md#outbound-editor-workflows-and-saved-connection-window-geometry)
adds reviewed Journal note translation and CLI snippet assistance, SFTP AI
streaming, built-in Groovy indentation and private connection window geometry.
The [migration and file operation checkpoint](PORTING-PARITY.md#java-snippet-migration-password-commands-and-rsync-directory-upload)
adds private Java snippet library import, manual SSH and SFTP password programs,
Unix rsync directory upload to a new path, and prompt Windows AI stream cancellation.
The [file actions, drag transfers and knowledge checkpoint](PORTING-PARITY.md#protected-file-actions-sftp-drag-and-larger-knowledge-indexes)
adds typed Linux Scheduler rename and delete, SFTP drag and draft destinations,
larger local knowledge indexes, and an optional Linux ZIP. Fedora namespace
fixtures cover the privileged file protocol and filesystem operations.
Installed sudo over authenticated SSH remains an acceptance case.
The [local drop, protected copy and package checkpoint](PORTING-PARITY.md#local-sftp-drops-protected-folder-copy-and-linux-packages)
adds desktop copies in the Local pane, replacement and refresh of the
compared right local file, protected Scheduler folder copy, and optional
native Linux DEB and RPM staging.
The [privileged archive checkpoint](PORTING-PARITY.md#privileged-scheduler-archives)
adds a separately installed Linux helper mode with configured source and
destination roots, and an exact Scheduler approval for passwordless
server-only archive creation.
The [journal, desktop and knowledge checkpoint](PORTING-PARITY.md#startup-recovery-native-desktop-actions-and-larger-knowledge-stores) adds startup recovery for native interactive journals,
local Windows Quick Select file opening, a macOS status item, expanded
knowledge-source capacity, and a checked Linux server-file replacement path.

## Confirmed differences from the Java v2.26.0 release

The [tagged release notes](https://github.com/chardonnay/korTTY/releases/tag/v2.26.0)
were checked against the working source. Agent-to-Chat continuation, the Agent
profile role, AI Manager search, journal chat transcripts and exports, and the
native editor structure panel are present. The follow-up implements these
workflows and tracks their residual differences:

- **S1 — Editor tools.** Native Lua highlighting, completion, and LuaJIT
  detection are available in the snippet and file editors. Reviewed tool
  actions use installed StyLua and `luac -p`. Tool discovery includes the usual
  macOS locations for Finder and Dock launches. Verify the native UI and actual
  macOS launch environment. The reviewed tool menu covers Lua, shell, Rust,
  Go, Python Black and Ruby RuboCop. Native editors also offer built-in
  JSON, INI/Properties and Groovy indentation and reviewed installed tools for XML,
  YAML, TOML, HTML, CSS, JavaScript, TypeScript, Java, Perl, SQL and Terraform.
  Java bundles more formatter runtimes.
  Verify the newly added installed-tool routes on each native platform.
- **J2 — Journal summaries and progress.** Summary cards follow command
  boundaries and omit help-only calls and recognized failed starts. The
  reviewed chunk option processes retained long command output in multiple
  masked requests. The progress window can reopen while the pass runs and
  shows elapsed time and an estimate for planned work. The native viewer and
  HTML bundle use a 40-pixel minimum scrollbar thumb. Verify dragging on native
  desktops and in browsers that support the export's scrollbar styling. The
  Java and Rust filtered raw bundle counters both count retained non-partial
  input records. Rust also persists the classified command count for the
  full-journal header and exports, including recovery from retained capture.
  The startup pass also finalizes abandoned native interactive journals with
  automatic summaries disabled. A linked chat that is unavailable opens in a
  read-only transcript tab. Verify the tab from the viewer and the journal dock
  on native desktops. Review the running-card update cadence against Java.
- **P1/U3 — Startup and backup.** The managed password gate also supports an
  opt-in personal startup password. Java prompts by default and offers saved
  unlock. The native default remains off and saved unlock needs a scope
  decision. Encrypted profile backup now includes saved AI and Swarm chats,
  eligible AI memory, skill statistics, and AI model and knowledge-source
  metadata. Restore retains local memory/statistics and removes portable AI
  approvals. Reattaching locally reviewed assets to restored model IDs still
  needs a workflow. Complete native desktop and cross-host restore acceptance.
  Java AES-256 and legacy ZipCrypto backups can add supported connection,
  snippet, theme, project, credential environment and saved AI chat data
  through an omission-aware preview and
  startup restore. The Windows ARM64 fixture covers repeated startup replay.
  Java GnuPG archives for one supported recipient use the same preview and
  startup restore. Native fixtures cover the default GnuPG envelope and one
  compressed packet. Java chat UUIDs and environment IDs are preserved.
  AI chat restoration reuses standalone masking and retains Agent history
  without carrying provider authority or live links. Java Swarm conversations
  restore as read-only archives with messages, server outcomes and usage.
  Java SSH host keys have a separate fingerprint review in the backup dialog.
  The selected new keys are published through the native trust store.
  The Java SSH credential review imports supported managed private key copies,
  stored SSH passwords and inline target or jump passwords. The source master
  password and unlocked destination vault protect passphrases and passwords.
  Unencrypted managed keys can be reviewed without a vault. Active assignments
  bind to exact new routes without modifying colliding native entries. Startup
  checks the vault, key catalogue, managed destinations and connection catalogue
  before publishing and can replay a partially installed transaction. An
  unstarted stale import can be discarded from the startup error screen.
  Supported Java HTTPS AI profiles restore as drafts subject to current
  managed policy. Local profile names take precedence. The user reviews each
  destination in AI Manager and can choose a default separately. Java API keys,
  approvals, roles, other global settings, model registrations, and RAG
  definitions remain outside this import.
  Supported Java Scheduler command and snippet-script jobs restore disabled.
  They bind only to SSH connections added by that restore and to matching
  snippet and variable definitions. Existing job IDs take precedence.
  Unsupported actions, unresolved or conflicting references, group targets,
  Scheduler credentials, host pins, history, and webhooks are omitted.
  On Windows ARM64 the HTTPS restore and all four Scheduler restore tests
  passed. The application suite stopped at the porting pause after 2,351 tests
  passed with zero observed failures. The Scheduler crate passed all 37 tests,
  and strict application and Scheduler Clippy passed. The remaining application
  tests, native desktop review, and cross-host acceptance are open. Persisted
  temporary keys still need a scope decision.

The Rust packager stages ZIP archives for Windows and macOS and a TAR.GZ for
Linux. The optional `--linux-zip` flag emits a portable Linux ZIP and checksum
alongside the TAR.GZ. Matching Linux build hosts can also request
`--linux-deb` and `--linux-rpm`. Native install and clean-system verification,
and the Java MSI, DMG, Flatpak and Pacman formats remain in the release matrix.

The broader release audit also confirmed these workflow differences:

- **A3/B1/S3 — AI.** Native Swarm can save a named target-only draft for
  saved SSH connections and give a single approval to the ordinary commands
  shown across several targets. Native **Schedule…** stages a disabled
  draft for one or more eligible saved SSH routes. Interactive Swarm can
  pause, resume and restart an individual target, or restart the whole round.
  Restarts drain prior work and use fresh reviews. Rust also runs saved
  snippets through its reviewed Swarm mode. Java's standing round-wide
  approval extends to later commands that were absent from the initial review.
  A native batch includes the currently known ordinary commands in order per
  target. Later generated commands, privileged commands and managed per-command
  policy still require their own reviews. Native scheduled Swarm
  supports sixteen parallel slots and 32 selected saved SSH routes. Java can
  resolve current group membership including descendants. Native jobs persist
  selected groups and validate the effective route list at firing and dispatch.
  A membership or route change requires refreshing and reviewing the job.
  Larger groups need a separate capacity check. Reviewed CLI profiles serve journal summaries
  and scheduled Agent/Swarm alongside the reviewed HTTPS routes. The Java CLI provider
  catalogue has 18 named descriptors, seven with executable discovery hints,
  and built-in templates for Codex, LM Studio and MiniMax. The native catalogue
  offers the same names and passive suggestions for seven routes, including
  verified Node and package review for MiniMax. Reviewed LM Studio model load
  and eligible CLI role preselection are implemented. Saved snippet splits
  can review HTTPS or CLI planning and generation. The automatic follow-up
  after a saved analysis can offer a separate HTTPS or CLI planning review.
  Journal questions and regular snippet analysis accept reviewed CLI profiles.
- **B1/B5/J3 — AI profiles and knowledge.** Java has model-family prompt
  presets and a separate per-store opt-in for autonomous retrieval in Agent,
  Planning, Swarm and jobs. Rust supports those review flows alongside
  Text and Coding retrieval. Live review supports local indexes and Qdrant.
  Scheduler reviews support local, anonymous Qdrant and vault-backed Qdrant
  indexes. Larger corpus capacity remains open. Durable environment-key
  Qdrant is a native extension beyond the Java vault-key route.
  Native model-family presets and GGUF recommendations use the pinned
  catalogue. Cloud setup offers eight presets with offline examples. The guided
  GGUF setup is in the working source, with native acceptance pending. Signed
  catalog refresh and verified fallback are implemented. A live release and
  its independent publisher trust key are needed for production acceptance.
  AI Chat and Session Journal can hand verified exports to the desktop's
  registered application. The journal viewer can save standalone local HTML
  and open the verified file in the registered HTML application. Native
  handler launch still needs acceptance.
  The Java chat renders inline SVG, raster data URLs, Mermaid, LaTeX and
  Markdown tables. It offers copy actions for a whole table, column or cell.
  Native chat previews static SVG, Mermaid and embedded raster images in
  bounded local memory and retains source fallback. Native Markdown tables
  support table, column and cell copying. Fenced and display LaTeX math have
  a bounded native preview. Embedded raster images can be copied at their
  original resolution. Verify the native view and clipboard ownership on each
  desktop. Unsupported math remains available as source.
  Native AI Memory now has separate reviewed merge and shorten actions with
  a proposal diff and revision-checked apply. Complete native UI and live
  provider acceptance for both actions.
- **T3/Q1/Q2 — File operations.** Java's SFTP manager has drag transfers between
  panes and bounded external drag-out, including from the terminal file
  sidebar. Native drag transfers between the panes use the same file manager
  queue as its upload and download actions. Native desktop drops in the Server
  pane use the same upload queue on Windows, macOS, X11 and Wayland. Drops in the
  Local pane copy files and folders with no-replace naming, including with
  SSH disconnected or remote transfer denied. Native Windows, Wayland and X11 can also offer
  Local pane selections to other applications using its drag grip. The Server
  pane prepares up to 20 files and 16 MiB on the verified SFTP connection before
  offering the same native Copy gesture. The terminal's Remote files sidebar
  uses the same preparation and Copy source for its current regular-file
  selection. Native desktop acceptance and drag-out on macOS
  remain open. Native archive dialogs have per-archive options and saved
  remote directory and
  compression defaults. The Java transfer settings exchange carries
  explicitly provided archive fields. The SFTP file comparison can review
  an AI explanation request or generate a draft from both versions. Prompt
  presets are editable. A draft can open as a new local file or unsaved
  snippet, or be reviewed for publication under a new name in a canonical
  directory on the same authenticated server connection. The server operation
  publishes only exact reviewed bytes and requires OpenSSH-compatible
  no-replace hard links. Streaming profiles show masked partial responses and
  can stop before publication. Rust can replace a separately selected existing
  local file with an optional new `.bak` copy. The compared right local
  file can be selected directly and the comparison refreshes from the same
  pair after success. The right compared server file can also be replaced with
  a reviewed optional `.bak` through the native Linux helper. It requires a
  protected directory and matching ownership, bytes and access metadata.
  Extending the workflow to another existing server file and other server
  platforms remains open.
  Native UI and configured provider acceptance remain open.
  The remaining Scheduler replacement, rsync and sudo variants are tracked
  in Q1 and Q2.
- **D2/P1 — Connection settings and credentials.** Java stores per-connection
  outer window geometry in its editor and private connection XML. The pinned
  Java launch reads project and global geometry. Its portable connection
  exporter and Duplicate omit that machine-local value. Native saved SSH
  connections can edit and import the private value. **Connect in New Window**
  applies it without changing the shared window or remembered global placement.
  Manual SSH and SFTP can use a private vault password program with exact
  executable and arguments, reviewed SSH pins and one approval per launch.
  Java also supports free-form shell commands. Define any additional shell,
  Java credential migration or unattended support before implementing it.
  Persisted temporary keys and remembered vault unlock need a scope decision.
- **D5/A4 — Coding-agent indicators.** Native badges exist on macOS and
  Windows. Linux emits a persistent LauncherEntry D-Bus counter with an
  installed desktop entry. The native bus and count lifecycle have a
  Fedora fixture. Visible dock acceptance remains open.

## Basis and confidence

- Java reference is the exact `v2.26.0` tag at
  `b235d91f20356f8a9b33ad5398550a043cc21ed2`. The earlier audited release
  commit `1ebbe724ee84d55f31295ec3060eecad3653d2f2` differs only in
  `package/size-baselines.json`. The latest spot checks use the tagged tree.
- Rust is `main` at `f463f9ce2d14d1805d8c5fd170467490ed155427`,
  including the substantial current uncommitted implementation. HEAD alone
  does not represent the audited application. Record the exact source
  snapshot and executable hashes for subsequent verification.
- SithTermFX remains v1.2.7 at
  `8b02de38b7c6396f1af2558b0b00477cfb9a2871`. Retained reference fixtures
  and their recorded runs were inspected. This audit did not repeat an
  exhaustive opcode comparison or regenerate fixtures from a fresh checkout.
- Three parallel read-only reviews covered desktop/editor/journals,
  AI/agents, and transport/scheduler/policy. The review compared current
  entry points, persistence, pinned Java behavior and verification records.
  That original audit did not run new builds, native GUI tests or real-provider
  tests. The subsequent D1/A1 implementation checks are recorded below.
- **Implementation** identifies a demonstrated missing behavior.
  **Acceptance** identifies implemented behavior needing specified live or
  platform evidence. **Audit** requires a bounded reference comparison or
  scope decision before prescribing code. **Resolved** closes a proposed
  requirement that the pinned reference does not require. A row can have
  more than one state. Completion percentages are not inferred from tests
  or file counts.

In source references below, **J** means
`src/main/java/de/kortty/` at the pinned Java commit and **R** means this
Rust working tree. Paths describe the audited snapshot, not an installed
user executable.

### Current evidence and distribution

The [Step 4 implementation](PORTING-PARITY.md#interactive-swarm-reviewed-ai-providers-and-persistent-indexes)
covers saved Swarm rounds, reviewed Agent and Terminal Chat provider paths,
local tokenizers, and knowledge and journal persistence. Native Windows ARM64
passed 3,340 workspace tests with 54 ignored. Clippy passed on Windows and as
a macOS ARM64 cross-check. The Windows PDF helper fixture passed at the new
input boundary. See the
[verification record](target/verification/20261112-step4-ai-workflows.md).

The [Step 3 implementation](PORTING-PARITY.md#sftp-file-operations-and-server-only-archives)
covers the attached-session lane cap, resume and cancellation preferences,
browser file actions, server-side archive creation, reviewed Unix rsync upload,
and typed sudo file actions. Native Windows ARM64 workspace verification
passed 3,328 tests with 54 ignored, including the resume, cancellation and
replacement regressions. The Fedora archive helper suite passed 17 tests
and Clippy. Fedora also passed native OpenSSH file editing and both
scheduled rsync directions with the application bridge. Native desktop and
installed-helper acceptance remain open in T1–T4 and Q2. See the
[verification record](target/verification/20261112-step3-file-operations.md).

The [Step 2 implementation](PORTING-PARITY.md#connection-input-settings-and-managed-policy)
covers connection input overrides, Control and MCP policy, SFTP transfer
restrictions, the managed application update feed, and encrypted recovery
output. Native Windows ARM64 workspace verification passed 3,316 tests
with 54 ignored. Clippy and the macOS ARM64 cross-check passed. See the
[verification record](target/verification/20261112-step2-connection-policy.md).

The D1/A1 checkpoint records **3,305 passing Windows workspace tests
and 53 ignored tests across 67 test summaries**. This includes **2,713 passing
application-binary tests and 50 ignored tests**; these counts are not additive.
Windows Clippy with warnings denied and formatting passed. The macOS ARM64
all-workspace/all-targets check passed as a cross-check, not native execution.
Native ConPTY tests covered PowerShell 5.1 and CMD Agent routes and opted-in
Git Bash `-i`/`-il` startup integration. A native SSH Agent route against Fedora
passed; the Fedora PTY subset recorded **29 passed and 4 ignored**, with Clippy
passing. These are automated execution checks, not a completed manual GUI matrix.

See [the shell/Agent checkpoint](PORTING-PARITY.md#local-shell-integration-and-terminal-agent-recognition)
and its [verification record](target/verification/20261112-shell-integration-agent.md).
The earlier automation/SFTP checkpoint recorded **2,694 passing
application-binary tests and 49 ignored tests**. Its private Windows ARM64 GUI
checks covered the managed AI default, colliding personal profile IDs,
policy-locked SFTP preferences and the corrected Settings tab scrollbar.

See [the recorded checkpoint](PORTING-PARITY.md#scoped-automation-capture-and-sftp-conflict-settings)
and the local
[verification record](target/verification/20261112-automation-journal-and-sftp-policy.md).
A real SSH conflict under the managed choice and a successful scheduled
command under an installed automation-capture denial still need acceptance.

The shell and Agent Windows ARM64 static-CRT release ZIP passed private-profile
startup/shutdown smoke: read-only Control ping, exactly one owned restored
PowerShell pane, graceful exit and removal of the extracted package/profile.
The verified [ARM64 package](target/dist/kortty-0.1.0-aarch64-pc-windows-msvc-20261112-shell-agent/kortty-0.1.0-aarch64-pc-windows-msvc.zip)
and its [SHA-256 sidecar](target/dist/kortty-0.1.0-aarch64-pc-windows-msvc-20261112-shell-agent/kortty-0.1.0-aarch64-pc-windows-msvc.zip.sha256)
are retained in a dedicated directory. The older ARM64 archive is also retained
because automatic approval review blocked moving, replacing or deleting it
without a further stated reason. This is a specific retention exception.
The Step 2 source checkpoint awaits packaging under V7. The x64 ZIP still
needs a current build and verification. The exact executable and
configuration used in the user's originally reported failing session have
not been identified.

## Scope and recorded decisions

- Native Windows, macOS, Wayland and X11 remain required. The highest product
  priority remains usable Java-equivalent windows, dialogs, menus and tool
  windows. Every implementation step includes its corresponding UI.
- Preserve native Rust and OS APIs and the current Iced architecture.
  No required Java runtime, WebView or browser renderer. Both Mosh modes
  remain excluded.
- The smaller native editor still requires class/function/variable/placeholder
  navigation. All currently implemented scanners must be evaluated before
  adding a missing-language ticket.
- Application text, source comments and repository documentation stay English.
  Java application localization is not scheduled. User-content translation
  is a separate implemented AI action.
- Platform TLS for HTTPS AI was approved. The bounded RustCrypto `cmov`
  exception was also approved for the corrected SSH RSA implementation.
  Neither is a pending authorization or an unimplemented RSA feature.
- Real IBM/HP and physical serial tests are **deferred by the user**.
  Keep them visible under V4 without blocking independent software work.
- Optional providers, MLX, external editors, rsync and video encoders remain
  explicitly enabled integrations. Core terminal use remains independent.
- S4's durable folder Apply/POST proposal is resolved as outside confirmed
  Java parity. Durable single-snippet recovery already exists.
- JVM heap/GC/JMX and JAR loading require native capability mapping, not a
  literal JVM port. All eleven supplied terminal effects are implemented.
- Full Monaco formatting/rulers/named profiles, arbitrary third-party effect
  extensions and optional telemetry/feedback need a recorded product
  disposition. Do not introduce a collection endpoint to close a table row.
- Existing bounded authority and privacy choices remain explicit. Imported
  Java auto-approval does not grant command execution authority. Typed journal
  capture stays opt-in in Rust, unlike Java's default-on field. Test explicit
  and omitted imported values. Rust limits interactive Swarm to eight targets.
  Scheduled Swarm parallelism is capped at sixteen with up to 32 selected SSH
  routes. The provider worker pool and iterative SSH/journal limits still
  apply. Review larger dynamic-group workflows before increasing that bound.

## Implementation order

These are dependencies and priorities for continuing implementation, not a claim
that only these items remain. D4/V1/V2 native UI acceptance runs alongside each
step. All work-package IDs from the previous plan are retained below.

1. **Finish acceptance and distribution of the shell/Agent checkpoint.** The
   scoped A1 implementation now includes PowerShell/CMD prompt integration,
   setup/status and the SSH credential-mask initialization needed for terminal
   AI without capture. D1 now supports opted-in custom-shell startup and the
   Java integration field, including supported default-shell project routes.
   Windows PowerShell 5.1/CMD, Git Bash startup and native SSH Agent routes have
   automated evidence. The current ARM64 release ZIP passed private-profile
   startup, restored-pane and graceful-exit smoke. Complete PowerShell 7, live
   Zsh/Fish and native desktop acceptance, plus current x64 and other target
   package verification under V7. Preserve the documented CMD and
   custom-program Run limits.
2. **Verify the connection and policy implementation.** D2 now saves paste
   line delay and strict terminal overrides. A5/P1 applies explicit Control
   allow, locks the managed setting, and normalizes feature keys and values.
   P1 preserves permitted remote browsing and server operations under transfer
   denial and applies the managed update feed and encrypted recovery output
   rules. Complete the native checks in D2, A5 and P1 and continue the bounded
   schema audit.
3. **Complete the file-operation workflows.** T1/T2 now enforce the attached
   lane cap and implement resume and cancellation settings. T4 adds browser
   image opening and bounded ordinary external-file editing. Q2 adds server-only
   ZIP, TAR, TAR.BZ2 and 7z creation for the browser and Scheduler, plus reviewed
   Linux sudo mkdir, chmod, chown and regular-file copy to a new path. Q1 adds
   reviewed single-file rsync upload on Unix. Complete native acceptance and
   resolve the T3 replacement publication limits before expanding scheduled
   replacement and privileged existing-target transfer. The Q1 and Q2 rows
   list the remaining modes.
4. **Complete AI workflows and data persistence.** A3 implements interactive
   Swarm save and continuation, group selection, history search, exports and
   journal controls. B1/B8 add provider presets and separately reviewed
   LM Studio integrations. B7 implements selectable tokenizers. J3 persists
   semantic vectors for reviewed journal cards. B6 extends source and index
   capacity. The remaining provider, capacity and acceptance work is in the
   corresponding rows below.
5. **Complete platform isolation and bounded audits.** T5 adds local PTY
   sandboxing on Windows/macOS. The S1/S5/U3/P1 audits record concrete
   pinned-source cases and implementation or acceptance dispositions in
   the rows below.
6. **Close the release matrix.** Run V1–V7 against a fixed reviewed snapshot,
   including native architectures, actual enabled providers, clean-system
   packages, dependency/security review and comparative performance. U1's
   owner-published release and the deferred hardware cases have separate
   prerequisites and must be reported honestly.

## Desktop, connections and terminal behavior

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| D1 | Acceptance. Saved local-shell program/arguments/directory and launch routes now preserve the integration opt-in. Supported custom Bash/Zsh/Fish startup flags and qualifying Windows executable paths use private startup wrappers; PowerShell/CMD presets are supported. Java `shellIntegrationAutoInject` is imported/exported, the legacy native flag remains readable, and conflicting flags are rejected. Supported default-shell Java project routes preserve the opt-in and validate the saved catalogue identity. R `src/connection_store/local.rs`, `src/connection_window/local.rs`, `src/profile/projects/java/routes.rs`. | Verify native launch/edit/restart, live Java exchange, PowerShell 7 and Zsh/Fish startup on the remaining native platforms. J `core/LocalShellTtyConnector.java`, `shellintegration/ShellIntegrationInjection.java`. Preserve exact-argument native extensions. Portable Java project routes requiring a custom command, working directory or Teamwork integration remain explicitly omitted; machine-local identities must not be published as portable routes. |
| D2 | Acceptance + bounded audit. Saved SSH, local shell and serial settings support inherited paste line delay and strict terminal overrides, including Java connection XML. HP2392 paste uses its existing line handling. Themes, independent appearance/scrollback/PTY dimensions, keepalive, project split presentation, local image backgrounds and remote hosted tool restoration also exist. J `model/ConnectionSettings.java`, `ui/ConnectionEditDialog.java`. R `src/connection_store/terminal.rs`, `src/connection_window/terminal_overrides.rs`, `src/workspace/paste_protection.rs`. | Verify native editing, launch, reconnect, paste routing, Java exchange, and the theme/project/sizing/authentication workflows. The user-deferred HP and physical serial acceptance remains in V4. Verify standalone saved SSH window geometry, private Java XML import, and portable omission. |
| D3 | Acceptance + bounded audit. Named highlight sets, per-connection selection, bounded network matchers, rule editor/preview, interchange, notification and reviewed snippet triggers exist. R `src/terminal_highlight.rs`, `src/workspace/highlight.rs`. | Native pinned-set acceptance under changing output, palette, policy and snippet authority. Audit a concrete incompatible custom Java expression before extending the bounded matcher. Do not reimplement the existing set picker or master controls. |
| D4 | Acceptance. Native lifecycle, placement, menu routing, hosted/detached tools and Settings/AI Manager keyboard guards exist. Selected Windows ARM64 default-selection, dirty-close and tab-label behavior is verified. Quick Select opens files in native Windows PowerShell/CMD panes. macOS has a separate status item for New Window and Quit and App Nap activity scopes for recent terminal I/O, running jobs and active AI requests. | Verify the status item with zero windows and observe App Nap activity on a packaged Mac build. Complete V1/V2 across native desktops. Cover fresh-profile size, footer reachability, preferred geometry, toolbar overflow, keyboard traversal, owner modality, dirty close and search focus in both hosted and detached forms. |
| D5 | Acceptance. General BEL/OSC, shell, highlighting, Chat, Agent/Swarm and Scheduler notifications exist. Windows OSC 777 click routing to the original inactive split was measured. The Linux LauncherEntry backend emits on a persistent D-Bus name and the portable archive stages a matching desktop entry and icon. | Verify the Linux counter on a supporting launcher and complete native sudo/password waits, moved/closed/stale targets, suppression/coalescing and revocation. Complete macOS and Linux delivery/activation under V3. |

Evidence for the completed D2/D3 work includes
[terminal dimensions](PORTING-PARITY.md#initial-terminal-size-for-local-shells-and-ssh),
[Java project dimensions](PORTING-PARITY.md#java-project-initial-terminal-dimensions),
[theme exchange](PORTING-PARITY.md#java-theme-and-project-appearance-exchange) and
[named highlighting](PORTING-PARITY.md#reviewed-changed-file-batches-and-named-highlighting-selection).

## Built-in agents and external coding-agent control

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| A1 | Acceptance. Built-in Run/Ask/Plan, reviewed local/SSH execution, skills/quota, bounded history, opt-in `agent` aliases, Tab completion, sudo/vault challenges and mirror holds exist. PowerShell/CMD integration and per-pane setup/status now ship in source. Native ConPTY PowerShell 5.1/CMD and Fedora SSH Agent routes passed. Permitted terminal AI initializes an in-memory SSH credential mask independently of capture, including incognito, without enabling logging. R `src/workspace/terminal_agent/shortcut.rs`, `src/workspace/ssh_access.rs`, `src/session/shell_activity.rs`, `crates/pty/src/shell_integration.rs`. | Verify remaining supported shells, native desktops and current packages. Complete native manual password entry on macOS/X11/Wayland, history/run controls, mirror release, changed targets and revocation. Custom-program Agent Run remains blocked by the native-shell capability gate. See the diagnostic contract below. |
| A2 | Acceptance + bounded audit. Planning stages, design module IDs d01–d20, docking/status/approval placement, inline history, picture-in-picture, usage/repair display, seven activity export formats and Agent-to-Chat/Journal links exist. R `src/terminal_agent/design/custom.rs`, `src/workspace/terminal_agent/`. | Native d14 cursor review, d19 snap/follow/detach, d20 behavior, inline selection/Kitty row mapping, exports, linked Journal cards and live provider usage/repair. There is no demonstrated missing design-module ID. |
| A3 | Implementation + acceptance + bounded audit. Live and saved targets, named pre-run saved SSH drafts, completed interactive rounds with renaming, reviewed continuation, connection groups and working-directory metadata, history search, Markdown/text/PDF export, saved journal defaults, estimate and journal links are implemented. A batch review binds one approval to the displayed ordinary commands and target identities. R `src/workspace/terminal_agent/swarm.rs` and its `batch.rs`/`comparison.rs`/`conversation.rs`. | Verify native renaming, selection, batch review and revocation, comparison, failure isolation, workflow drafts and continuation over pinned SSH. Evaluate Java's standing batch policy and incognito target scope against native authority requirements. Saved follow-up uses reviewed saved SSH routes. |
| A4 | Acceptance. All eight pinned Agent verbs, events, wait/start/rename and `notification.show` have native Control/CLI routes. Windows socket backpressure and native notification submission have evidence. | Native visible delivery and exact focus/target routing, stale agents, moved splits, timeouts and losses across desktops. Admission/native submission receipts alone do not prove a displayed notice or activation. |
| A5 | Acceptance. Explicit `control-api = "allow"` starts the listener and locks the setting while retaining the personal preference. The parser normalizes feature keys and values. MCP stdio keeps its independent opt-in, retains its write tools preference while disabled, and gates writes on effective policy and consent. Masked reads, reconnect identity guards and managed MCP denial exist. | Verify managed activation, lock display and relaxation on native desktops. Complete affirmative write consent and invalidation across those desktops. P1 owns shared policy acceptance. |

### Agent shortcut diagnostic and implementation contract

The built-in Agent exists. Its terminal shortcut is enabled separately and
defaults to off. Recognition requires fresh OSC 133 A/B prompt markers, a live
UTF-8 Xterm primary screen at its bottom, and no password prompt, incompatible
full-screen application or competing run ownership. A previous-prompt marker
alone does not prove that OSC 133 B marked the prompt end and input start.

Five integration snippets now ship: Bash, Zsh, Fish, PowerShell and CMD.
Automatic integration is a separate local-shell opt-in and remains off by
default. Supported presets and validated custom-shell startup arguments use
private startup files or fixed launch commands without editing user profiles.
The Java pin supplies the three POSIX snippets; native Windows integration is
a cross-platform usability addition. Disabled shortcuts preserve ordinary
shell input, and interception requires a newly verified empty prompt.

PowerShell 5.1 and CMD have native ConPTY Agent-route evidence; PowerShell 7
and live Zsh/Fish remain acceptance items. CMD uses the standard path prompt
and emits OSC 133 A/B only. It does not provide C/D command boundaries,
exit status, duration or OSC 7 directory reports. Per-pane setup/status explains
the relevant opt-in, prompt and capability requirements.

For native SSH connections, install the snippet in the remote user's actual
shell startup file and reconnect. Automatic local injection does not install
remote startup files. Prompt/directory markers must arrive through that SSH
pane. Reconnect older SSH sessions to initialize the credential mask required
for permitted terminal AI. This mask does not require logging, journals or
MCP and does not turn on persistent capture in incognito terminals. Running
an `ssh` subprocess inside a local pane does not turn it into a native SSH
target. The menu entry **AI → AI Agent…** already supports reviewed local
starts, including Windows absolute-directory review. Custom-program terminals
can receive prompt integration, but Agent Run remains blocked by their
native-shell capability gate.

Acceptance must record the exact executable, opt-in, shell, received markers,
`agent`/Tab behavior and correct pane identity. Do not infer the user's root
cause from a repository source check or an old ZIP alone.

## AI foundations and AI Manager

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| B1 | Implementation + acceptance. Managed/local, native LM Studio, OpenAI-compatible HTTPS, Anthropic and reviewed configured CLI profiles exist. The named CLI catalogue provides passive native path suggestions, fixed presets and reviewed LM Studio model loading. The Manager offers catalog-based GGUF recommendations by role and RAM, eight cloud presets with offline examples, and anonymous loopback model discovery. The separately signed model and prompt catalog verifies the current sequence and falls back to the verified cache or bundled data. The guided setup reviews the signed runtime and exact model files, runs a local test for each model, and saves the selected roles together. Standalone Agent Ask/Run, interactive and scheduled Swarm and Agent, and journal summaries use reviewed HTTPS or CLI requests. | Verify the live signed catalog with the publisher trust key and audit the remaining specialized prompt consumers. Verify a real guided install, inference, cancellation and role save on native desktops. Verify the reviewed routes with enabled providers and native UI, including MiniMax login and regional endpoints, live LM Studio loading, macOS executable quarantine and owned-helper cancellation. J `core/AiCliProviderRegistry.java`, `core/AiServiceFactory.java`. |
| B2 | Acceptance. Bounded answer/reasoning streams and provisional analysis/progress are implemented for supported native providers. Generic CLI buffering until exit also matches the pinned CLI behavior. R `src/local_ai/streaming.rs`. | Real provider interruption, protocol errors, cancellation and complete saved/exported final results. Do not list streaming itself as absent. |
| B3 | Acceptance. Reviewed single/split GGUF acquisition, resume, signed runtime channels, Off/Notify/Automatic Stable, version pins, withdrawals and idle staged activation exist. | Signed-channel interruption, disk limits, withdrawn packages and actual supported runtime execution on each native target. Preserve leases and last verified usable installation through failures. U1 is the separate application-update channel. |
| B4 | Acceptance. Private/external GGUF sets, projector pairs and signed MLX offline/online install, removal, update policy, pinning, withdrawal and idle activation exist. | Real signed MLX package/model on Apple Silicon through startup, streaming, idle exit, cancellation, rollback, remove/reinstall and updates. Native GGUF vision/external-reference acceptance remains. Cross-checks and x64 emulation do not prove native runtime behavior. |
| B5 | Acceptance. Persistent indexes, approved automatic refresh and per-store opt-in exist. Agent Ask/Run, Planning, Swarm members and comparison have separate query and prompt reviews. Scheduler proposals, per-firing Agent and Swarm, read-only Swarm and optional scheduled comparisons can include exact reviewed excerpts from local and anonymous or vault-backed Qdrant indexes. | Verify native Manager and request previews, cancellation, saved/reopened jobs and source/embedding changes with configured providers. Durable environment-key Qdrant is a native extension. Compare larger corpora under B6. J `ui/RagKnowledgeStorePane.java`, `rag/RagRuntimeService.java`. |
| B6 | Implementation + acceptance. UTF-8/PDF ingestion, page citations, persistent local HNSW and optional Qdrant exist. Streaming UTF-8 supports a newly reviewed 50 MiB text file. PDF stays at 50 MiB and a store supports up to 5,000 source files. Local and Qdrant snapshots support up to 16,384 passages in 96 MiB and use bounded exact local ranking above the 8,192-passage HNSW limit. The existing settings-size, per-file and snapshot bounds still apply. Earlier automatic approvals retain their original discovery and Qdrant bounds until reviewed again. | Verify native macOS PDF helper/cancel, larger-corpus peak memory, native limit/Vault controls and configured HTTPS Qdrant recovery. J `rag/RagSourceScanner.java`, `rag/LocalHnswStore.java`. |
| B7 | Acceptance + bounded audit. Connection skills, reviewed relevance/classification, usage ledgers, forecasts and graphical profile quota meters exist. Profiles can select Estimate, CL100K, O200K, P50K or R50K for reviewed request forecasts. J `model/AiTokenizerType.java`. | Audit other consumers for exact missing forecast/usage paths. Complete native meter and real-provider accounting acceptance. |
| B8 | Acceptance + bounded audit. Reviewed local/SFTP text attachments, saved masked snapshots, native flowcharts, direct Tavily and the five LM Studio MCP internet modes exist. The search binds its reviewed query, integration and allowed tools. The answer is separately reviewed. J `model/AiInternetAccessMode.java`. | Verify attachment identity, size and change, archived follow-up and actual configured MCP services. Compare any additional provider tool execution against concrete reference use cases. |

Recent evidence includes
[LM Studio discovery](PORTING-PARITY.md#scheduler-journal-details-java-retention-and-ai-provider-discovery),
[local HNSW](PORTING-PARITY.md#persistent-local-hnsw-knowledge-search),
[Qdrant](PORTING-PARITY.md#optional-qdrant-knowledge-backend),
[reviewed Agent Run](PORTING-PARITY.md#reviewed-standalone-terminal-agent-run-requests),
[interactive Swarm comparison](PORTING-PARITY.md#reviewed-ai-comparison-of-completed-swarm-targets)
and [signed MLX acquisition](PORTING-PARITY.md#bash-declarations-signed-mlx-acquisition-journal-chats-and-attachment-quotas).

Ask the manual uses the configured Text and translation role or default,
including a reviewed CLI profile, with the same masked effective prompt in
the review and the worker. Journal note translation, screenshot descriptions,
the journal topic export filter, ASCII Art and skill shortening apply the
selected model-family prompt preset. Note translation prefers the local
Text and translation role or default and retains the previous local Journal
configuration for profiles without either assignment. Outbound journal
note translation uses a separate masked review for saved HTTPS and CLI
profiles. Native desktop and configured-provider acceptance remain open.

## Editor, snippets and diagrams

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| S1 | Acceptance. Rust, Python, shell, Markdown, AsciiDoc, Dockerfile, SQL, Perl, Ruby, Lua, PowerShell, Java, Groovy, JavaScript and TypeScript structure scanners exist. Python recognizes `with … as`, `except … as`, unpacking and initial augmented assignments and classifies `dict()` and `tuple()` bindings. Lua syntax and completion, LuaJIT detection, and reviewed native format and syntax actions are implemented. | Verify native jumps after edits, nested scopes, repeated names, Unicode, placeholders, and editor tool review, cancellation and SFTP save. Verify discovery and launch from Finder or Dock. The [Step 5 product audit](target/verification/20261112-step5-product-audits.md) records the reference structure cases. |
| S2 | Acceptance. Completion/improvement/correction/translation/descriptions/alternatives/security actions, description-only Apply/Undo, compact/embedded commands and reviewed HTTPS and CLI assistance exist. | Configured providers and native selection boundaries, changed source, discard/undo, original-profile reruns and explicit clipboard review on each desktop. |
| S3 | Acceptance. Hardening, validation, script headers, optional modularization, durable selective publication/recovery and workflow diagrams exist. Manual split planning and per-module generation use reviewed HTTPS or CLI alongside local and managed profiles. Outbound reviews bind the saved source, profile, credential context and current job, and are renewed after restart. Single-snippet and folder analyses accept saved CLI profiles. After a saved HTTPS or CLI analysis, the opt-in follow-up opens a separate split preview for that exact report and profile. | Verify actual configured providers, native optional-chain/option-only flow, language eligibility, collisions, cancellation, selected-file publication and restart recovery. |
| S4 | Resolved. Pinned Java project/folder Apply uses task-local preview state and saves accepted files without durable project Apply/POST records. | No new durable folder Apply/POST implementation is required for confirmed parity. Existing single-snippet durable recovery/POST and S3 modularization remain in scope. |
| S5 | Implementation + acceptance. Native per-snippet diagram persistence, selection/revision binding, stale previews, source jumps, regeneration/deletion, structured interchange and backup exist. Private Java JAXB `<snippets>` library migration retains diagram ownership, source digest, selection bounds and stale references. | Verify private-library migration on native desktops and review unsupported diagram formats against concrete examples. Java standalone snippet exports omit diagrams. Native focus/navigation/restart/whole-scene export remains acceptance. |

Evidence includes
[snippet HTTPS assistance](PORTING-PARITY.md#correcting-snippet-descriptions-and-reviewed-https-assistance),
[workflow publication](PORTING-PARITY.md#workflow-settings-flowcharts-and-snippet-publication),
[saved diagram navigation](PORTING-PARITY.md#folder-retry-and-saved-diagram-source-navigation)
and [Python bindings](PORTING-PARITY.md#model-policy-enforcement-and-python-structure-bindings).
The seven supported native diagram families and PDF/SVG/PNG exporters exist.
Their complete interaction and export matrix belongs to V5.

## Session journals

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| J1 | Acceptance. Per-connection automatic capture, opt-in typed input, XML settings, raw evidence, screenshots, redaction and retention exist. | Native input-off/on, imported omitted/explicit values, command boundaries, reconnect, duplicate/import/export, limits and restart. Preserve screenshot consent and the native typed-input privacy default. |
| J2 | Implementation + acceptance. Local/managed periodic and closing summaries, durable catch-up, re-evaluation, backfill and screenshot re-description exist. Command cards, help and failed-start filtering, the reviewed long-output mode, classified command counts in journal metadata, startup finalization of abandoned native interactive journals, reopenable progress details, and row/group status badges are implemented. Reviewed HTTPS and CLI approvals bind the profile revision, journal root, source identity, redaction rules and output mode. Live, entry and screenshot note editors offer separate reviewed HTTPS and CLI translation. The native journal viewer and compatible browsers for HTML bundles have a 40-pixel scrollbar thumb minimum. R `src/session_journal/summary_job.rs`, `src/workspace/journal_access/summaries.rs`. | Verify native close and count recovery, scheduling, cancellation, restart, pending deletion and configured HTTPS and CLI providers. Verify local vision, per-connection overrides, desktop progress lifecycle and native scrollbar dragging. |
| J3 | Acceptance. Reviewed local, HTTPS and CLI Q&A, lexical cross-journal search, reciprocal-rank fusion and exact capture/entry/chat citations exist. Separately reviewed local semantic ranking reuses a private per-journal vector for the exact document, masked card and model. Stale and unavailable cache entries use fresh reviewed embeddings or lexical fallback. | Verify native citation routing, stale-source/policy rejection, recorded chats and configured providers. The CLI route uses one approved invocation and validates exact quotations from the reviewed evidence. The semantic scope follows the reviewed candidate shortlist. J `core/SessionJournalAskService.java`, `core/SessionJournalSemanticIndex.java`, R `src/session_journal/question/rank.rs`. |

See [HTTPS journal answers](PORTING-PARITY.md#reviewed-https-journal-answers-and-retained-scheduler-totals)
and [journal search and recovery](PORTING-PARITY.md#source-selection-managed-recovery-local-file-ownership-journal-search-and-mlx-removal).
The latter evidence distinguishes journal ranking from persistent knowledge-store indexing.

## SSH, SFTP and serial

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| T1 | Acceptance. Hosted/detached workbench, parallel lanes, idle protection and transfer limits exist. Attached sessions use at most two transfer lanes. A managed cap of one locks the SFTP parallel preference and preserves the personal value. J `ui/SettingsDialog.java`, R `src/sftp_window/parallel.rs`. | Verify native large and colliding transfers, cap changes while lanes drain, idle settings, and hosted/detached behavior. |
| T2 | Acceptance. Durable single-file downloads/new uploads, reviewed replacement-upload resume, folder child retry and recovery exist. Settings and Java SFTP preference interchange include `sftpResumePartialTransfers` (default on) and dependent `sftpKeepPartialOnCancel` (default off). Turning resume off releases retained offers and history pins and changes waiting retries to fresh copies. | Verify live Java preference exchange, native Linux/macOS interruption and restart, real parallel-lane retries and remote cancellation cleanup after a lost connection. Remote stage removal requires verification of the authenticated route and stage ownership. |
| T3 | Implementation + acceptance. Browser child-queue folder merge, selective retry, regular-file source links, changed-file batches, destination-only removal and same-server copy exist. Ordinary browser/Scheduler download inheritance and P1's transfer-policy separation are implemented. One-use Windows Scheduler download replacement uses held-object rename, retained recovery copies and content-bound approval. | Verify extension absence without client-payload fallback, collisions, foreign-owner writes and changed destinations. Unix and remote replacement and privileged existing-target operations still need an appropriate compare/publication protocol. |
| T4 | Implementation + acceptance. Native text and external file editing, browser **Open image**, Linux guarded elevated text writeback, and reviewed AI comparison with editable presets and local/unsaved snippet drafts exist. A review can publish a new draft file to a chosen canonical directory on the same authenticated server connection. Local draft replacement reviews an existing target, can create a new `.bak`, and can replace the compared right local file and refresh the exact pair. The right server file can be replaced through the checked native Linux helper with optional `.bak`, preserving supported access metadata and reloading the exact pair. J `ui/SFTPManagerTab.java`, R `src/sftp_window/worker.rs`, `src/sftp_window/compare/ai.rs`, `crates/ssh/src/sftp/edit.rs`. | Verify native UI and configured providers, close/reconnect, bounded masked streaming, cancellation and publication. Verify the native Linux helper via direct and isolated SSH. Extend replacement to other reviewed existing destinations and supported server platforms. Verify native image opening, editor launch/writeback/disconnect/recovery and elevated writeback with a real authorized sudo account. |
| T5 | Implementation + acceptance. Local PTY sandbox launch paths use Linux Landlock/seccomp, macOS Seatbelt and Windows Process Security Environments. Fedora ARM64 local-shell and worker containment, Windows ARM64 ConPTY containment, and native Apple Silicon Seatbelt and packaged SSH isolation passed. Windows uses CMD with Win32k lockdown and rejects the Windows PowerShell preset in sandbox mode. | Run local-shell and packaged SSH fixtures on native macOS Intel and Windows x64, including interactive behavior and containment. Track broader Windows shell compatibility. Direct, process-isolated and jump RSA passed on Apple Silicon. V6 still tracks the external review and active dependency advisories. See the [native Mac verification](target/verification/20261112-step6-macos.md). |
| T6 | Acceptance, hardware deferred. Saved serial connections, encodings, grouping/tags/favorites/pins, mixed library actions and import/export exist. | Native Manager selection/actions/interchange. Physical break/modem/flow-control tests remain user-deferred under V4. Serial library organization is no longer an implementation gap. |

Java drag transfers and global archive defaults are in `ui/SFTPManagerTab.java`,
`ui/TerminalRemoteSidebar.java` and `ui/SettingsDialog.java`. The native queue,
archive dialog and transfer preferences live in `src/sftp_window.rs`,
`src/sftp_window/archive.rs` and `src/workspace/settings_window/sftp_settings.rs`.

### File operation contracts to preserve

Ordinary browser folders merge and publish per child. Completed children remain
after cancellation and only unfinished children retry. Source links to canonical
regular files retain the selected destination name, including targets outside the
selected tree. Directory/broken/loop/special links are skipped. Whole-tree
no-replace publication applies to the applicable new-tree workflows, including
legacy Scheduler folder downloads, and does not replace browser child merges.

New browser downloads and their resumed targets apply source POSIX access masked
for the destination, or actual Windows parent inheritance. Reviewed replacements
preserve the existing nine POSIX access bits or Windows access ACL and apply the
source server timestamp. Ordinary Scheduler downloads use normal local
creation/inheritance and local write timestamps. Their existing revised approval
scopes must remain intact. Privileged transfers and other approved scopes must not
silently inherit changed authority.

Same-server copy uses server `copy-data` and checked no-replace publication with
no client-payload fallback. File-transfer denial still rejects client transfer
and export, while in-app file/image viewing follows its independent policy.
Server-only archive creation and extraction use the verified remote helper.
The client-streamed archive mode requires file-transfer permission.

Existing target replacement has no atomic compare-and-publish primitive. Preserve
identity checks and explain each publication mode, including partial visibility
for verified foreign-owner in-place writes. A reviewed race protocol and tests
are a dependency for expanding scheduled replacements.

Windows one-use Scheduler downloads can include changed regular files. The
replacement path holds the reviewed local destination against write and delete
sharing, renames that held object into a private recovery folder, and publishes
only to a vacant name. It binds both content digests, local identity and access
metadata, exact paths and the saved SSH route. The Unix and remote path-based
rename cases still need a race protocol. SFTP v3 rename provides no conditional
comparison against the reviewed object.

T3 native acceptance also covers destination permissions/default ACLs, mixed
replacement/removal and quarantine recovery in both directions, the deployed
extraction helper on direct/isolated routes and unsupported filesystems, named
ownership, metadata/listing/account labels and allocated-size fallback.

Relevant evidence includes
[ordinary folder queues](PORTING-PARITY.md#ordinary-sftp-folder-queue-and-selective-retry),
[Scheduler download creation](PORTING-PARITY.md#scheduler-ordinary-download-creation-and-approval-renewal),
[folder inheritance](PORTING-PARITY.md#legacy-scheduler-folder-download-inheritance-and-cancellation),
[regular-file links](PORTING-PARITY.md#regular-file-source-links-in-the-sftp-manager),
[foreign-owner replacement](PORTING-PARITY.md#reviewed-foreign-owner-sftp-replacement)
and [replacement-upload resume](PORTING-PARITY.md#resuming-reviewed-server-replacement-uploads).

## Scheduler, policy and recording

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| Q1 | Implementation + acceptance. Missing-only SFTP sync, one-use/standing/cascade/predecessor reviews, multi-target Agent/Swarm, per-round approval, comparison and saved follow-up exist. Read-only and iterative scheduled Swarms admit up to sixteen reviewed target workflows and queue the remainder of up to 32 routes. Windows has a one-use reviewed changed-file download path. Unix rsync supports reviewed regular-file upload and download and checked directory upload and download to new destinations. | Verify native larger Swarm runs over pinned SSH with configured providers, cancellation, journal/report/reopen and saved group refresh and reapproval after membership changes. Extend scheduled changed-file replacement after T3. Extend rsync to existing directory destinations, multiple sources, optional delete, collision-free multi-target trees, reviewed sudo and native Windows. Verify native recurrence/cascade/expiry and native rsync on both Unix targets. |
| Q2 | Implementation + acceptance. Elevated command/script/snippet, reviewed new-file/new-tree transfers, desktop outcomes and independently approved HTTPS webhooks exist. Typed Linux sudo actions cover mkdir/chmod/chown, regular-file and recursive folder copy to vacant paths, no-replace file/folder rename and protected deletion. Recursive operations support up to 128 total entries. The browser and Scheduler support server-only ZIP, TAR, TAR.BZ2 and 7z creation. A separately installed Linux helper supports those four codecs through a passwordless, path-constrained Scheduler sudo route. | Add privileged existing-target upload/download/sync after T3. Evaluate privileged archive passwords and optional reviewed local download. Add reviewed output ownership and mode controls for server-only archives. Verify the installed archive helper under transfer denial, real authorized sudo, Windows/macOS publication, configured webhooks and live desktop delivery. J `jobscheduler/JobSchedulerJobRunner.java`, R `src/job_executor/remote/privileged.rs`. |
| Q3 | Acceptance. Per-target Session Journals, capture revocation independent of execution, closing summaries, retention, estimates/totals, result links and Java retention-preference XML exist. | Native viewer/details/estimate/options and import/export dialogs, actual run/provider outcomes and target lifecycle. Verify a successful scheduled command under installed capture denial while existing retention continues. Java JAXB 0/37/3650 round trips already exist. |
| P1 | Implementation + acceptance. Managed Control activation, SFTP separation, update feed, encrypted recovery output, scoped AI restrictions, protected/provisioned models, custom headers, automation capture and managed SFTP conflict defaults exist. Startup supports managed and personal password requirements. The shared `allow-telemetry` Boolean is accepted for compatibility. Scoped `rule.ai-memory.statistics` controls local collection. | Decide the scope of remembered vault unlock. Verify startup vault setup and unlock, the stricter host-pin lock contract, native worker enforcement, scoped precedence and revocation, SMB sources, signed protected inference and macOS protected-cache restart. See the [field audit](target/verification/20261112-step5-policy-audit.md) for the earlier snapshot. |
| R1 | Acceptance. Native/tiled playback, visible IBM recording, Java JSONL/GZIP import/export, legacy plain JSONL import, and reviewed WebM/VP9 and MKV/FFV1 exports exist. Java reader compatibility and both Fedora codecs have evidence. | Native wide/resize/synchronized playback and approved encoder execution/cancellation/cleanup on Windows/macOS. Java recording interchange and video export are no longer implementation gaps. |

### Concrete policy work and residual schema audit

1. Verify Control startup and its locked setting under explicit allow and
   deny, then reopen after relaxing the rule. Exercise MCP opt-in and write
   consent with the retained personal preferences and normalized rule values.
2. Exercise browsing, metadata search, ownership, hosted and restored SFTP,
   server copy and extraction under an installed `file-transfer = deny` rule.
   Check blocked client transfers and external edits, and exercise the
   independent in-app editor and image permissions. Server copy requires
   SFTP copy-data and hard-link support. Verify server-only archive creation
   and extraction with the installed helper and client transfer denial.
3. Verify `[rule.updates].feed-url` against a managed HTTP(S) metadata endpoint
   with a native release and digest. Check the reviewed HTTPS archive origin,
   offer invalidation after policy changes, and the distinction between the
   application and AI runtime channels.
4. Verify `[rule.terminal].session-restore-output` with an unlocked vault and
   a native restart. Cover pane identity, mixed private and ordinary tabs,
   vault lock, policy locks, and purging when capture is disabled.
5. Follow up the exact migration differences and deployment evidence in the
   Step 5 managed policy field audit. Keep the Java startup password and
   local skill statistics workflows distinct from telemetry.

Policy entry points include R `src/clipboard_policy.rs`,
`src/workspace/dashboard.rs`, `src/workspace/project_access/local_files.rs`
and `src/sftp_window.rs`. Preserve user-over-group-over-global precedence
per field and same-tier deny. Managed locks must retain the personal
preference for later relaxation.

Scheduled Agent/Swarm authority remains target-, run-, round- and exact-request
bound. Fixed probes and masked results belong to their original target.
Continuation or comparison needs its own reviewed request. Imported
auto-approval flags never authorize native commands.

Evidence includes
[scheduled Swarm continuation](PORTING-PARITY.md#continuing-saved-scheduled-swarm-conversations),
[automation capture and SFTP policy](PORTING-PARITY.md#scoped-automation-capture-and-sftp-conflict-settings),
[provisioned profiles](PORTING-PARITY.md#provisioned-ai-profiles) and
[protected model launch](PORTING-PARITY.md#kernel-protected-policy-model-launch).

## Distribution, help and residual product audits

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| U1 | Acceptance with external release prerequisite. Manual and opt-in application checks, managed metadata feeds, native archive selection, size/SHA-256/native executable validation and reviewed download exist. R `src/application_update.rs`. | Verify the managed feed under P1 and an anonymously accessible owner-published Rust stable release with compatible assets and digests on the default channel. The last recorded anonymous check on 2026-11-12 returned 404 for the configured repository and latest-release metadata. Update current local packages under V7 and validate installation, signing and runtime per target. |
| U2 | Acceptance. Bundled rich manual chapters/anchors/figures, validated links, persisted zoom, Settings Help and reviewed Ask the manual with exact numbered source navigation exist. | Native clicks, keyboard use, zoom, layout and reviewed answers across desktops. Do not list the manual or its Q&A as unimplemented. |
| U3 | Implementation + acceptance. Native SSH connection monitoring, worker process views and all eleven supplied effects/previews exist. Fractional effect speeds survive Java connection exchange and uniform-tab project exchange. GitHub, SkillsMP and direct Markdown provider profiles support discovery, reviewed disabled import and revision updates with local edit protection. Local skill usage is recorded, displayed, and included in encrypted profile backup. JVM heap/GC and JAR ServiceLoader mechanisms are Java-specific. | Verify native provider and Skills UI acceptance, cross-host AI backup and restore, hosted/detached monitor and effect lifecycle. Java projects represent one effect per tab. Make a product decision before implementing telemetry or third-party native effect plugins. |

U1 evidence is recorded in
[shared MLX policy and release checks](PORTING-PARITY.md#shared-mlx-update-policy-managed-download-restrictions-and-python-parameters).
The repository CI matrix includes macOS, Ubuntu and Windows format/Clippy/tests
and release staging. It does not establish a successful run for the current
uncommitted snapshot or publish current package artifacts.

## Native acceptance and release gates

Existing results close only their recorded build/platform/conditions.
Cross-compilation, loopback fixtures and emulation must be labelled as such.
External prerequisites do not turn implemented workflows into absent features.
The [Step 6 checkpoint](target/verification/20261112-step6-release.md) links
the current platform, package and audit evidence.

| Gate | Remaining evidence |
| --- | --- |
| V1 | Windows, macOS, native Wayland and native X11 nested-split transfer under output by menu and drag, cancellation, same processes/scrollback/focus, independent blocked broadcast, restore and unsaved-close cancellation. Record Xwayland separately. |
| V2 | Fresh profile/restart, small screen, DPI changes, large→small→large monitors, owner modality/focus, modeless re-entry, hosted/detached tools, menu/toolbar keyboard use, fullscreen/transparency and macOS Dock reopen. Preserve compositor control of Wayland placement. |
| V3 | Windows ARM64 already has measured activation of the exact inactive Gemini split and original window. Complete native macOS permission/display/activation, Linux compositor-qualified delivery, and moved/closed/modal/revoked/stale targets on each backend. General and programmatic notices need their own cases. |
| V4 | All 21 emulations exist. Complete GUI/key/reply/charset/mode/resize/reflow and supported Scheduler comparisons per profile. Restore the pinned SithTermFX checkout before fixture regeneration. Verify audible Windows/Linux bell and retain documented Wyse differences. Real TN3270/TN5250, HP application/device and physical serial break/modem/flow-control remain explicitly user-deferred. |
| V5 | Enabled real providers and signed runtimes, every AI Manager section hosted/detached, editor source jumps after edits, seven bounded diagram families, clicks after zoom, full SVG/PNG/PDF beyond the viewport, retained analyses after restart and consent/cancellation at mutation boundaries. Include current A/B/S/J changes. |
| V6 | The separate SSH source review led to two transport fixes with native wire regressions. Apple Silicon passed the packaged App Sandbox, changed pin, tampered helper, approved tunnel, cancellation and jump fixtures. Resolve active `crypto-bigint` and OpenPGP RSA advisories, obtain external independent review, and complete native Windows x64 and macOS Intel packaged isolation. Keep the SFTP external replacement race as a separate hardening risk. See the [SSH audit](target/verification/20261112-step6-ssh-audit.md). |
| V7 | Apple Silicon passed the native workspace suite on the repaired snapshot with 3,446 passed and zero failed. Its Developer ID package passed strict signature verification, native desktop start and exit, Seatbelt and packaged SSH isolation. Apple accepted notarization and Gatekeeper accepted the app with its stapled ticket after extraction from the final ZIP. Complete fixed-snapshot native suites and clean-system package evidence on the remaining targets and same-host startup, idle memory and p95 input comparisons including 5K macOS. Keep staged packages as candidates while security and release gates are open. See the [native Mac suite](target/verification/20261112-macos-suite.md), [Developer ID package verification](target/verification/20261112-macos-package.md) and [notarization verification](target/verification/20261112-macos-notary.md). |

Forwarding currently permits thirteen concurrent streams and four remote listener
registrations per SSH connection. Establish a reference workload before raising
these bounds. The broad HP hardware statement likewise needs an opcode/device
comparison before a new implementation ticket.

Native Wayland desktop drops use a surface-scoped `text/uri-list` Copy offer
through the existing clipboard worker. The app resolves the compositor drop
coordinates in the current SFTP window, including scrolled folder rows. The
offer is finished after the local copy or server upload queue accepts the
validated selection. Fedora ARM64 verified the native GTK-to-Iced handshake
and app-side destination and generation tests. Verify the complete drop
workflow in hosted and detached SFTP with a desktop file manager and a real
SSH connection. See the [Wayland checkpoint](PORTING-PARITY.md#incoming-wayland-file-drops-in-sftp).

The V6 follow-up traces the remaining private operations to Sunset's SSH
NIST ECDSA adapters and rPGP's RSA decryption and ECDH/X448 operations.
Stable rPGP still uses the affected dependency generations. The upstream
[RustCrypto migration](https://github.com/rpgp/rpgp/pull/573) is open.
Evaluate the NIST curve migration separately against the existing
cryptography constraints and all private-key and route fixtures. Review
OpenPGP decryption blinding and legacy backup restore before adopting a
new provider.

## Maintenance and completion

For each work package, trace the pinned action, defaults, persistence and policy
into the native UI and execution route. Implement the connected flow, add focused
regression/failure checks and run the applicable native acceptance. Record the
source snapshot, binary/package hashes, platform/architecture/window system,
steps, result and remaining limitation in PORTING-PARITY.

Preserve the existing Connection Manager map, Teamwork, saved local profiles,
designs, signed runtime lifecycle, automatic knowledge refresh, journal summaries,
snippet recovery/PDF, serial library, durable SFTP recovery, scheduler journals,
recording exchange and manual/updater implementations. Their specific remaining
cases are listed above.

Follow [AGENTS.md](AGENTS.md) for shared Cargo targets, private test profiles,
scratch/process cleanup and keeping the latest verified packages/checksums.
Record verification cleanup and package promotion in the current checkpoint;
pending build or package work must not be reported as completed cleanup.

The port is complete only after in-scope implementation gaps are closed,
remaining audits have an explicit disposition and the native/interoperability/
security/performance gates have evidence. Deferred hardware and unavailable
release/provider prerequisites must remain visible until accepted or explicitly
removed from scope.
