# Remaining Java-to-Rust porting plan

Porting plan reconciled on 2026-11-24. This plan tracks the scope agreed in
[HANDOFF.md](HANDOFF.md). The [parity record](PORTING-PARITY.md) identifies the
source and environment for earlier checks. Paths under `target/verification/`
are local to the checkout that ran the tests. Historical logs referenced
by this plan are available in the original main checkout when absent from
the porting worktree.

## Current result and actionable backlog

**Java v2.26.0 parity remains partial.** [PR #35](https://github.com/chardonnay/korTTY_rust/pull/35)
includes device-local vault unlock (`fe110de1`), saved temporary SSH keys
(`cff7d8e7`), reviewed Java key import (`08567961`) and automatic project
reconnect with eligible keys (`b046b261`). It builds on the PR #34 merge
at `10e4ee78`. The Java reference is tag `v2.26.0` at `b235d91f2035`.
Windows ARM64 desktop checks cover the Settings frame and Font and Window
pages, the Connection Manager and selected SSH editor tabs, and specific
AI Manager and Agent Design workflows. The standalone Agent Design editor
already saves changes to a built-in as a custom copy. GGUF and MLX have
owned Configure dialogs. The recorded desktop save and cancel check covered
GGUF. [November 23 review and test scope](PORTING-PARITY.md#november-23-dialog-parity-review)

The follow-up implements the Plan and execution layout, retains the
publication date when registering a Hub MLX model, and adds owned prompts
for naming Agent designs and reviewing GGUF imports and backend changes.
The same checkpoint restores Knowledge Store source outcomes after restart,
copies the complete retained and masked output of the selected Agent step,
and applies the Java matching and ranking rules to Settings and AI Manager.
The GGUF Configure window now sizes to the model path and keeps the action
buttons visible when the form scrolls. The Windows ARM64 comparison also
covered the compact field layout and both native import storage choices.
Knowledge Stores opens with its model inventory loading intact, and a
Windows ARM64 restart restored both manual and automatic source outcomes.
Global Settings groups password rotation, the password requirement and
optional device-local vault auto-unlock under Security. A password
requirement takes precedence, and unavailable or stale device credentials
return to the typed password prompt. The temporary-key switch enables
both Quick Connect and encrypted temporary credentials in the saved SSH
editor. Java backup review can seal supported temporary keys for exact new
saved profiles. The imported lifetime starts at credential review.
The Apple Silicon tests authenticated SSH and SFTP with an imported key.
They also exercised automatic project reconnect after vault unlock and
deferral after expiry. Scheduler jobs now retain a reviewed binding to
eligible temporary SSH keys for the target and any jump host.
[Import verification](PORTING-PARITY.md#november-23-java-temporary-ssh-key-restore)
· [Project reconnect verification](PORTING-PARITY.md#november-23-project-reconnect-with-temporary-ssh-keys)

Security also provides the global session isolation default and shows the
native local-shell and SSH sandbox prerequisites. New local shells, ad hoc
SSH and SFTP, and Teamwork SSH use the default subject to their applicable
managed limits.

The Connection Manager now stores session isolation on folders and offers
inheritance as a separate SSH profile choice. The editor shows the effective
level and its source. Interactive saved SSH launches and Teamwork use the
folder rules. Saved local-shell profiles can inherit the nearest folder or
choose a level directly. Older Rust local profiles keep their global
default. Java-format profile XML preserves explicit None and inheritance.
Java backup restore applies unambiguous isolation levels to exact new
folders. It stages other folder levels, the global default and the
temporary-key option for explicit review in the native editors.
[Connection isolation checkpoint](PORTING-PARITY.md#november-23-connection-and-folder-isolation)
· [Java backup security checkpoint](PORTING-PARITY.md#november-23-java-backup-security-settings)
The Windows ARM64 comparison verified effective isolation in the profile
inspectors and the managed Security display. Native application recovery
reopened a saved local profile in the sandbox under both folder inheritance
and an installed minimum. [Windows isolation checks](PORTING-PARITY.md#november-23-windows-isolation-dialogs-and-launches)
On Apple Silicon, the native application fixture reconnected saved SSH
and Teamwork terminals through a pinned direct OpenSSH route. Saved SSH
picked up a stricter folder rule, and Teamwork retained its reviewed route
and the existing isolation floor. Both replaced the original pane in
place. Windows ARM64 passed 36 focused regressions. The desktop reconnect
flow and the jump-host wire path are still open.
[SSH reconnect checks](PORTING-PARITY.md#november-23-saved-and-teamwork-ssh-reconnect)

Start with the [confirmed implementation work](#confirmed-implementation-work).
Other open areas include the remaining Settings pages and connection editor
tabs, [Java backup field and identity mappings](#remaining-field-and-identity-audit),
and the [Teamwork Agent/Swarm Scheduler extension](#implemented-workflows-awaiting-acceptance-and-requested-extensions).
The [implementation order](#implementation-order)
also covers provider, transfer and editor workflows. The [native and release
gates](#native-acceptance-and-release-gates) track platform acceptance, the
independent SSH review and release-candidate verification.
The [November 19 audit](PORTING-PARITY.md#november-19-java-release-source-comparison)
records the broader comparison with the tagged Java release.

### Confirmed implementation work

| Area | Remaining change | Completion check |
| --- | --- | --- |
| D4 · Searchable controls | Compare the remaining Java Settings and AI Manager pages, subpages and labeled controls with the Rust search inventory. The [Knowledge Stores toolbar](PORTING-PARITY.md#november-24-knowledge-stores-toolbar-search), [Local Models runtime and GGUF import controls](PORTING-PARITY.md#november-24-local-models-toolbar-search), and [Appearance controls](PORTING-PARITY.md#november-24-appearance-inventory) have independent search targets. | Verify that controls on a page that has never been opened are discoverable, and that navigation reveals the exact control with the current page state and policy. |
| D4 · Settings section navigation | The [Settings section menu](PORTING-PARITY.md#november-24-settings-section-overflow) appears when the tab labels exceed the available width at the current application font scale. It lists every Rust section in tab order and marks the active section. [Keyboard navigation](PORTING-PARITY.md#november-24-settings-section-keyboard-navigation) reaches the trigger, reveals highlighted options and closes the menu before Settings handles Escape. | Compare the styling with Java and exercise the menu with keyboard and pointer on the remaining native desktops. |
| Appearance preferences | [Chat color profile](PORTING-PARITY.md#november-24-appearance-inventory) is available in Settings and the AI Chat toolbar. [UI font size](PORTING-PARITY.md#november-24-application-font-scale) and Match display resolution are searchable in Settings. Java backup import preserves the application and guide scales independently. | Verify the theme-following and fixed profiles in running AI Chat and Swarm views, including when a chat is already open. Compare manual and automatic UI font scale at multiple native display scales with Java, including popups and the terminal, editor and chat font boundaries. |

The [November 23 review](PORTING-PARITY.md#november-23-dialog-parity-review)
records the dialog differences. The [November 22 comparison](PORTING-PARITY.md#november-22-design-editing-command-output-and-settings-search)
traces step copying and both search matchers.

### GUI dialog design parity

**Open, highest UI priority. Every Rust GUI dialog with a Java counterpart
must match the design of that dialog in Java v2.26.0.** The current Rust
dialogs have been reported as difficult to scan. Use the tagged Java dialog
as the reference for these corrections.

- **Structure.** Match the navigation, tab and section order, field groups,
  and the sequence of labels and controls. Keep related settings together
  and put advanced options and contextual help where the Java dialog puts them.
- **Visual hierarchy.** Match the typography of headings and labels, theme
  colors, icon placement, field alignment and widths, and spacing within
  and between groups. Match the placement and order of primary, secondary
  and cancel actions.
- **Resizing.** Compare the initial window size, which regions grow or scroll,
  and where the action buttons sit. At small window sizes and higher display
  scaling, labels and fields must remain readable and all actions reachable.
- **Verification.** Inventory the dialogs from their normal menu entries.
  Start with Settings, Connection Manager and its connection editor, AI
  Manager and the Agent Design chooser. Include Scheduler, SFTP and the
  remaining dialogs. For each, compare Java and Rust screenshots side by
  side on the same platform, with the same state, language, theme, window
  size and display scale. Record those conditions, correct visible
  differences, and verify the revised
  layout on the native desktops covered by V2. For a Rust-only dialog,
  follow the grouping and action placement of the closest Java workflow.

The [tagged Java dialog sources](https://github.com/chardonnay/korTTY/tree/v2.26.0/src/main/java/de/kortty/ui)
and the [native acceptance matrix](#native-acceptance-and-release-gates)
define the reference and target platforms for this work.

The shared Settings frame and Font page now follow the Normal Java design.
The Window page groups tool-window hosting with window preferences. A
Windows ARM64 comparison covered both pages, search result focus, the
font-size preview and cancellation. A direct-start recheck confirmed the
Font field order. Rust has 18 tabs and shows an overflow menu when the
full strip exceeds the available width. Java has 21 sections. The first
Security comparison covered the master-password group and both search
results. Control styling and deeper Settings pages still need visual
work. [Desktop comparison](PORTING-PARITY.md#november-22-java-dialog-design-on-windows-arm64)

Resources exposes a JVM launch profile and its heap limit. Rust already
detects physical memory for local-model recommendations and provides a
terminal scrollback memory limit. Compare those native controls before
specifying a Resources page. [Host memory](src/host_memory.rs)
· [Settings controls](src/workspace/settings_window.rs).

The Security page offers the imported global isolation and Quick Connect
temporary-key choices for explicit adoption. Windows ARM64 verified the
managed display, below-minimum rejection and folder review presentation.
Exercise the Save and restart flow for reviewed choices and repeat it on
the other native desktops. Continue the [saved temporary-key desktop checks](PORTING-PARITY.md#november-23-native-saved-temporary-ssh-keys).
Java Privacy manages analytics consent and the date and version of that
decision. Rust validates the shared `allow-telemetry` policy field, and
Java backup restore drops telemetry consent. [Policy validation](src/clipboard_policy.rs)
· [Backup restore regression](src/workspace/backup_data/maintenance.rs).
Java Language selects the application locale. Java Translation manages
profiles, runs and generated UI and guide locales. Application localization
is deferred under the [port constraints](#scope-and-recorded-decisions). Assess
the generated guide workflow on its own. The implemented snippet and
journal translation actions operate on user content.
[Tagged Java Settings](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/SettingsDialog.java)

The Connection Manager adopts the Java toolbar order, places Undo in the
Folder group and groups Connect and Close at the right. The SSH editor
organizes its existing form into Connection, Terminal Settings, SSH Tunnels,
Jump Server, Terminal Logging, Journal, Window Geometry and AI. The
[Journal and AI tabs](PORTING-PARITY.md#november-24-ssh-editor-journal-and-ai-tabs)
use the existing saved connection options. The
[connection memory shortcut](PORTING-PARITY.md#november-24-memory-from-the-ssh-editor)
opens the exact saved SSH profile's memory in a separate owned window.
The [managed Journal requirement](PORTING-PARITY.md#november-24-managed-session-journal)
locks the SSH editor's autostart control and starts capture on eligible
terminal streams. A native Apple Silicon OpenSSH test covered automatic
start and masked capture from a private saved profile. The
[SSH editor tab strip](PORTING-PARITY.md#november-24-ssh-editor-tab-navigation)
uses one row with an overflow menu and a separate position counter and
arrow controls. Ctrl+Tab, Ctrl+Shift+Tab and Ctrl+Page Up or Down step
through the tabs from the editor. Compare the compact and wide variants
on the native desktops at the same font scale as the Java reference.
The [Journal rotation update](PORTING-PARITY.md#november-24-java-journal-rotation-limits)
matches Java's 1,000 MiB part size and default editor ceiling of 500 parts.
The native reader and managed policy support up to 1,000 parts. Java's
editor can use managed ceilings above that bound. The reader, search,
[replacement](PORTING-PARITY.md#november-24-streaming-journal-replacement),
and [HTML bundle](PORTING-PARITY.md#november-24-streaming-html-journal-bundles)
use the larger part budgets. OpenPGP HTML bundles stream the ZIP directly
into encryption. An [active SSH journal](PORTING-PARITY.md#november-24-ssh-journal-reconnect)
survives disconnects and returns to the same journal on a committed
reconnect. Apple Silicon verified this over pinned OpenSSH with managed
masking in an incognito tab. The [isolation entry](PORTING-PARITY.md#november-24-journal-isolation-status)
records the effective startup or replacement stream isolation. Verify the
rotation settings in native desktop editors, including managed values and
rotation across a reconnect.
The Manager's separate
Teamwork sidebar group and editor form alignment remain open. The earlier
Windows desktop check covered the three-pane Manager, its Map view, opening
the editor and navigating between editor tabs.

The AI Manager now uses the Java window size, a search overlay, underlined
primary tabs and General subpages. The profile form retains its four
sections without a selection, and the empty and editable forms share
field alignment. Budget & usage contains the tokenizer, quota, warning
thresholds, reset and prices. Local AI separates
profile roles, runtime settings and Hugging Face access. Local Models
starts with runtime and GGUF inventories. Knowledge Stores stacks
store and source inventories and places file and folder selection below
the source table. The shared picker can review multiple files together.
Folder selection opens the existing scan review. Create and Configure
open a separate owned store settings window with model and role fields
and collapsible advanced storage settings. Create continues to a compact
initial-source dialog. Approving the file or folder review stages the new
store and its sources together. AI Manager Save applies the draft and
indexing requires separate consent. Knowledge source actions use native
file and folder pickers. Individual files are scanned locally before an
owned preview shows the usable files and the reasons for skipped entries.
Approving the preview checks the reviewed contents again before staging
the usable sources. Oversized files can be rescanned with a reviewed
per-file limit before they are added. The Advanced form exposes local
embedding dimensions from 1 to 65,536. New stores suggest the selected
registered GGUF model's declared size, and a blank field retains automatic
detection. The local index checks the selected size before publishing vectors
and retains compatibility with existing snapshots. Verify the native pickers
and previews on macOS and Linux. The Windows comparison confirmed the
single-line dimension label, arrow controls, and inactive Qdrant fields
in Java's order. Both applications step the same GGUF dimension up and
down. The Rust spinner enforces the limit of the selected backend. The
advanced spacing and active Qdrant credential rows still differ. Rust
now supports range and toggle selection in the source inventory. Update
reads the selected enabled sources, or all enabled sources with an empty
selection, and preserves existing compatible vectors for other sources.
Removal opens a separate confirmation owned by AI Manager and lists the
exact selected paths before changing the store draft. Save applies the
draft change. Both inventories grow with the AI Manager window. At
standard height the source inventory has room for four rows. The shared
source action row wraps at smaller widths, and the page scrolls to keep
the tables and controls reachable at the minimum size. The source actions
now stay in the Java order, and secondary index actions sit below them.
Refresh reloads the selected store's index inventory. Sync mode opens
an owned choice dialog. Changing a folder's mode requires reviewing its
file list, and AI Manager Save applies the staged changes. Both tables
use neutral selected rows in the Normal theme. Advanced opens an owned
file or folder editor. Folder include and exclude globs each have a
multiline field. Review checks the file list before updating the draft.
The owned editors use an editable MiB value and adjacent increment and
decrement buttons. A B suffix accepts or displays exact byte limits
already present in saved profiles. Align the remaining table column
widths. Java also shows filters in its individual-file form. Its
scanner and watcher apply those filters during folder traversal.
Update scans the selected enabled files and folders together, or all enabled sources
when no source is selected. The owned preview shows path, format, size,
status, and reason. Approval stages the folder manifests and holds the
reviewed individual-file digests for the approved selection. Index
reviewed sources remains a separate action with embedding consent.
It verifies the reviewed contents before sending them to the provider.
The source table shows Files / chunks, Skipped / failed and Last indexed.
File and folder totals reflect the matching index snapshot. New snapshots
carry per-file timestamps, and partial refreshes retain the times of
unchanged sources. Earlier snapshots retain their counts and show no
timestamp. Folder review problems persist with the approved manifest.
Manual updates show scanning, indexing and cancellation in the requested
source rows. Cancel sits beside Update now and keeps the operation pending
until its worker returns. An unreadable folder appears as a skipped row in
the mixed review. Ready sources can still be approved, and the unavailable
folder retains its saved manifest and vectors. An indexing failure tied to
a particular file marks its source row and keeps the previous committed index.
Automatic sync now reports scanning, indexing, unavailable sources,
required review and source-specific indexing failures in the same inventory.
The latest completed result and problem count survive reopening the manager
after a restart. A failed attempt leaves the last published index and its
timestamp available to the source inventory. [Source outcome checkpoint](PORTING-PARITY.md#november-23-knowledge-source-outcomes)
At the source limit, a mixed folder review now counts all successful
decreases before admitting growth in inventory order. Skipped folders
continue to count with their saved manifests. Folder preview rows
distinguish unchanged files from files to be indexed, in the Status column.
Align the remaining control styling and source actions.
[Windows ARM64 source table comparison](target/verification/20261123-dialog-tables-windows-arm64-desktop.md)
[Windows ARM64 mixed source comparison](target/verification/20261123-knowledge-source-updates-windows-arm64-desktop.md)
[Windows ARM64 folder update comparison](target/verification/20261123-knowledge-folders-windows-arm64-desktop.md)
[Windows ARM64 inventory layout comparison](target/verification/20261123-knowledge-table-layout-windows-arm64-desktop.md)
[Windows ARM64 source selection comparison](target/verification/20261123-knowledge-selection-windows-arm64-desktop.md)
[Windows ARM64 automatic source status comparison](target/verification/20261123-automatic-sources-windows-arm64-desktop.md)
[Windows ARM64 source control comparison](target/verification/20261123-inventory-actions-windows-arm64-desktop.md)
[Windows ARM64 advanced source comparison](target/verification/20261123-advanced-dialog-windows-arm64-desktop.md)
[Windows ARM64 source size comparison](target/verification/20261123-source-stepper-windows-arm64-desktop.md)
[Desktop verification](target/verification/20261123-knowledge-spinner-windows-arm64-desktop.md)
Skills places preferences and the Local/External switch next to the
editor. Memory groups entry and Markdown actions beneath the selected
view. Local Models places the runtime inventory above the installed
models and groups the actions immediately below each table. Its model
columns follow the Java order. New Hugging Face GGUF downloads retain
the repository publication timestamp, and existing registrations
without publication metadata show an em dash. The signed-package import
discloses its verification fields. Import GGUF opens a native multiple-file
picker and then an owned review with the selected paths, model purpose,
and private-copy or external-reference options. An eligible detached
registration can be reattached through its existing review. A manual path
entry remains available.
The installed model table combines GGUF and MLX registrations on Apple
Silicon. Other hosts display GGUF. The headers sort by the selected column
in either direction. Ctrl or Command toggles rows, Shift extends the range
in the displayed order, and the focused table accepts the arrow keys and
Select All. Refresh reads both supported registries. Configure follows the
current row. Start, Stop, and Remove act on the selected models across
both runtimes. Start can warm two models concurrently. Its estimate
includes selected and resident GGUF and MLX weights and opens a separate
confirmation at the 80 percent threshold. Remove opens a separate
confirmation listing the reviewed registrations and validates each
registration again under its registry lock before unpublishing it.
Install runtime and Other versions open separate dialogs. Starting selected
GGUF models without a compatible runtime offers the signed installation
first. Cancel preserves the selection. After the reviewed online package
activates, the manager checks the selected registrations again before
starting them. The version chooser filters the signed channel for the
current host and backend, and lets users return a pinned install to
stable updates. The toolbar and row context menu set or clear a single
default for GGUF or MLX. The manager restores its star and preselects it
when there is no live selection. Existing GGUF preferences keep their
format. MLX preferences record the model family explicitly.
New Hub MLX downloads retain the upstream publication date. The installed
table reads the saved value and uses the shared Age sort. Older registrations
without a date still show an em dash. Apple Silicon passed the registered
date round trip and mixed-table sort fixtures. Verify the displayed age and
sorting after a desktop restart, the MLX default and live model starts.
[Apple Silicon checks](PORTING-PARITY.md#november-23-mlx-publication-date-on-apple-silicon)
Native Windows ARM64 installation,
version selection with rows, and the subsequent model start need a
compatible signed runtime.
Configure opens an owned window for GGUF and MLX. Both show the model
location and use numeric steppers for the editable unload time. GGUF also
shows backend, purpose, context size, CPU threads, and GPU layers in the
Java order. Escape dismisses the draft. Save checks the current
registration before writing it. When a saved GGUF model requires a
different backend from the active signed runtime, Configure offers
to keep that runtime or open the existing signed-runtime review.
Enter path opens the manual import fields in the manager. GGUF Configure
uses the compact Java label column, adjacent spinner arrows and a fixed
Save and Cancel row. The path is one line when the available width permits
it and wraps within the scrollable form at smaller sizes. Windows ARM64
compared the same long path against the Java desktop, saved from the narrow
form, and verified private copy, external reference and cancellation from
the native import review. Verify split-model reattachment and both responses
to the matching-runtime offer in the desktop.
[Windows ARM64 model inventory comparison](target/verification/20261123-model-inventory-windows-arm64-desktop.md)
[Windows ARM64 native model picker and sorting](target/verification/20261123-model-picker-windows-arm64-desktop.md)
[Windows ARM64 model selection and group actions](target/verification/20261123-model-selection-windows-arm64-desktop.md)
[Windows ARM64 runtime dialogs](target/verification/20261123-runtime-review-windows-arm64-desktop.md)
[Windows ARM64 installed model table](target/verification/20261123-mixed-model-windows-arm64-desktop.md)
[Windows ARM64 model group actions](target/verification/20261123-model-batch-windows-arm64-desktop.md)
[Windows ARM64 local model default](target/verification/20261123-model-default-windows-arm64-desktop.md)
[Windows ARM64 model configuration](target/verification/20261123-model-config-windows-arm64-desktop.md#ai-manager)
Agent Design now opens
as a standalone dialog with a module table and adjacent sample. Changing
a built-in creates a custom copy named from the built-in's full name.
New, Duplicate and Rename open an owned dialog with the proposed name
selected. The standalone dialog saves its own
preferences as they change, including auto-hide and reduced animations.
Replay and changes to Reduce animations restart the selected module
preview. The disclosure next to Replay opens the sample controls and
dock preferences. The General Settings entry
retains its draft behavior. The Windows comparison confirmed the auto-hide
stepper, the Standard choice caption, bold table headings, and measured
ellipses with full description tooltips in the standard and compact views.
The shared Metro presenter now places the title and prompt on the left,
the station track in the center, and run metadata and actions on the right
in a 52-pixel logical bar. Windows confirmed the standard and compact
previews, the compact sample approval strip, and the steps menu. The
module selection highlight spans the full table row, including the Active
column. The table participates in Tab focus. Up, Down, Home and End preview
and reveal the selected row. Space activates a module using the existing
custom-copy behavior. Compare the remaining sample content, approval
actions, keyboard details and additional preview and dock actions. Side
timeline uses Java's default 240-pixel panel width. The shared presenter
shows the run title, usage, elapsed time, prompt, and actions. Metro
spans the terminal and side dock in the shared composition. Windows
confirmed the row-driven timeline preview, independent hide, and
restoration through Replay. The timeline uses the Java role colors,
transparent header actions, and the Java preview prompt, answer and
reported token count. It also shows exit codes with available output line
counts and wraps long commands within the dock. Side timeline now uses
the reference panel insets and more compact text, a muted scrollbar,
and inline command review controls. The existing bound approval and
number-key checks still apply. Metro groups compact token usage and
elapsed time into one label, also used by Side timeline. Continue
comparing the remaining module details, preview controls and dock actions.
Plan and execution places Metro above its bottom panel and shows the
prompt, run controls and Plan checklist. The panel starts at 152 logical
pixels and its adjustable divider starts at 42%. Windows ARM64 verified the
standard and compact previews, divider movement and Metro across a side
timeline. Verify the bound review and command output in a live desktop
run. [Plan and execution checks](PORTING-PARITY.md#november-23-plan-and-execution-layout)
The sample prompt, disk-usage output and outline caret now render from
ANSI terminal cells. Blocks in the terminal show the action-step count and
compact token usage. The summary wraps at words within the terminal cell
width. Continue comparing the presentation of each module around the shared
sample.
[Windows ARM64 Side timeline comparison](target/verification/20261123-agent-timeline-windows-arm64-desktop.md)
[Windows ARM64 Metro comparison](target/verification/20261123-metro-layout-windows-arm64-desktop.md)
[Windows ARM64 module selection comparison](target/verification/20261123-inventory-actions-windows-arm64-desktop.md#agent-design)
[Windows ARM64 module keyboard verification](target/verification/20261123-advanced-dialog-windows-arm64-desktop.md#agent-design-keyboard)
[Windows ARM64 preview control comparison](target/verification/20261123-source-stepper-windows-arm64-desktop.md#agent-design)
[Windows ARM64 Plan and execution comparison](target/verification/20261123-model-config-windows-arm64-desktop.md#agent-design)
[AI dialog comparison](PORTING-PARITY.md#november-22-ai-manager-and-agent-design-on-windows-arm64)

For standalone Windows previews, build the development application with
`cargo rustc --locked -p kortty --bin kortty --features dialog-preview -- -C linker=rust-lld`.
Run `scripts/preview-windows-dialog.ps1` with `-Dialog settings`,
`connections`, `ai-manager`, `agent-design` or `agent-design-compact`.
The compact variant opens Agent Design at its 940 by 480 minimum client size.
Each run creates a private
Normal-theme profile and opens the requested dialog through the application's
usual startup and window routes. Use these entry points for Windows desktop
comparison without operator interaction in the terminal window. The preview
feature is opt-in and its startup command is available in debug builds.

### Reconciliation against the current working tree

| Implemented in the working tree | Evidence and remaining acceptance |
| --- | --- |
| Windows ARM64 portable ZIP, November 19 | Its exact source passed 4,526 native workspace tests with 96 ignored and strict workspace Clippy. The subsequent Agent Design changes have a separate native development build, focused tests and live desktop evidence. [ZIP and suite](PORTING-PARITY.md#november-19-windows-arm64-portable-release) · [Agent Design desktop checks](PORTING-PARITY.md#agent-design-module-preview-and-status-preferences) |
| Saved local shell profiles, local and SSH `agent` recognition, connection overrides and the three OS sandbox paths | Verify remaining shells, packaged isolation and desktop behavior on the outstanding native targets (D1/D2/A1/T5). |
| Journal HTTPS credential binding for questions, summary preflight, streaming and retries | F1 has focused native regressions on Windows ARM64, Apple Silicon and Fedora ARM64. Exercise configured providers and the desktop review under V5. |
| Calendar and Chain transfers to protected files and folders, including creation followed by a merge into the same confirmed folder | State and editor fixtures cover both directions and both sudo modes. Recurring protected folders still need an end-to-end run against the deployed helper. Earlier real sudo/rsync checks cover their recorded routes. [Folder verification](PORTING-PARITY.md#recurring-protected-folder-creation-and-confirmed-receiver-progression) |
| Reviewed recursive SFTP deletion, terminal text/file drop routing, remembered snippet translation language and selectable AI alternatives with diff and undo | Direct and jump OpenSSH deletion passed on Apple Silicon and Fedora. The GTK text gesture reached the inactive split on X11 at 100% and 200% and on Xwayland at 100%. Remaining desktop sources and live provider interaction are in V1–V5. [Desktop drop evidence](PORTING-PARITY.md#x11-text-drops-in-terminal-panes) · [Snippet evidence](PORTING-PARITY.md#selectable-snippet-alternatives) |
| Local knowledge snapshots up to five million passages and 64 GiB, recording interchange, diagram export and native package staging | Measure real corpora, check configured Qdrant recovery, and complete native playback, exports and release verification. Merge CI passed for Windows, macOS and Ubuntu. Later local changes have targeted records. [Merge CI](https://github.com/chardonnay/korTTY_rust/actions/runs/44106367721) |

### Implemented features with outstanding checks

| Area | Implemented in Rust | Remaining verification |
| --- | --- | --- |
| D4/P1 · Vault startup | An explicit profile opt-in stores the current derived vault key in the protected device credential store. Startup authenticates the vault, falls back to the password prompt on failure, and checks both managed and personal password requirements. Native credential-store fixtures on Windows ARM64 and Fedora ARM64 verified rotation, reopen and removal. Apple Silicon passed the vault and startup suites. | Exercise Security through an actual restart on each native desktop. Include interactive Keychain access with a signed macOS app, the installed Flatpak Secret portal and a managed startup policy set after opting in. [Startup unlock checkpoint](PORTING-PARITY.md#november-23-native-vault-startup-unlock) |
| D2/P1 · Session isolation and reconnect | Effective isolation appears in the folder and connection editors. Windows ARM64 verified saved local-shell recovery with inherited and managed Sandbox choices. The native Apple Silicon fixture verified saved SSH and Teamwork reconnect over pinned direct OpenSSH. | Exercise a fresh local launch, unavailable sandbox feedback, desktop reconnect and a live jump-host reconnect. Repeat the applicable desktop checks on macOS and Linux. [Windows desktop checks](PORTING-PARITY.md#november-23-windows-isolation-dialogs-and-launches) · [OpenSSH reconnect checks](PORTING-PARITY.md#november-23-saved-and-teamwork-ssh-reconnect) |
| D2/D4 · Quick Connect temporary keys | The saved Security opt-in enables in-memory temporary authentication for interactive SSH and SFTP, with destination validation, expiry and retry. | Use a disposable key for a live SSH and SFTP login, retry after expiry, and disable the option while a form or retry holds a key. [Quick Connect checkpoint](PORTING-PARITY.md#november-23-quick-connect-temporary-ssh-keys) |
| D2 · Saved temporary keys | Connection Manager stores the key, expiry, and exact account and destination in the encrypted vault. The login form uses an unexpired saved key with optional automatic selection or accepts a replacement for that attempt. A pinned Apple Silicon OpenSSH fixture verified locked-vault rejection and SSH and SFTP after unlocking. | Exercise the desktop editor, locked unrelated saves, reconnection on expiry and after replacement, and the isolated SSH worker. Check Quick Connect's in-memory paths separately. [Saved temporary-key checkpoint](PORTING-PARITY.md#november-23-native-saved-temporary-ssh-keys) |
| D2/M1 · Java temporary-key backup restore | Credential review decrypts compatible keys into a sealed vault transaction and binds each reference to an exact eligible new profile and route. Apple Silicon verified an imported key after startup restore by authenticating to pinned OpenSSH and downloading the exact SFTP fixture bytes. | Exercise Review SSH Credentials and Merge on Restart on native desktops. Verify the displayed omissions, subsequent expiry and the login response after an account change. [Java restore checkpoint](PORTING-PARITY.md#november-23-java-temporary-ssh-key-restore) |
| D2 · Project reconnect with a temporary key | A restored project pane can use an exact saved profile with an eligible temporary key when automatic selection is active. The Apple Silicon fixture verified retry after vault unlock over pinned OpenSSH, preservation of the tab and deferral after key expiry. | Exercise the native desktop attention bar, automatic retry after unlock, and expired-key review with an existing project and its saved jump route. [Project reconnect checkpoint](PORTING-PARITY.md#november-23-project-reconnect-with-temporary-ssh-keys) |
| Q1/Q2 · Scheduler and rsync with temporary keys | Saved jobs bind eligible temporary keys to the target and jump logins. The shared native transport checks permission before using each key. Portable backups remove the binding and disable the job. Fedora ARM64 verified an exact-byte rsync download across a pinned jump host. | Exercise job save and route reselection after replacing a key in native Scheduler dialogs. Repeat the applicable rsync and managed-policy cases for the remaining native platforms. [Scheduled temporary-key checks](PORTING-PARITY.md#scheduled-ssh-and-rsync-with-temporary-keys) |
| B4 · GGUF Configure styling | The measured path width, compact two-column form, joined numeric controls and fixed actions follow the tagged Java layout. [Configure checkpoint](PORTING-PARITY.md#november-23-gguf-configure-sizing) | Compare the same layout with the platform theme on the remaining native desktops. |
| D4 · Search matching | Settings and AI Manager use the shared Java matching and ranking rules for visible page and control names, aliases, duplicate suppression and the 40-result limit. Available controls are filtered before ranking. Imported model IDs remain selectable when their normalized names collide. | Compare result labels, ranking and keyboard and pointer navigation against the tagged dialogs on native desktops. [Search matching checkpoint](PORTING-PARITY.md#november-23-settings-search-matching) |
| A2 · Copying a command step's output | **Copy output** uses all masked stdout and stderr retained for the selected step within the existing capture limits. The compact preview still shows the last eight lines. Retained runs use the same action and clipboard policy. | Verify both streams, a line longer than 320 characters and redaction in the native clipboard for live and retained runs. [Step-copy checkpoint](PORTING-PARITY.md#november-23-complete-masked-step-copy) |
| B5 · Knowledge source status | Completed source outcomes and problem counts are restored with the existing index metadata. Windows ARM64 confirmed manual and automatic missing-file results after a process restart, preserved index metadata and automatic recovery of the repaired file. The desktop review recognized the restored manual file. The manual recovery test indexed it and checked the unchanged sibling after reopening the Manager. | Complete the Windows desktop manual indexing flow after repair. Repeat the desktop restart and recovery paths on the other supported native platforms. [Source outcome checkpoint](PORTING-PARITY.md#november-23-knowledge-source-outcomes) |
| A2 · Naming Agent designs | New, Duplicate and Rename use owned prompts with the suggested text selected. Modifying a built-in creates a custom copy with the full preset name and a numeric suffix. The catalog keeps stable IDs and the Settings entry keeps changes in the active draft. | Verify the final compact prompt on Windows ARM64 against Java. Check Rename, deletion, saved names after restart and owner focus on the native desktops. [Naming checkpoint](PORTING-PARITY.md#november-23-agent-design-naming) |
| B4 · GGUF import and backend review | A native multi-file selection opens a private-copy or external-reference review. Windows ARM64 verified both storage choices with the native picker and checked that Cancel preserved the registrations. The manual entry and detached-model reattachment paths remain available. Saving an incompatible backend offers the existing signed-runtime review. | Repeat the storage choices on the remaining desktops. Exercise reattachment of a split model and both backend responses in the desktop. Verify runtime readiness with a compatible signed package. [GGUF checkpoint](PORTING-PARITY.md#november-23-gguf-import-and-backend-review) |
| A2 · Terminal Agent | The design presenters and earlier-run selection paths are implemented. The Log Table shows sortable typed step metadata and a live Duration column that adopts the measured result at completion. A double-click opens the selected masked output or the finished answer. Live step and answer details allow read-only selection and policy-checked partial copying. A right-click offers step actions on retained rows and run actions on the answer or table background, including Earlier runs. Export all finished runs remains available while a different run is active. The export review fixes the selection when it opens. New rows flash for 900 ms and running rows use a 120 ms spinner. Reduced animations keeps the running marker still, omits the flash and updates elapsed time once per second. Reviewed model requests create a running row and update it with the accepted response metadata. An outstanding command review has a masked, read-only Approval row that is removed when the review ends. | The live table, detached history and Settings preview support focused keyboard navigation. Ctrl+Shift+F6 focuses the visible table in the active terminal pane. Tab reaches the detached table and scrolls the Settings preview into view. With live table focus, the Context Menu key or unassigned Shift+F10 opens the selected row's actions. Fedora ARM64 X11 verified focus, row selection, opening details, the keyboard step menu and source terminal input isolation in a docked table. Open details receive more of the dock height, with model usage in a compact scrollable summary. Fedora ARM64 X11 verified Tab entry, row details, exact output copying and reattachment in detached history. It also verified that the Settings preview reveals the table and selected sample output with the keyboard. Verify the keyboard paths on the other native desktops. Columns fit the viewport above font-scaled minimum widths. Verify resizing and numeric alignment on native desktops. Chronological tables follow new rows at the bottom and preserve the reading position when reviewing older activity. Scrolling up exposes Latest activity without selecting a row. Fedora ARM64 X11 verified stable reading through command approvals and continued following after Latest activity. Fixed column headers stay visible during vertical scrolling and follow the data cells horizontally. Column dividers adjust widths within the fitted table. Fedora ARM64 X11 verified Step sorting in both directions, fixed headings while scrolling vertically and horizontally, divider dragging and sort retention after resizing. Verify these paths on the other native desktops. Exercise the narrow viewport, elapsed time, sorting, selected output, approval-row lifecycle, step actions and motion preferences. Verify the decision and repair rows with a configured provider. Exercise the terminal context menu and the keyboard and pointer paths for the Log Table menus and run/step controls on native desktops. Switch runs, copy individual outputs, export one or all finished runs and open an unsaved step as a snippet. |
| D4 · AI Manager search | The search reveals provider presets, CLI and MiniMax fields, reviewed HTTPS options, usage quota and cost, and the Web search controls exposed by the active provider and policy. The profile wizard searches its reachable steps using the same validation as Next. General search reveals the shortcut toggle, alias name, case preference and retained Java settings individually. The Profiles search reveals connection and CLI tests, model discovery and inventory refresh, imported model mapping and the active deletion review. Local AI search opens the relevant Profile roles, Local runtime or Hugging Face subtab and reveals the matching control. Runtime repair searches its signed-package fields and compatible release controls. The search highlights active signed-runtime cancellation and pause, the separate keep and discard actions, and the open llama.cpp runtime removal review. Selecting one of these results preserves the operation or review. Knowledge Store search follows the selected backend, source options and embedding mode, and reveals active folder reviews, source file size edits, store removal controls, profile bindings, Agent availability and embedding consent. Local Models search reaches Hugging Face and the managed GGUF and Apple MLX workflows, including model selection, runtime controls, configuration and active reviews. Imported GGUF and MLX settings can be found by name, format or registry ID. The result highlights the matching row and leaves review and adoption explicit. Memory search reveals imported preferences, active Markdown and entry editor actions, history restore and external file review controls. Skills search reveals the library filter and sort, the direct Java-style Providers entry point, available editor fields and actions, eligible Java preference suggestions, and the fields and actions of the open provider editor. | Select a control by keyboard and mouse, verify its page, scroll position and highlight, and preserve editor-local Find in Skills and Memory. Native widget tests verify keyboard focus and draft retention for GGUF and MLX controls and MLX runtime download policy. Exercise search from a partially completed wizard, repair or model review and verify draft retention and cancellation on native desktops. |
| B4 · Hugging Face model browser | Rust loads additional pages, sorts metadata and resolves missing sizes for the RAM filter. Apple Silicon offers All formats, GGUF and MLX with independent continuations. Review MLX package fixes the commit, exact sizes and SHA-256 digests. Download and register validates the complete directory through the native MLX registry. Paused MLX downloads can be found, reviewed, resumed or discarded after restart. | Exercise continuation, sorting, RAM filtering, format changes, review, installation and recovery in the native desktop. The native transfer has passed a live pause, resume and registration test against a pinned public Hub package. [Java browser](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/LocalModelManagerPane.java#L803) · [Rust MLX Hub transfer](src/local_ai/managed_models/huggingface/mlx/download.rs) |
| D2 · Connection folders | Drag writable SSH and serial rows to a local folder in the tree or sidebar. List and map share successive Undo. | Exercise native pointer gestures across the list, sidebar and map, reverse two saved moves and check read-only Teamwork entries. |
| Q1 · Scheduler target picker | The job editor can filter saved SSH connections and groups by name, host or group path and apply a batch to one draft. | Exercise the picker on native desktops with an individual target, overlapping groups and a source-qualified Teamwork group. Verify search, reviewed order and cancellation. |
| T2 · SFTP | The Local and Server panes review and clean up regular transfer parts in the displayed folder. Current-process stages are reserved during transfer and checked again before removal. | Exercise the confirmation and cancellation on native desktops. Verify protected deletion over a configured SSH route, including a replacement during review and a partial failure. |
| D4 · Logfile Manager | Saved connections' existing log folders appear before their first logging session. Manual and logger-discovered origins are tracked independently. | Check deduplication when two connections share a folder, registration by the logger and forgetting a manually added folder. |
| U1 · Update prompts | About offers a version-specific skip and a reminder on the next local day. Successful verified publication suppresses that version's automatic prompt. Startup and periodic prompts have separate repeat limits, and manual checks can still show the version. | Exercise the native About dialog and background notices across restart against a configured managed feed and an owner-published stable release. |
| M1 · Portable data | Java folder colors with ambiguous or unmatched paths are available for explicit assignment in the folder color editor. Exact existing favorite profiles offer explicit review. Java backups retain custom OS symbol definitions for review in the symbol manager, including definitions without connection references. Imported connections retain pending assignments. | Exercise folder color and favorite review on native desktops. Exercise the symbol manager and connection picker with a missing PNG, a supplied image and a colliding local badge ID. Verify reviewed assignments after restart. |

### Implemented workflows awaiting acceptance and requested extensions

External skill providers (U3) accept an explicitly selected vault entry or an
environment variable. The provider editor binds a vault assignment to the
provider type, URL and HTTP Basic username. Changing those fields requires a
fresh credential choice. Search, preview, updates and provider tests check the
unlocked vault revision and the request URL. Java settings import retains
supported providers as inactive suggestions. Adoption starts from a disabled
draft and requires a separate local credential choice for providers with Java
authentication. The migration report counts omitted encrypted secret fields.
Verify import review, credential selection and removal on native desktops
and exercise configured provider deployments.

| Work item | Implemented behavior and next verification or extension |
| --- | --- |
| Q1/M1 · Verify and extend Teamwork scheduling | Rust supports direct and jump-host Teamwork SSH targets and source-qualified groups with descendants for commands, scripts, snippets and SFTP actions including reviewed folder sync. The Scheduler retains the reviewed source, endpoint, effective login and host-key pin and rechecks the selection before execution. Verify native OpenSSH firing and reviewed rsync with both local authentication methods. Verify protected SFTP actions on shared routes. The requested Rust extension still needs scheduled Agent/Swarm on the live Teamwork catalog. Java's picker draws on locally configured connections. [Scheduler eligibility checks](src/scheduler_window.rs) · [Native Scheduler targets](src/scheduler_window/connection.rs) · [Shared route](src/connection_store/teamwork/reviewed_route.rs) |
| T3/T4/Q1/Q2 · Verify SFTP comparison and transfer recovery | After generating an AI draft, Rust can switch among the masked originals and either original against the draft. Verify the picker in the native desktop. Rust supports a new remote name and reviewed replacement of the right server file or another selected existing file through the Linux, macOS and Windows helpers. Verify macOS and Windows replacement over native OpenSSH, including permissions, backup and conflict recovery. The Linux helper reclaims marked stages left by a terminated helper. Verify native recovery through the deployed helper. [Java comparison](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/SftpFileDiffDialog.java) · [Draft preview](src/sftp_window/compare/ai.rs) · [Server save](src/sftp_window/compare/ai/server.rs) · [Helper cleanup](crates/server-archive/src/server/privileged_rsync/upload/handoff.rs) |
| S2/S3 · Verify mixed-language snippet workflows | The snippet assistant detects embedded programs locally and distinguishes ordinary scripts from orchestration formats. Whole-script migration offers the Java target languages, a reviewed request, and a before/after diff with migration notes. Applying updates the snippet language and the extension of an existing filename. Undo restores the original source and metadata. Mixed steps in Azure Pipelines, GitHub Actions, and Jenkins can use the same review workflow with host-specific targets and byte-preserving scaffold checks. Explicit platform conversion offers the Java destination formats and checks the result against the selected format and any selected step language. Selected security fixes can start with optional migration. The editor previews the migrated source and reviews the separate fix request before sending it. Apply and Undo cover the combined proposal. Full-analysis Apply can begin with the chosen migration and pass the validated source to the finding and hardening stages. The migration checkpoint supports resuming the remaining work or reviewing the completed migration. Verify the native workflows with configured providers. [Java language unification](https://github.com/chardonnay/korTTY/blob/v2.26.0/app-docs/site/docs/en/reference/language-migration.md) · [Native detection](src/snippet_window/languages/mix.rs) · [Migration plans](src/snippet_window/ai_assist/migration.rs) |
| S2 · Verify snippet review actions | The standalone security report sorts findings by severity and lets users select individual findings before reviewing a fix request. Fix requests bind the selected findings to the exact reviewed source. Verify the desktop flow against a configured provider. Full code analysis has a separate selection workflow. Formatting and syntax checks offer a reviewed AI fallback when a local tool is unsupported or unavailable. Verify the fallback and its request and result reviews on native desktops. [Java report](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/SnippetSecurityReportDialog.java) · [Native review and fix](src/workspace/snippet_access/ai_assist.rs) · [Native tools](src/editor_window/tools.rs) |
| S1/S2/M1 · Verify editor controls and migration | The library/editor divider resizes and remembers its position, including a matching Java backup setting when unset. Java restore maps open and active tabs to exact restored snippet records, including identical native records with different IDs. The snippet editor honors the Java preference for optional AI instructions and restores independent font sizes for alternatives, diffs, and reviews. Alternatives have read-only code previews, expansion, and selection copying. Verify divider dragging, restoration of tabs and font sizes, preview interaction, and reviewed requests on native desktops with a configured provider. [Java workspace](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/SnippetWorkspaceDialog.java) · [Native layout](src/snippet_window.rs) · [Tab persistence](src/snippet_window/session.rs) |
| M1/B4 · Complete portable data adoption | Java backup retention and frequency initialize unset native backup settings. ASCII Art applies restored image-size and copy-format choices where local preferences are unset. The Java preview explains highlight ID collisions and binds archive references to a separate imported copy. The toolbar editor can assign unresolved Java buttons to saved connections or groups. Continue explicit review for other ambiguous group references. Native restore can reattach reviewed standalone and split GGUF files, GGUF vision pairs, and MLX directories to their original model IDs. Java model-catalog adoption already exists. Verify native desktop review and startup after restore on another host. [Field and identity audit](#remaining-field-and-identity-audit) · [Portable model metadata](src/profile_backup/data/ai.rs) |

The reduced editor has a multi-language completion list, a configurable
shortcut and reviewed opt-in inline AI suggestions after a typing pause.
Remaining gaps in the language data and native desktop verification are
tracked under [Editor, snippets and diagrams](#editor-snippets-and-diagrams).
Also decide which remaining consumers can accept server-selected AI models
(Swarm, scheduled AI, vision and embeddings). Track the
native and external release requirements separately under [V1–V7](#native-acceptance-and-release-gates).

### Work package references

The following work packages record the supported cases and remaining
implementation, acceptance or scope decisions. In this table, J code
paths are relative to `src/main/java/de/kortty/`, J manual paths to
`app-docs/site/docs/en/`, and R paths to this Rust working tree.

| Work package | Implemented scope and remaining work | Source comparison and completion condition |
| --- | --- | --- |
| B1 / M1 | Manual and server-selected profiles preserve validated exact loopback chat and OpenAI-compatible HTTPS completion paths, including custom OpenAI-compatible gateway paths, case variants and a trailing slash. Automatic inventory requires the canonical route. An omitted mode with a Java-blank model infers Auto where discovery is supported. Explicit `DEFAULT` restores as a server-selected chat profile. A reviewed Java global assignment can be adopted as the Terminal Chat default. A reviewed Java Text assignment applies to Terminal Chat, manual questions and journal note translation. A reviewed Java Agent assignment applies to standalone Ask and Run. Reviewed Java Coding and Security assignments apply to interactive snippet and file assistance. Reviewed loopback and exact HTTPS Anthropic Messages profiles preserve the Messages route and explicit model, including Java Auto and Default profiles. Journal questions support saved server-selected HTTP loopback and reviewed HTTPS profiles; text summaries support reviewed OpenAI-compatible HTTPS and compatible loopback HTTPS. Java Journal-role adoption is implemented. Other Java path families and the named exact-model consumers below still need a disposition. | J `core/AiServiceFactory.java`, `core/LocalLmModelResolver.java`, `model/AiProfile.java`. R `src/ai_manager/profiles/java_backup.rs::read_http_profile`. Preserve endpoint, protocol, source identity and earlier import fingerprints. Verify native review against a configured HTTPS gateway and assess explicitly reviewed use in other foreground workflows. |
| B4 | Finish the local-model dialogs and verify the managed runtimes. Signed mlx-lm 0.31.3 passed Qwen3 On/None, GPT-OSS Low/Medium/High, stale-setting rejection, Java profile adoption and startup rollback. GGUF vision and external inference with a single file and with three shards passed on Apple Silicon, Fedora ARM64 and Windows x64 under emulation. MLX settings offer the Java Never unload checkbox and retain the unsaved timed value while it is checked. | J `ai/mlx/EmbeddedMlxAiService.java`; R `src/ai_manager/profiles/imported_model.rs`, `src/local_ai/mlx_models/reasoning.rs`, `src/local_ai/streaming/harmony.rs`, `src/local_ai/managed_models/tests.rs`. The [MLX settings round trip](PORTING-PARITY.md#november-20-mlx-idle-timeout-and-never-unload) passed on Apple Silicon. Verify its desktop interaction, native Windows inference and additional signed runtime versions when available. |
| B6 | Newly added files and folders default to 50 MiB per file. Reviewed UTF-8, PDF and folder settings accept up to 1 GiB, with a 500 MiB scan limit. Legacy approvals retain their earlier bounds. Native Windows and Fedora ARM64 workers processed 500 MiB fixtures. Windows also passed cancellation and cleanup tests. | J `features/rag.md` and `rag/RagSourceScanner.java`. R `src/ai_manager/local/knowledge/source.rs` and `src/knowledge_pdf.rs`. Apple Silicon passed packaged-worker extraction, the exact 500 MiB limit, cancellation and spool cleanup. The local backend supports 5,000,000 passages and 64 GiB per snapshot. Above 8,192 passages, search reads vectors from the validated snapshot and index construction stages vectors in a temporary file. The expanded local format uses a 32-bit passage count. Fedora ARM64 passed staging, exact search, refresh and replacement publication with 524,288 dense synthetic 1,536-dimensional vectors. Apple Silicon passed the same workflow at 1,048,576 passages. Windows ARM64 passed 524,288 passages after correcting publication with an open reader. Verify higher corpus tiers with configured providers and real sources. |
| B8 | Verify LM Studio MCP over loopback HTTPS with a configured provider. Java Toolpack profiles can retain an inactive plugin hint. Adoption in AI Manager requires endpoint trust, a locally enrolled credential and exact user-entered tool names. | J `core/AiServiceFactory.java`, `core/LmStudioNativeAiService.java`; R `src/ai_manager/profiles/web.rs`, `src/local_ai/web_search/mcp.rs::search`. Verify native adoption and invocation with an installed LM Studio and the configured plugin. Preserve certificate validation, exact integration, masked query/excerpt review and model binding. Broader automatic web selection from later command output requires a scope decision. |
| T3 / T4 | Reviewed existing-file replacement destinations and native acceptance of macOS file drag-out. AppKit Copy starts from the original mouse gesture in the SFTP Manager or terminal sidebar. Reviewed AI replacement of the compared right server file uses the native Linux, macOS or Windows helper. | J `ui/SFTPManagerTab.java`, `ui/TerminalRemoteSidebar.java`; R `src/sftp_window/drag.rs`, `src/workspace/remote_dock/desktop_drag.rs`, `src/sftp_window/compare/ai.rs`. Verify the new AppKit drags in Finder, including cancellation, delayed file reads and route or policy revocation. General replacement must preserve reviewed identity, metadata and recovery under external changes. |
| Q1 | rsync accepts separate vacant-output batches and combined transfers mixing files, named folders and folder contents. Each saved route has a reviewed source projection and destination. Combined transfers publish one vacant or reviewed existing tree with scoped mirror deletion and recovery. Existing Linux uploads use the protected receiver helper. Recurring existing-directory approval binds calendar and Chain firings to the eligible saved routes and advances each receiver only after confirmed publication. Recurring uploads require the Linux v3 helper. Passwordless elevation supports reviewed files, directories and combined sources per route with separate protected Linux read, create and replacement roots. An upload can replace one exact reviewed Linux file. A direct ordinary or passwordless download can replace one reviewed local file per route. Both retain the original and preserve supported receiver metadata. Confirmed recurring file replacements record the exact successor and consume no removals. Windows supports a reviewed executable with a private staged DLL bundle. One-use changed-file SFTP downloads are implemented for Windows and Unix. The upload path supports login-owned private remote files with recovery. Explicit recurring SFTP calendar approval supports ordinary replacements in both directions with per-firing bounds and recovery. Recurring SFTP Chain approval passed pinned OpenSSH acceptance on Fedora. Privileged file transfers support one-use and bounded recurring replacement approval. Privileged folder uploads and downloads support one-use and recurring merges, including confirmed vacant-to-existing receiver progression. Configured desktop deployments and broader target metadata compatibility remain open. | J `features/jobscheduler.md`, `jobscheduler/JobSchedulerJobRunner.java`; R `src/job_executor/rsync.rs`, `src/job_executor/rsync/native/mixed.rs`, `src/scheduler_window/rsync.rs`, `src/scheduler_window/sync.rs`. Verify recurring approval and reapproval in native Scheduler windows, recovery after interrupted publication and guarded SFTP replacement. Fedora verified combined elevation with real sudo and pinned OpenSSH. Verify configured deployments and remaining native editor paths. Local file receiver regressions and the opt-in Fedora and Windows pinned SSH cases passed. Complete native Scheduler editor and remaining platform checks. Verify recurring passwordless receivers on remaining platforms. The reviewed creation-to-replacement layouts are implemented; derive any additional destination layout from a pinned Java case. |
| Q2 | Privileged SFTP transfers support one-use and bounded recurring replacement of a reviewed local or server file in both sudo modes. The ordinary and sudo Linux Scheduler routes implement opt-in archive path reuse per firing, ZIP Add, TAR regeneration and recovery for server and local output. Fedora passed isolated helper checks with real sudo and PAM. Pinned OpenSSH client fixtures passed for both sudo modes. Ordinary SFTP and the Linux sudo route support reviewed explicit-class symbolic permissions and owner/group name resolution. The Linux sudo archive helper creates root-private ZIP, including vault-backed password ZIP, and streams a verified local copy from an administrator download root. A separate protocol supports a reviewed vault-backed sudo challenge. | J `features/jobscheduler.md`, `features/sftp.md`, `ui/JobSchedulerDialog.java`. R `src/scheduler_window/remote.rs`, `src/scheduler_window/ownership.rs`, `src/job_executor/remote/server_archive.rs`, `src/job_executor/remote/privileged.rs`. Verify repeated schedules, local-copy replacement, cancellation and same-path archival through authorized sudo. Check named resolution, symbolic modes, changed pins and denied policy. |
| M1 | Java backups fill unset Coding Agents, remote sidebar, supported keyboard and recording preferences. Restored terminal preferences include shell tab titles, command timestamps, link detection, transparency and silence threshold. The Scheduler menu status and field-by-field terminal notification settings also restore. Supported SFTP transfer, idle and archive defaults fill unset fields. The preview identifies replace-on-conflict. Journal appearance, capture format, link behavior, and PDF branding fill unset fields. Terminal selection, paste protection and pacing, and compatible Quick Select patterns fill unset fields. Quick Select and terminal highlighting preserve supported Java expressions, including named captures, word boundaries and final-line anchors. The bounded matcher also imports lookarounds, numeric and named backreferences, atomic groups and possessive quantifiers. Inline and scoped Java case, Unicode-class, comments and line flags restore. Java grapheme escapes use the bounded matcher with grapheme classifications matching OpenJDK 25.0.2 and Unicode 16.0. Numeric and named backreferences apply the ASCII or Unicode case mode active at the reference. The Unicode backreference table matches JDK 25. Supported Unicode case-insensitive bracket classes match Java singleton and range rules. Unicode case-insensitive literals preserve Java single-character, literal-run, and quantifier boundaries. All 18 Java-named character properties use Unicode 16.0 and Java-specific identifier and whitespace rules. POSIX and binary Unicode properties preserve Java flag handling and category aliases. Java script and block selectors use Unicode 16.0 with Java's short script aliases and historical block names. Java octal, control, paired UTF-16, named scalar and short category escapes restore, and the bounded matcher preserves the previous-match anchor and subsequent empty matches. The generated Unicode 16.0 scalar name catalog matches OpenJDK 25.0.2. Java quote delimiters are removed before parsing expressions. Empty quote sequences preserve adjacency in classes and elsewhere. Isolated UTF-16 surrogate literals support bounded repetition, alternation and fixed-width lookbehind. Matching keeps UTF-16 positions internally and returns complete UTF-8 characters when displaying partial matches. Numbered and named backreferences can compare partial UTF-16 captures and follow Java case modes. Surrogate literals and ranges in character classes, the surrogate category and surrogate blocks use UTF-16 matching. Negation and intersections preserve low-surrogate membership. Other character predicates carry their surrogate membership in that mode. The UTF-16 matcher supports variable-width lookbehind with Java's width limits, nearest-first candidate order and original endpoint check. The bounded Java matcher uses UTF-16 positions for ordinary predicates and empty matches without an explicit surrogate atom. Linear Java matching returns empty matches after nonempty matches and checks low-surrogate positions after empty matches. The bounded matcher applies Java word boundaries and selects code-unit or code-point scanning from the source predicates. Regular word assertions use the bounded Java matcher when supplementary characters or nonspacing marks can affect them. Escaped supplementary characters in literal runs preserve Java's scanner selection. The 3,251-case comparison matches OpenJDK. The recorded Java line-ending cases cover CRLF retry, direct and grouped repetition, and lookbehind. Imported whole-word highlight rules retry alternatives inside Java boundary assertions in the terminal and logfile viewer. Inline canonical-equivalence flags cover bracket classes and explicit Unicode properties. Pending Java highlight restores recover the marker for legacy literals. Reimport upgrades a legacy remapped set in place when its content-derived ID proves the relationship. Ambiguous legacy source IDs preserve the local set and show the explicit mapping to a separate imported set during review. Audit other regex syntax and the limits of the bounded fallback. Compound class negation and property complements subtract from the scalar range, including at U+D7FF and U+E000. Nested classes and intersections preserve Java set operations, including empty operands, lone ampersands after nested operands and vertical-whitespace escapes in ranges. Remembered-window and dashboard options and fixed startup geometry with signed coordinates and maximization fill unset fields. Java startup applies stored bounds and maximization. The restore preserves those values when the record includes fullscreen metadata. Snippet analysis diagram, modularization, and source-code export defaults fill unset fields. File-editor caret preferences and the active Java snippet editor font and palette fields also restore. The logfile viewer uses native terminal highlight sets and a saved link, path and keyword presentation. Java backups restore compatible custom highlight sets for presentation and resolve logfile viewer references at ID conflicts. Existing local selections are preserved. Verify native clicks and presentation across desktop backends. Live terminal master and alternate-screen settings fill unset fields. The global default and imported SSH, local shell and serial choices follow remapped custom IDs. Transactional restore verifies the reviewed mapping before publishing connections. Serial connection editing exposes the same set selector. Custom Journal markers and ordered rules restore with exact remapping at conflicts. Remaining Java backup work covers residual regex semantics, unmapped global settings and unresolved or ambiguous identities. Native inspection of retained analysis snapshots and diagrams is implemented and awaits desktop acceptance. Java GGUF and MLX registry metadata restores as inactive hints for separately reviewed native models. Saved snippet and folder analyses can restore from matching Java library entries with inert workflow evidence and verified source snapshots. Read-only run history includes reviewed options, stages, completion markers, checkpoints and model provenance. Unique built-in skill identities restore historical usage and Java-specific size measurements. Custom, deleted and otherwise unmapped Java IDs restore as separate read-only usage histories in native skill reports. Exact new SSH, local shell, and serial profiles can restore Memory documents, history and fully matched mixed archives using canonical and supported legacy scope IDs. Default, text, coding, Agent and snippet security role references restore as inactive suggestions. Java RAG definitions restore as inactive suggestions. Java Scheduler file transfers, sudo staging, ordinary or privileged archives, and sudo commands, snippets and file actions restore as disabled drafts. Staging, archives, sudo file actions and sudo virtual terminals bind exact individual routes. Adoption requires a reviewed sudo mode and current policy. Archive and protected transfer workflows verify their installed helper where required. Java rsync jobs restore as disabled route-bound drafts with independently selected native destinations and fresh review. | J `core/BackupManager.java`, `features/backup.md`. R `src/profile_backup/archive/java.rs::classify`, `src/profile_backup/java_migration.rs`, `src/profile_backup/java_scheduler.rs`. Existing saved connections support explicit AI Memory destinations. Colliding Java filenames are reported and exact archive IDs restore independently. Windows ARM64 verified the assignment dialog and restart. Verify the workflow on macOS and Linux desktops. Verify serial Memory in a native desktop terminal. Verify adoption of imported roles and model settings on native desktops. Keep credentials, host pins, executable paths and execution approvals subject to separate review. See the detailed M1 row for remaining Scheduler and Memory cases. |
| U4 | ZIP/TAR.GZ and optional Windows MSI and Linux ZIP/DEB/RPM/Pacman/Flatpak staging exist. ARM64 passed the real Java-to-Rust Pacman downgrade and Flatpak replacement. Host-shell integration and required local Landlock isolation passed in the Fedora ARM64 Flatpak. Both MSI architectures passed Windows Installer database and cabinet verification using retained executables. Native Apple Silicon DMG creation, app copy and launch passed. | Java v2.26.0 release assets and R `packaging/src/main.rs`, `packaging/src/dmg.rs`, `packaging/src/msi.rs`, `packaging/src/linux.rs`. Verify the remaining clean-system install, upgrade, removal, dependency and signing cases on native systems. Repeat the Flatpak launch and Landlock checks on x86-64. The earlier notarized Apple Silicon ZIP covers its recorded source. |

For OpenAI-compatible and native LM Studio chat, Java DEFAULT omits `model`
even when a stored model name is present. Rust preserves that behavior in
OpenAI-compatible, native LM Studio chat and MCP requests. The Terminal Chat and MCP search reviews identify
the server as the model selector. Standalone terminal Agent Ask and Run also
offer loopback profiles and trusted OpenAI-compatible HTTPS profiles. A
saved Agent role assignment, including an adopted Java assignment, selects
the profile for standalone Ask and Run. HTTPS requires a saved Agent opt-in,
and the outbound request still requires review.
Reviewed Java Coding and Security role assignments can also select the
server-default profile for interactive snippet and file assistance. The
review shows the destination and identifies the server as the model selector.
Standalone Agent Ask and Run also use an eligible Coding assignment when
no Agent assignment takes precedence. The same HTTPS opt-in applies.
The reviewed full code analysis chooser accepts server-selected profiles
for single snippets and folders and follows the Coding assignment. A report
records the server selection mode. Verification of accepted changes may use
the same profile through a fresh analysis review. HTTPS split planning,
module generation and repair use separately reviewed requests and bind the
server-selection mode and endpoint. Local split planning and generation
can use a saved loopback server-selected profile. The plan retains the
reviewed profile revision and blocks generation if that revision changes.
Applying findings to snippets and folders also accepts saved loopback
server-selected profiles. Interrupted snippet applies retain the approved
profile revision. Reviewed diagrams from saved analyses also allow saved
loopback server-selected profiles and recheck the profile before a repair.
Single-snippet and folder apply accept saved trusted HTTPS profiles. The
folder review checks all project context, including files outside the
selected findings. Both workflows check each request and returned
replacement for protected values and require a separate review before
publishing changes. The private TLS fixtures cover the exact reviewed
requests for folder analysis, split planning after report storage, and
accepted-file publication. Verify these workflows against a configured
external HTTPS gateway.
Model discovery is skipped. Provider-reported
metadata can inform usage records after a response. Exact-model workflows keep
their own profile restrictions. APIs that require an explicit model can reject
these requests. [LM Studio documents that rejection](https://lmstudio.ai/docs/developer/rest/streaming-events#error).

The following work requires native acceptance or a scope decision.

- **Native acceptance, V1–V5:** window/split movement, geometry and focus,
  notifications, supported shells, the 21 terminal profiles, editor/diagram
  interaction, configured providers and installed signed GGUF/MLX runtimes.
  Windows ARM64, Apple Silicon and Fedora have useful partial evidence;
  native Windows x64, macOS Intel, Wayland and X11 need their remaining cases.
- **Security and release, V6–V7:** resolve the recorded dependency advisories,
  obtain independent external SSH review, build and verify one fixed source
  snapshot per target, and measure startup, idle memory and p95 input latency.
  An existing test count or notarization does not cover subsequent source edits.
- **Product decisions** cover the import and unattended use rules for saved
  temporary keys, automatic web requests using later command output, and round-wide
  approval of future commands across Swarm targets. An explicit grant for one
  run or one target already exists. Larger or dynamic Swarms, incognito scope,
  optional telemetry and third-party effects also need decisions. Review
  additional language coverage against the agreed reduced-editor scope.
- **Explicit scope/deferments:** both Mosh modes are excluded; application
  text stays English; JVM/JMX/JAR mechanisms use native equivalents. Real
  IBM/HP and physical serial checks remain deferred by the user.

## Recorded implementation checkpoints

The [existing-folder Scheduler and skill history checkpoint](PORTING-PARITY.md#scheduler-rsync-uploads-to-existing-linux-folders-and-java-skill-statistics)
connects the native receiver protocol to saved uploads and adds read-only
statistics for unbound Java skill IDs.

The [existing rsync receiver, archive sudo and Java history checkpoint](PORTING-PARITY.md#existing-rsync-receiver-helper-archive-sudo-credentials-and-java-apply-history)
adds a protected Linux receiver protocol for an exact tree, vault-backed
sudo for installed Scheduler archives, and read-only Java apply-run details.

The [scheduled transfer and archive checkpoint](PORTING-PARITY.md#scheduled-transfer-batches-protected-archive-copies-and-java-archive-targets)
adds Unix rsync source batches, Unix one-use replacement downloads, protected
archive copies and exact-route drafts for ordinary Java archive fan-out.

The [knowledge, archive and Pacman checkpoint](PORTING-PARITY.md#java-knowledge-and-archive-drafts-protected-zip-and-pacman-packaging)
adds inactive Java RAG and Scheduler archive migration, AES-256 ZIP through
the protected sudo helper, and native Pacman packaging.

The [exact HTTPS, Java model registry and password ZIP checkpoint](PORTING-PARITY.md#exact-https-routes-java-model-metadata-and-password-zip)
adds reviewed exact completion paths, inactive native model metadata,
password ZIP on the ordinary Scheduler server route and streamed PDF input.

The [loopback Messages, analysis history and archive checkpoint](PORTING-PARITY.md#local-messages-profiles-analysis-histories-and-verified-archive-copies)
adds reviewed local Messages profiles, verified Java analysis history restore
and optional digest-verified local copies of Scheduler server archives.

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
profile role, journal chat transcripts and exports, and the native editor
structure panel are present. Finished terminal Run presentations now offer
Chat with a follow-up field beneath the pane, including Classic. When Chat
is unavailable, the offer and the entries in the existing run menus stay
visible and disabled. The status pill, Orbit, Minimap, and the toast badge
now expose the shared run actions and earlier-run chooser on right-click.
Fedora verified Spotlight, Orbit, Minimap, the toast badge, and managed
Chat denial under X11 and native Wayland. Windows ARM64 verified
Copy answer from Spotlight and Continue in AI Chat from the terminal
footer. A2 tracks the rest of the Windows matrix and the macOS workflow.
The design editor, step output and shared settings-search behavior have
[specific implementation gaps](#confirmed-implementation-work).
[Terminal handoff comparison](PORTING-PARITY.md#november-21-terminal-run-chat-handoff).

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
  The snippet editor also offers built-in and remembered language
  labels. Java's `customSnippetCodeLanguages` list appends validated,
  normalized entries to the existing Rust list on additive restore.
  Verify selecting and remembering labels across native editor windows.
  Snippet editors and previews share saved line-number and word-wrap
  toggles. Java `snippetLineNumbers` and `snippetWordWrap` fill unset
  Rust choices independently. Unwrapped lines support horizontal
  scrolling and caret tracking. Verify both switches during live
  editing, scrolling and window transfers on native desktops.
  Snippet and folder analysis history supports a saved 1–20 report
  limit, with a default of five. Java `snippetAnalysisHistoryMaxSize`
  fills the unset Rust preference. Saving a new report applies the
  limit and preserves pinned records, verified evidence and pending
  recovery. In Settings, lowering the limit previews how many reports
  across current snippets and folders would be removed as new analyses
  are saved. The preview and publication share the same retention
  rules. Verify both on native desktops.
  The separate `snippetAnalysisMaxStoredContentBytes` preference fills
  an unset Rust source limit. New report, apply and checkpoint source
  fields use the lower of the user limit and the managed ceiling.
  Existing snapshots remain subject to the managed ceiling and the
  file budget. Verify the storage control on native desktops.
  Snippet editor tabs capture debounced source versions and navigate
  them in the editor. Saving a snippet persists its versions. Java's
  `snippetHistoryMaxSize` restores as an unset global preference, and
  the private library restore preserves saved versions and per-snippet
  overrides. Verify the slider, tab transfers, and overrides on
  native desktops.
  The Structure panel restores unset visibility and requested width
  from Java's `snippetStructurePanelVisible` and
  `snippetStructurePanelWidth` independently. Verify drag and reopen
  behavior on native desktops. The analysis report can appear beside
  its matching editor with a draggable width. Java's
  `snippetAnalysisPanelVisible` and `snippetAnalysisPanelWidth` fill
  unset fields separately. Opening an editor with saved analysis
  history reopens the enabled panel. Narrow layouts and active reviews
  use the full report tab. Verify docking, resizing and source jumps
  on native desktops. Apply progress can move between the report and
  a right-hand dock. Live and same-session runs follow the saved
  placement, and restored runs can be docked explicitly. Java
  `snippetAiProgressDockVisible` fills the unset placement preference.
  Verify moving live and saved progress, narrow-window fallback, and
  opening review from the dock on native desktops.
  The saved code-analysis report font and the default expansion state
  of classic hardening options restore from Java's
  `codeAnalysisFontSize` and `codeAnalysisHardeningExpanded` when the
  matching Rust preferences are unset. The analysis preview and
  enlarged flowchart share a saved diagram-options toggle. Java's
  `codeAnalysisDiagramOptionsExpanded` fills that preference when
  unset. Project analysis also saves its diagram visibility independently
  of automatic generation, and Java `codeAnalysisProjectDiagramVisible`
  fills that unset choice. Snippet diagrams and analysis previews share
  a saved vertical or horizontal layout. Java `snippetDiagramLayout`
  fills the unset layout preference. Snippet diagrams and analysis
  previews share the saved system, light or dark palette and a custom
  light canvas. SVG and PNG diagram exports use those same colors.
  Java `snippetDiagramColorMode` and `snippetDiagramBackgroundColor`
  fill the matching unset preferences independently. Verify controls,
  OS theme changes and reopen behavior on native desktops.
  Java's saved classic and input-hardening selections, input enable
  switch, and file-size limit fill unset Rust preferences independently.
  Snippet Analysis and Terminal Workflow use those defaults when
  initializing reviewed requests. Verify the restored choices in both
  native dialogs.
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
  on native desktops. Live running cards use Java v2.26.0's 15-minute initial
  age and minimum interval after a successful preview for the same command.
  The native viewer exposes the explicit Summary now override. Journal cards,
  closing summaries, single-journal questions and screenshot descriptions store
  observed token totals. The journal manager uses configured profile rates and
  marks incomplete estimates, including grouped totals with mixed currencies.
  Verify live desktop controls and configured provider usage on native targets.
  Multi-journal requests and CLI answers without token reports have no
  per-journal attribution. New Java profile imports retain supported token
  prices and currency. Usage stores the selected profile's identity and display
  name. Re-evaluation suggests that profile when it is still available and
  resolves original Java IDs through unique imported profile provenance.
  Reviewed OpenAI-compatible HTTPS profiles can use the server's default model
  for journal summaries. Manual runs require an explicit review. Recording and
  Scheduler use separately saved approvals. Changing the model selection mode
  invalidates the existing approval. A private TLS fixture verified the
  evidence-free probe, masked summary request, omitted `model` field and
  revocation on macOS ARM64 and Fedora ARM64. Verify the settings and run
  against a configured provider on a native desktop.
- **P1/U3 — Startup and backup.** The managed password gate also supports an
  opt-in personal startup password. Java prompts by default and offers saved
  unlock. The native default remains off and saved unlock needs a scope
  decision. Encrypted profile backup now includes saved AI and Swarm chats,
  eligible AI memory, skill statistics, and AI model and knowledge-source
  metadata. Restore retains local memory/statistics and removes portable AI
  approvals. The AI Manager can reattach a reviewed local GGUF file or complete
  shard set to an empty restored registration, retaining its ID and settings.
  Vision reattachment publishes the verified private copies of weights and
  projector together. MLX reattachment reviews the local directory, matches
  its saved asset inventory and publishes the private copy as one directory.
  Complete native desktop and cross-host restore acceptance.
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
  Supported Java HTTPS, loopback, embedded-model and CLI AI profiles restore
  as drafts subject to current managed policy. Local profile names take
  precedence. AI Manager reviews the endpoint, registered model mapping, or
  native executable and literal arguments before use. A default is chosen
  separately. Recording and automation Journal profile assignments and
  evidence limits have explicit adoption suggestions. Default, text, coding,
  Agent and snippet security roles, model registry metadata and RAG definitions
  can also restore as inactive suggestions. Other global settings and unresolved
  scopes remain in M1. Provider credentials and execution approvals require
  separate native review.
  Supported Java Scheduler command, snippet-script, AI Agent, AI Swarm, and
  file jobs restore disabled. They bind to SSH connections added by that restore.
  Selected connection folders retain their ordered target expansion, including
  nested folders. Each folder's expansion must exactly match the source backup.
  See the [Scheduler folder restore checkpoint](PORTING-PARITY.md#java-scheduler-connection-group-restore).
  Snippet jobs also require matching snippet and variable definitions. Existing
  job IDs take precedence.
  The Rust importer dispatches all 15 action names in the tagged
  `JobActionType.java`. Unsupported option combinations, unresolved or
  conflicting target and snippet references, Scheduler credentials, host
  pins, history and webhooks have their own omission or review paths.
  Later Windows ARM64 runs supersede the earlier interrupted application run.
  The connection-group checkpoint passed 3,308 application tests, including
  encrypted backup/startup replay coverage. A later historical checkpoint passed
  3,320. Native desktop review and cross-host acceptance remain open. M1 tracks
  remaining fields, action options and shared target identities. The
  [credential backlog](#confirmed-implementation-work) tracks persisted
  temporary keys and their migration separately.

The Rust packager stages ZIP archives for Windows and macOS and a TAR.GZ for
Linux. The optional `--linux-zip` flag emits a portable Linux ZIP and checksum
alongside the TAR.GZ. Matching Linux build hosts can also request
`--linux-deb` and `--linux-rpm`. Native install and clean-system verification
remain in V7. MSI, DMG, Flatpak and Pacman staging are also implemented
under U4. Their remaining work includes native distribution acceptance.

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
  target. A separate explicit per-target grant can cover later ordinary
  commands in that run. Elevation and managed per-command confirmation retain
  their own reviews. Native scheduled Swarm
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
  The shared HTTP provider applies compatible MiniMax thinking settings and
  handles native response envelopes and streams. The Java importer retains an
  exact supported MiniMax native endpoint in its own connection mode. Regional
  accounts and live inference still need acceptance.
- **B1/B5/J3 — AI profiles and knowledge.** Java has model-family prompt
  presets and a separate per-store opt-in for autonomous retrieval in Agent,
  Planning, Swarm and jobs. Rust supports those review flows alongside
  Text and Coding retrieval. Live review supports local indexes and Qdrant.
  Scheduler reviews support local, anonymous Qdrant and vault-backed Qdrant
  indexes. Real-corpus performance at the implemented local ceiling and the
  separate Qdrant capacity review remain open. Durable environment-key
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
  selection. Native desktop acceptance, including the implemented macOS
  AppKit drag-out in Finder, remains open. Native archive dialogs have per-archive options and saved
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
  a reviewed optional `.bak` through the native Linux, macOS or Windows helper. It
  requires a protected directory and matching ownership, bytes and access metadata.
  The same helper supports another existing file selected by folder and name
  or from the server listing. Native macOS and Windows OpenSSH acceptance
  remain open. The draft result lets users switch
  between both masked originals or compare either original with the draft. Native UI and
  configured provider acceptance remain open.
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
  Optional startup unlock is implemented and awaits the remaining native
  checks. Reviewed Java temporary-key restore is implemented. The
  [implementation backlog](#confirmed-implementation-work) tracks
  unattended use. The backup review, saved-connection editor and login
  forms have their own acceptance checks.
- **D5/A4 — Coding-agent indicators.** Native badges exist on macOS and
  Windows. Linux emits a persistent LauncherEntry D-Bus counter with an
  installed desktop entry. The native bus and count lifecycle have a
  Fedora fixture. Visible dock acceptance remains open.

## Basis and confidence

- Java reference is the exact `v2.26.0` tag at
  `b235d91f20356f8a9b33ad5398550a043cc21ed2`. The earlier audited release
  commit `1ebbe724ee84d55f31295ec3060eecad3653d2f2` differs only in
  `package/size-baselines.json`. The latest spot checks use the tagged tree.
- The November 19 comparison used the worktree based on `68d3093f` and the tagged Java documentation and source. The November 18 audit used uncommitted Rust source on `main`
  at `e3c1760a576f0f358d529c5076537f60d32e7f22`. The previous audit on
  `8fdb5d4` and the earlier PR #4 runs are historical. GitHub records
  successful format, Clippy, test, release-build and staging steps for all
  three runners on the PR #7 merge commit. The later working-tree changes have
  individual verification records. [Merge CI](https://github.com/chardonnay/korTTY_rust/actions/runs/44106367721)
- SithTermFX remains v1.2.7 at
  `8b02de38b7c6396f1af2558b0b00477cfb9a2871`. Retained reference fixtures
  and their recorded runs were inspected. This audit did not repeat an
  exhaustive opcode comparison or regenerate fixtures from a fresh checkout.
- Earlier read-only reviews covered desktop/editor/journals,
  AI/agents, and transport/scheduler/policy. The review compared current
  entry points, persistence, pinned Java behavior and verification records.
  That original audit did not run new builds, native GUI tests or real-provider
  tests. The 2026-11-14 follow-up rechecked provider migration, MLX settings,
  knowledge limits, Java backup conversion, file drag, Scheduler variants,
  package formats and release/security prerequisites. It also checked the
  tagged release notes against the implemented release-specific workflows.
  The latest Java checkout HEAD is newer than v2.26.0; comparisons use
  `git show v2.26.0:<path>`, not that checkout's default working files.
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

The November 17 Apple Silicon ZIP described below was built from
`af1c17601ffa2f32c3aade762b13eca1727c0d6f0c826a631844d7d94baf65ed`
on base `8fdb5d4`. Subsequent Journal, Scheduler, SFTP, desktop and snippet
results cover the individual scopes below and in the parity record.

| Snapshot and platform | Retained result | Limit of the result |
| --- | --- | --- |
| PR #35, `ab5b101e`, native ARM64 | Full workspace suites passed on Windows (4,804 tests), Apple Silicon (4,909) and Fedora (4,953). All three passed formatting and strict workspace Clippy. [Native test record](PORTING-PARITY.md#november-23-full-native-workspace-tests) | Covers the tested source revision. Package verification, remaining interactive desktop checks and independent SSH review stay open. |
| November 23 Knowledge Store restart, Windows ARM64 | Manual review and automatic sync each recorded a missing file. A separate process showed both stored outcomes and retained Last indexed times without rescanning. Automatic recovery changed only the repaired row. [Source outcome evidence](PORTING-PARITY.md#november-23-knowledge-source-outcomes) | Uses a bounded synthetic loopback embedding service. Configured provider, Qdrant and other native desktop acceptance remain open. |
| November 23 compact GGUF Configure and import review, Windows ARM64 | A native development build passed. The serial AI Manager local suite passed 141 tests with six ignored. The desktop comparison checked one-line paths in Java and Rust, scrolling and Save at a narrow width, and each storage choice plus cancellation from the native file picker. [Configure evidence](PORTING-PARITY.md#november-23-gguf-configure-sizing) · [Import evidence](PORTING-PARITY.md#november-23-gguf-import-and-backend-review) | Uses synthetic GGUF registrations. Native split reattachment and the backend offer with an active signed runtime remain open. |
| November 23 follow-up in PR #35, Apple Silicon and Windows ARM64 | Apple Silicon passed focused selections for Knowledge source outcomes (99 passed, six ignored), Agent output (125 passed), Settings and AI Manager search (506 passed, 12 ignored), and GGUF Configure sizing (52 passed). Windows ARM64 passed the all-target check and a native development build for the final sizing change. Native desktop checks exercised AI Manager search and compared the same GGUF path in wide and narrow layouts. [Search evidence](PORTING-PARITY.md#november-23-settings-search-matching) · [Configure evidence](PORTING-PARITY.md#november-23-gguf-configure-sizing) | The selections overlap and cover successive revisions. Full workspace suites and package verification remain release gates. |
| November 23 dialog implementation in PR #34, Windows ARM64 | The recorded native development run passed 571 focused tests, with 12 ignored and 3,611 filtered out. Automatic desktop launches compared GGUF Configure and Plan and execution with the Java x64 release on the same host. [Dialog review](PORTING-PARITY.md#november-23-dialog-parity-review) | Selected dialogs and a focused suite. The release candidate still needs full native suites and package verification. |
| PR #33 implementation `abca21a5`, Windows ARM64 | 4,731 workspace tests passed with 98 ignored and zero failed across 74 binaries. Strict workspace Clippy passed. [PR verification](https://github.com/chardonnay/korTTY_rust/pull/33) | Predates the dialog changes in PR #34. |
| PR #8 head `fb8870149949`, November 18 | Full native suites and release builds passed on Windows ARM64 (4,371 passed, 95 ignored), Apple Silicon (4,474 passed, 208 ignored) and Fedora ARM64 (4,524 passed, 269 ignored). Checksums, architecture and packaged CLI startup passed on all three. The Mac ZIP passed ad hoc signature verification. [PR verification](https://github.com/chardonnay/korTTY_rust/pull/8) | Covers the tested PR head. Subsequent worktree changes require a new candidate. |
| November 19 Windows ARM64 | The [portable release](PORTING-PARITY.md#november-19-windows-arm64-portable-release) and matching full suite cover source fingerprint `64f505367778`. The subsequent [Agent Design desktop run](PORTING-PARITY.md#agent-design-module-preview-and-status-preferences) used a development executable with SHA-256 prefix `3efbe2807fba`. | The later desktop run covers the Agent interactions recorded there. |
| PR #7 merge, `e3c1760` | Windows, Ubuntu and macOS passed their configured tests and release staging. [GitHub Actions](https://github.com/chardonnay/korTTY_rust/actions/runs/44106367721) | Covers the merged revision. |
| Post-merge Journal HTTPS binding (F1) | Four opt-in TLS cases passed on each of Windows ARM64, Apple Silicon and Fedora ARM64, alongside the regular Journal tests at the recorded revisions. [Windows](target/verification/20261117-journal-credential-windows-lld.log) · [Apple Silicon](target/verification/20261117-journal-credential-native-final-macos.log) · [Fedora](target/verification/20261117-journal-language-native-fedora-lld.log) | Configured provider and packaged desktop acceptance remain open. |
| November 18 snippet alternatives | Fedora ARM64 passed 35 focused tests. Windows ARM64 passed app and test-target Clippy. [Fedora](target/verification/20261118-snippet-alternatives-fedora.log) · [Windows](target/verification/20261118-snippet-alternatives-windows.log) | Covers the imported count, candidate handling and reviewed assistant paths. Live provider and GUI acceptance remain open. |
| Pre-merge PR preparation, Windows ARM64, Rust 1.99.1 | `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` passed. | Static checks on that revision. |
| `af1c17601ffa`, Apple Silicon | 612 regular Journal tests passed, with 12 ignored; two opt-in loopback TLS question/summary tests passed separately. The release ZIP passed checksum, ARM64, strict ad hoc app/SSH worker signatures and CLI startup. Separate Seatbelt and packaged backup-startup checks passed. | Targeted application tests; no new full workspace, desktop GUI acceptance, Developer ID signing or notarization. |
| `af1c17601ffa`, Fedora ARM64 | One regular loopback TLS selection test passed, with two ignored; both opt-in TLS fixtures passed separately with TLS 1.3. | Targeted tests, not a full suite or a Fedora release package. |
| `af1c17601ffa`, Windows ARM64 | `cargo check --locked -p kortty --tests` passed. | Compilation only; no execution of the latest Journal tests on Windows. |
| `e75aff4df394`, Apple Silicon | This earlier full workspace run passed 4,332 tests, with zero failures and 198 ignored. | Earlier full-analysis snapshot; later source changes require a new suite. |

Evidence: [PR preparation Clippy](target/verification/20261117-pr-current-833377bc5b-windows-clippy-final.log),
[Mac package source context](target/verification/20261117-native-journal-loopback-tls-macos-af1c17601ffa-source-context.txt),
[source manifest](target/verification/20261117-native-journal-loopback-tls-macos-af1c17601ffa-source.sha256),
[Mac Journal tests](target/verification/20261117-native-journal-loopback-tls-macos-af1c17601ffa-tests.log),
[Mac package checks](target/verification/20261117-native-journal-loopback-tls-macos-af1c17601ffa-package.log),
[Mac isolation and startup checks](target/verification/20261117-native-journal-loopback-tls-macos-af1c17601ffa-scale.log),
[Fedora regular tests](target/verification/20261117-native-journal-loopback-tls-fedora-lld-af1c17601ffa-tests.log),
[Fedora TLS fixtures](target/verification/20261117-native-journal-loopback-tls-fedora-lld-af1c17601ffa-scale.log),
[Windows compilation](target/verification/20261117-native-journal-loopback-tls-windows-af1c17601ffa.log)
and [earlier full workspace run](target/verification/20261117-native-full-analysis-default-macos-e75aff4df394-tests.log).

The November 17 Apple Silicon ZIP measured 41,292,554 bytes and had SHA-256
`988a378e3f02fbc1e94b002ae461356a858510f5a3ea301bb2f4a23e0af0da48`.
Its ad hoc signature predates the credential-binding follow-up. The notarized Developer ID package also belongs to an earlier source revision.

#### Earlier snapshot evidence

The 2026-11-16 working snapshot based on
`bf757dce9fcb766593625960194891c593ace7cd` built natively for Apple Silicon
with Rust 1.100.0. The workspace test binaries reported 4,246 passed,
zero failed and 186 ignored. Cargo used 20 build jobs for release and 17
for tests, based on available memory at each phase. The ad hoc ZIP passed
checksum, architecture and strict signature checks after extraction.
Its SSH worker has the App Sandbox entitlement. The publisher signing and
notarization checks under V7 still apply. Fedora ARM64 used 26 build jobs
for 91 passing archive and filesystem tests on the same source snapshot.
[Source manifest](target/verification/20261116-native-macos-951c74d138ba-source.sha256),
[Mac build](target/verification/20261116-native-macos-951c74d138ba-release.log),
[Mac tests](target/verification/20261116-native-macos-951c74d138ba-tests.log),
[Mac package](target/verification/20261116-native-macos-951c74d138ba-package.log)
and [Fedora tests](target/verification/20261116-native-current-fedora-tests.log).

The test follow-up uses a private receiver directory for the reviewed archive
copy, representable timestamps for Chain admission, and an archive integration
test name that runs on Windows without an elevation request. Apple Silicon
again passed all 4,246 workspace tests with 186 ignored. The opt-in native
Seatbelt test also passed. Fedora passed the six named archive reuse fixtures.
[Updated source manifest](target/verification/20261116-native-macos-55867a98d73d-source.sha256),
[Mac rerun](target/verification/20261116-native-macos-55867a98d73d-tests.log),
[Seatbelt](target/verification/20261116-native-macos-55867a98d73d-seatbelt.log)
and [Fedora archive reuse](target/verification/20261116-native-current-fedora-archive-reuse.log).

The final Apple Silicon rerun reproduced the workspace result at 20 Cargo
jobs and eight test threads. The `--zsh-tests` wrapper now gives tests a
private fallback profile. Subprocess fixtures with their own home directories
continue to exercise the native profile path. The rerun observed
window-settings output in the private profile and verified the default
window-settings file's identity and metadata before and after the suite.
[Isolated test suite](target/verification/20261116-native-macos-87e84b8bc81f-tests.log)
and [profile verification](target/verification/20261116-native-macos-87e84b8bc81f-profile.log).

Windows ARM64 passed 654 component tests in the workspace run and 3,493
application tests in a separate rerun. These results include 85 ignored
tests. The native build used 24 Cargo jobs and the Rust toolchain's LLVM
LLD. The application
rerun used eight test threads. It followed one intermittent LM Studio
scheduling failure and twelve passing isolated repeats. The reconnect
fixture now waits for a CMD prompt on the actual anchor row. The updated
reconnect test passed twelve consecutive runs and the full application
harness.
[Windows build and components](target/verification/20261116-native-current-windows-final.log),
[isolated LM Studio tests](target/verification/20261116-native-current-windows-repeat-tests.log)
and [application suite](target/verification/20261116-native-current-windows-app-verified.log).

PR #4 at `590acff495710843614c60028e347361beb0734e` passed formatting,
strict workspace Clippy, workspace tests, release builds and package staging
on `ubuntu-24.04`, `windows-2025` and `macos-15`. The PR workflow passed
3,563 application tests on Ubuntu, 3,458 on Windows and 3,570 on macOS.
The fixes cover Scheduler file download adoption, explicit Windows ownership
for private rsync staging, archive mode conversion and loss of plain journal
output under scheduling delays. The final macOS push job completed in nearly
64 minutes with the 90-minute limit.
[PR workflow](https://github.com/chardonnay/korTTY_rust/actions/runs/43733461845)
and [Push workflow](https://github.com/chardonnay/korTTY_rust/actions/runs/43733458752).

On merged main `bf757dce9fcb766593625960194891c593ace7cd`, Fedora ARM64 passed
both opt-in existing-local-file rsync fixtures against pinned OpenSSH. The
protected fixture used real passwordless sudo in an isolated namespace with an
exact helper rule. Both verified two Calendar firings, metadata and receiver
identity, recovery and rejection of stale approval. The protected fixture also
verified managed denial and handoff cleanup. Windows ARM64 passed the opt-in
pinned OpenSSH file replacement against Fedora, preserving the ACL and
timestamps, retaining the original for recovery and rejecting reuse of the approval. Five
Windows local publication and migration regressions also passed. Verify the
remaining native target cases and editing, approval and reapproval in native
Scheduler windows. Complete package acceptance under V7, including clean-system
installation and signing.
[Fedora OpenSSH record](target/verification/20261115-pr4-rsync-fedora-native.log)
and [sudo record](target/verification/20261115-pr4-rsync-fedora-sudo.log).
[Windows OpenSSH record](target/verification/20261115-pr4-merged-rsync-windows-pinned.log)
and [Windows local regression record](target/verification/20261115-pr4-merged-rsync-windows-local.log).

An earlier application-suite checkpoint on 2026-11-15 passed
3,556 tests on Fedora ARM64 and 3,450 on Windows ARM64. The first Windows
run had a report-cancellation cleanup failure, followed by 30 isolated passes
and a successful full retry. Strict Clippy and an Apple Silicon cross-check
passed for that recorded source, which preceded the direct local file receiver.
See the [recurring sudo rsync record](target/verification/20261115-recurring-sudo-rsync-acceptance.md).

After resuming implementation on 2026-11-14, the Windows ARM64 application
suite passed **3,328 tests with zero failures and 62 ignored**. Updated
LM Studio import, reviewed MCP, managed MLX controls, knowledge limits,
ordinary symbolic SFTP permissions and additive Java OpenPGP references are
recorded in [the checkpoint](PORTING-PARITY.md#lm-studio-migration-and-https-mcp-mlx-reasoning-and-knowledge-limits).
The isolated MCP TLS fixture and application-test cross-compilation check
for Apple Silicon passed. Installed providers, physical Mac inference and
the other native release gates remain open.

The preceding recorded Windows ARM64 application run on 2026-11-14 passed
**3,320 tests with zero failures and 61 ignored**. Two isolated loopback TLS
tests passed separately. They cover both LM Studio chat protocols, inventory,
certificate/hostname rejection, and Scheduler/Swarm/journal model bindings.
Application Clippy, formatting and the Apple Silicon application-test
cross-compilation check passed. These are application counts, not a new full
workspace or native Mac run. Installed LM Studio desktop acceptance remains
open. See the [LM Studio loopback checkpoint](PORTING-PARITY.md#lm-studio-auto-over-localhost-and-loopback-https)
and its [verification record](target/verification/20261114-lm-studio-loopback-windows-arm64.txt).

The checks below describe **earlier source snapshots**. The November 13
Windows ARM64 size-optimized package measured 81,262,080 bytes (77.50 MiB)
for `kortty.exe`; its source predates the current AI and migration changes.
The Apple Silicon signature, notarization, Gatekeeper, Seatbelt and packaged
SSH evidence also applies only to its recorded package. Rebuild the current
working tree and repeat the affected V7 checks before distributing it.
See the [Windows package record](target/verification/20261113-windows-arm64-release-size-optimized.md)
and [Mac notarization record](target/verification/20261112-macos-notary.md).

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

The November 12 Windows ARM64 static-CRT ZIP passed a private-profile test
covering Control ping, one restored PowerShell pane, graceful exit and
cleanup. That ZIP and its checksum are retained in the original main
checkout under `target/dist/kortty-0.1.0-aarch64-pc-windows-msvc-20261112-shell-agent/`.
The [shell and Agent checkpoint](PORTING-PARITY.md#local-shell-integration-and-terminal-agent-recognition)
records the retention exception for the earlier archive in that checkout.
The porting worktree holds the November 19 package. On November 22 its
SHA-256 matched both its sidecar and the November 19 record.
[November 19 verification](PORTING-PARITY.md#november-19-windows-arm64-portable-release)
· [Retained ARM64 ZIP](target/dist/kortty-0.1.0-aarch64-pc-windows-msvc/kortty-0.1.0-aarch64-pc-windows-msvc.zip)
· [SHA-256 sidecar](target/dist/kortty-0.1.0-aarch64-pc-windows-msvc/kortty-0.1.0-aarch64-pc-windows-msvc.zip.sha256).
The current source checkpoint awaits packaging under V7. The x64 ZIP still
needs a current build and verification. The exact executable and
configuration used in the user's originally reported failing session have
not been identified.

## Scope and recorded decisions

- Native Windows, macOS, Wayland and X11 remain required. The highest product
  priority remains usable Java-equivalent windows, dialogs, menus and tool
  windows. Every implementation step includes its corresponding UI.
  Dialog changes must meet the [Java design criteria](#gui-dialog-design-parity).
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

Exercise the native Scheduler review and replacement flow for
[temporary SSH keys](PORTING-PARITY.md#scheduled-ssh-and-rsync-with-temporary-keys).
Continue the searchable-control inventory for Settings and AI Manager.
Exercise the separate native desktop workflows for
[Java backup review](PORTING-PARITY.md#november-23-java-temporary-ssh-key-restore),
[project reconnect](PORTING-PARITY.md#november-23-project-reconnect-with-temporary-ssh-keys)
and [vault auto-unlock](PORTING-PARITY.md#november-23-native-vault-startup-unlock).
Use disposable keys for the [saved-key editor](PORTING-PARITY.md#november-23-native-saved-temporary-ssh-keys)
and Quick Connect checks.

Continue the [native session isolation checks](PORTING-PARITY.md#november-23-windows-isolation-dialogs-and-launches)
with the macOS and Linux Security page and folder, SSH and local-shell
editors. Launch a new saved local profile in the desktop sandbox and
check the editor feedback for an unavailable backend.
Windows ARM64 recovery already exercised folder inheritance and the
managed minimum in running local sessions.
Use the [saved and Teamwork reconnect checkpoint](PORTING-PARITY.md#november-23-saved-and-teamwork-ssh-reconnect)
for the pinned live OpenSSH coverage on Apple Silicon. Exercise the visible
SSH and Teamwork reconnect flow on the native desktops and add a live
jump-host test.
Continue the [dialog design comparison](#gui-dialog-design-parity) with
the remaining Settings and AI Manager pages. Repeat the verified Windows
Knowledge Store restart and recovery paths on the other native desktops.
Compare the remaining searchable controls and complete the native checks
for both settings searches, Agent step copying, Agent naming and GGUF,
and live terminal acceptance for Plan and execution. Continue from the
Windows Font, Window and SSH editor comparisons when reviewing the remaining
Settings and connection pages. Scheduler, SFTP and the other dialogs still
need their own Java comparisons.
Continue the unresolved portable restore mappings and the requested
shared Teamwork Agent/Swarm extension. Run native acceptance for the
existing connection-folder gestures, Scheduler picker, SFTP partial-file
cleanup, comparisons, transfer recovery and snippet workflows.
The six work packages below cover compatibility decisions, platform
acceptance and release preparation. The independent SSH review under V6
can proceed alongside implementation.

**F1 — Implemented and verified Journal HTTPS credential binding.**
Questions and summaries now pass the captured credential into the HTTPS
completion attempt. Summary connection checks and retries retain the same
snapshot, and the transport applies it to LM Studio model lookups before
completion where applicable. The summary TLS fixture checks stale credentials,
revocation, cancellation and a retry after the provider rejects the requested
structured format. Windows ARM64 passed 606 Journal tests, 53 LM Studio tests
and four private TLS cases before the startup cache follow-up. The cache now
keys HTTPS results on the same credential used by the reviewed request. The
summary fixture also exercises removal of a previously approved credential.
Windows ARM64 passed 606 Journal tests (10 ignored) and all four TLS cases
after the cache follow-up. Apple Silicon passed 612 Journal tests (12 ignored)
and all four TLS cases, including the removal and retry checks, on source manifest
`bf19c116594c8be69026e5d63ccee8295847e4c37f81c7326cb285e0063cbad7`.
See the [Windows test record](target/verification/20261117-journal-credential-windows-lld.log)
and the [macOS test record](target/verification/20261117-journal-credential-native-final-macos.log).
Fedora ARM64 passed 614 Journal tests (12 ignored), 206 Scheduler tests, the
diagram import fixture and all four TLS cases on the later source manifest
`94e140faf0376a978b7ccfb46f22ba8626b115a39d2e71a920f1ca84ddfbdc23`.
[Fedora record](target/verification/20261117-journal-language-native-fedora-lld.log).
The implementation and focused native regressions for F1 are complete.
Configured-provider desktop acceptance remains under V5 and current release
packages under V7.

The existing-local-file rsync runtime fixtures passed on Fedora and Windows.
Recurring direct file downloads can start at an initially vacant name on one
or several saved SSH routes. Group reviews bind a distinct local file path and
initial receiver state per route. The mixed-source mode can create a reviewed
vacant local tree, including for a single Java directory source with named or
contents placement. Direct directory downloads support an explicit option for
creation at reviewed vacant paths. Checked direct and mixed directory
downloads can create missing local parents, including common parents of
disjoint reviewed receivers in a group. Java download drafts prefill the
original per-route folder layout on compatible local paths. Direct file
downloads can also create missing local parents. Ordinary and passwordless
uploads can start at a reviewed vacant file, named directory, directory
contents or combined receiver in a checked Linux parent. Later firings
replace the exact receiver confirmed by publication. Fedora passed Calendar
and Chain fixtures for these transitions. Protected single-file replacement
and one-use folder merges are also implemented and have native OpenSSH/sudo
evidence. Recurring protected folder creation and merges are now implemented.
Continue with their live helper and native Scheduler acceptance, the
remaining destination contracts and native Scheduler editor/deployment
acceptance. See the
[missing-parent checkpoint](PORTING-PARITY.md#scheduler-rsync-downloads-with-missing-local-parents-and-java-group-paths)
and the [shared directory](PORTING-PARITY.md#scheduler-rsync-groups-sharing-missing-local-parents)
and [direct file](PORTING-PARITY.md#scheduler-direct-rsync-files-with-missing-local-parents)
checkpoints.

1. **Close AI provider compatibility and acceptance, B1/B4/B8.** Verify the implemented
   Manual and omitted-mode LM Studio migration and reviewed loopback HTTPS
   MCP. Preserve the passed Apple Silicon signed-MLX reasoning checks and
   complete the remaining native Windows GGUF/runtime cases. Cover AI Manager
   and affected Agent, Swarm, Scheduler and journal workflows. Resolve the
   remaining noncanonical endpoint routes and automatic web-tool scope cases.
   Reviewed exact OpenAI-compatible HTTPS completion URLs preserve prefixed
   paths, case and a trailing slash in Manual and DEFAULT modes. Verify native
   AI Manager adoption and a configured gateway with its deployment CA and
   credential. Automatic discovery uses the existing canonical route.
   Verify server-selected profiles with a compatible live provider and Toolpack
   adoption with an installed LM Studio.
   Verify the implemented loopback and exact HTTPS Anthropic Messages flows in native UI and
   with a configured HTTPS provider. Native HTTP JSON and SSE passed with
   the signed llama.cpp test runtime. Java selects its Anthropic transport
   before resolving the model mode. The restored profile therefore retains
   the explicit model and exact reviewed path. The native Messages runtime
   has bounded extended-thinking budgets, adaptive effort, and guarded
   compatibility retries before response generation. One configured deadline
   governs the initial request and its retries. Verify the behavior against a
   configured gateway and model that reject the original mode or output limit.
2. **Complete data migration and knowledge acceptance, M1/B6.** Convert the
   remaining non-secret Java stores with review and restart replay. Check
   the outstanding options and target bindings for the 15 recognized Java
   Scheduler action kinds. Imported jobs stay disabled pending review. Keep external
   model files, credentials and execution authority separate. The local
   passage ceiling now matches Java at 5,000,000 and the snapshot byte limit
   is 64 GiB. Keep the disk-backed implementation and complete real-corpus
   memory, disk, CPU and cancellation measurements at higher tiers. Assess
   the separate Qdrant limits explicitly. Source file-size settings are implemented.
   Java allows a reviewed 1 GiB PDF setting with a 500 MiB aggregate input
   scan limit. Rust matches those size settings and streams reviewed large
   PDFs to a private mapping after the worker permit. Native 500 MiB worker
   checks exist; complete representative parser-memory measurements and
   larger snapshots with realistic embedding dimensions near the byte limit.
   The portable GGUF and MLX registry hints from `llm/models.xml` and
   `llm/mlx-models.json` now retain runtime type and exact Java model ID.
   Verify the native settings review and use explicit native model adoption to
   resolve future knowledge-store references.
   Verify native Skill Statistics interaction and cross-host restore of Java
   histories. Built-in usage and Java size measurements use separate history.
   Unmapped ledger entries retain their exact Java IDs in read-only reports.
   Verify sparse window preferences, automatic Java favorites for newly
   imported profiles with unambiguous source IDs, explicit favorite review for
   exact existing profiles, and exact colors for newly imported groups.
   Review the remaining global settings and conflicts with existing profiles.
   Verify the implemented
   snippet/folder analysis history restore and review on native desktops.
   Verify the implemented native inspection paths for retained run snapshots
   and preserved historical diagrams. Identify any unsupported diagram
   syntax with a pinned example before adding a new implementation task.
   Apply options, checklist stages, checkpoint
   statistics, retries and failure codes restore as read-only run evidence.
   Verify the native file editor and
   snippet profile routing with configured providers and connection overrides.
3. **Complete file and Scheduler variants, T2/T3/T4/Q1/Q2.** Verify reviewed
   folder-wide SFTP part cleanup on native desktops. Verify searchable multi-selection of
   Scheduler targets in the native job editor. Verify reviewed
   rsync creation and replacement on the remaining native desktop routes.
   Verify the existing shared routes for shell and file actions under deployed
   OpenSSH and sudo. Extend shared-route review and execution-time checks to
   scheduled Agent/Swarm. Verify the three draft comparison pairs in the
   native SFTP Manager, along with reviewed replacement of another existing
   server file using the Linux, macOS or Windows helper. Verify the macOS
   and Windows helpers on native OpenSSH routes.
   Recurring privileged single-file transfers and protected folder transfers to
   existing or initially vacant receivers are implemented, including bounded
   recurring Calendar and Chain approval. First publication binds the confirmed
   created receiver; later runs merge only into that identity. Verify this
   progression, failure recovery and reapproval through deployed native routes.
   The receiver-witness protocol and local merge completion now have backend
   support: optional legacy-compatible receipt requests, held receiver identity
   and metadata, and pre-transfer action/payload accounting. Existing-receiver
   recurrence now connects per-firing admission, durable pending/completed state,
   reapproval/recovery, schedule/sudo-mode binding and the review UI. The earlier
   macOS/Fedora checkpoint passed two focused state tests, the eight-combination
   editor fixture, all 208 Scheduler tests and 30 privileged tests per platform;
   explicit helper fixtures remained ignored. Windows ARM64 Clippy passed.
   Vacant receiver progression is now connected in both directions and both
   sudo modes. The first creation charges the receiver root as an action even
   for an empty source. The creation checkpoint passed the expanded sixteen-case
   editor fixture and focused state/publication tests on macOS, Fedora and
   Windows; Scheduler counts were 209, 209 and 210 respectively. Explicit native
   SSH/helper fixtures remain excluded from those counts. Fedora used LLD after
   a GNU AArch64 test-link failure. See the
   [creation verification entry](PORTING-PARITY.md#recurring-protected-folder-creation-and-confirmed-receiver-progression).
   Download review and limits include the entire streamed manifest, including
   unchanged files. Earlier native protocol, local-merge and Linux server
   receiver-anchor tests also passed at their recorded snapshots. Live
   helper, recurring-job and native UI acceptance remain open. See the protected-
   folder receiver entry in PORTING-PARITY.md for exact scope and records.
   Cover reference-required destinations outside the supported ownership
   and metadata scope.
   Fedora passed pinned OpenSSH ordinary SFTP Chain starts in both directions
   and the calendar regressions. Verify approval, reopen and reapproval
   in native Scheduler windows.
   Existing-tree uploads enroll a reviewed receiver fingerprint for each pinned
   route and recheck authority before quarantine and publication. Existing-tree
   downloads apply the same review and recovery model to a protected local
   receiver. Mixed routes retain ordered file and directory sources, named
   and contents layouts, and the projected tree inventory. The review shows
   the placement and overlap decisions. Mirror scope derives from the exact
   synchronized directories. Each route publishes one verified tree into a
   vacant or reviewed existing folder. Existing mixed uploads use the Linux
   v2 helper. Java passes the source list to rsync in one command per server
   (`RsyncCommandBuilder.java`). Verify grouped runs on the remaining native
   desktop platforms.
   Recurring existing-directory replacement saves a device-local approval and
   a durable per-route receipt for the published root. Each calendar or Chain
   firing takes fresh source evidence within its approved ordered paths,
   placement and deletion scope. The aggregate limits apply to transferred
   bytes, entries and removed paths. The v3 Linux helper and the protected
   local receiver return exact post-move snapshots. An armed attempt without
   a confirmed result requires receiver and recovery review before another
   firing. The nominal calendar time or admitted predecessor round is
   captured at dispatch and consumed independently of retained run history.
   Native editor acceptance remains open.
   Java rsync elevation uses passwordless `sudo -n`. Rust has an explicit
   passwordless mode for a single file, a directory or combined sources per
   route. The fixed Linux helper checks exact caller IDs and protected source,
   create and replacement roots. Ordinary rsync transfers verified bytes through a
   dedicated handoff. Remote directory replacement uses guarded quarantine
   and publication with recovery. Isolated native Fedora acceptance covers
   the installed helper, real sudo and pinned OpenSSH for the six file and
   directory cases. For combined sources the helper reviews one effective
   upload receiver or streams the ordered protected source inventories for
   a download. Execution verifies every source and publishes one combined
   tree. Isolated Fedora acceptance covered both directions with vacant,
   merge and scoped mirror receivers, owner-private new local downloads,
   recovery and denial of stale or unauthorized sources. Direct protected
   directory downloads also use owner-private publication at a new local
   path. Protected regular-file uploads can replace one reviewed server
   file. The approval binds the actual file identity, content and supported
   metadata. The fixed helper guards quarantine, publishes without replacing
   a late arrival and retains the original. Fedora verified exact file
   replacement and stale-file refusal with OpenSSH and real sudo. Recurring
   passwordless uploads support reviewed existing files and directories.
   Direct ordinary and passwordless downloads can replace one reviewed
   local file on each route, with explicit distinct paths for grouped jobs.
   The local publisher verifies the captured receiver and staged contents,
   retains the original for recovery and preserves supported receiver metadata.
   A recurring direct file download counts one entry and its source bytes.
   The receiver path remains present, so its replacement consumes zero
   removals. Only a confirmed publication records the exact successor for the
   next firing. Recurring passwordless downloads also support reviewed
   existing local directories. Combined transfers use the same scoped
   placement and deletion rules. The protected download preview bounds fresh per-firing
   source inventories under the saved policy and exact paths before creating
   a data handoff. A guarded upload supplies the held successor receipt.
   Fedora verified successive protected firings over both Calendar and
   Chain with real sudo and pinned OpenSSH. Verify configured
   deployments and native editor use. The local receiver regressions passed in
   PR #4 CI, and the opt-in pinned SSH cases passed on Fedora and Windows.
   Direct file downloads can start at reviewed vacant local file paths on saved
   single or grouped routes.
   Verify first publication and subsequent replacement with the installed
   helper on configured saved routes, including grouped targets.
   The Linux helper reclaims marked upload and download handoffs during a
   later preparation for the same configured account. Verify forced
   termination and concurrent transfer protection natively on Fedora.
   Verify symbolic modes through the reviewed sudo route. Exercise archive
   reuse and optional local-copy replacement across successive schedules on
   deployed saved routes. The native helper and pinned client fixtures cover
   the existing ZIP-update and TAR-recreate paths.
   Verify the named ownership dialog end to end on native desktops. The
   Fedora OpenSSH fixture verifies the pinned command channel.
   Verify the implemented ordinary server-archive local copy, optional
   password ZIP and their digest and permission checks on native desktops.
   Ordinary password ZIP uses the vault reference and sends the secret after
   the helper's acknowledgement, followed by a guarded publication marker.
   Verify password ZIP with both reviewed sudo modes on deployed saved routes.
   The vault path uses the installed V2 helper and a command-specific sudo
   rule. Fedora verified the pinned client fixture and separately exercised
   real sudo and PAM in a disposable namespace.
   Verify macOS Finder drag-out independently of the replacement work.
4. **Close the bounded scope audits.** Record explicit decisions for remaining
   vault/key persistence, approval/automatic-web behavior, Swarm capacity,
   remaining language coverage in the existing completion lists, other
   reduced-editor differences and optional telemetry/plugins. Verify the
   existing opt-in inline AI suggestions with a configured provider. Derive
   any new implementation from a named pinned Java action. Do not infer a missing
   terminal opcode, SSH algorithm or editor language from a historical summary.
5. **Complete native workflow acceptance, D/A/B/S/J/T/P/R and V1–V5.** Cover
   configured providers, signed local runtimes, shell opt-ins, window behavior,
   notification activation, current policy enforcement, restored data, SFTP
   and real authorized sudo. Obtain the missing Windows x64/macOS Intel
   evidence and finish native Wayland/X11 cases. Hardware stays deferred.
6. **Complete distribution and release gates, U1/U4/V6/V7.** Resolve active
   security findings and obtain independent SSH review. Verify Flatpak local
   isolation on x86-64 and build current fixed-snapshot packages in the
   implemented formats. Repeat native tests and
   signing/notarization/Gatekeeper where applicable, check clean-system
   install/upgrade/removal, and measure comparative performance. Publish a
   stable release and signed model/runtime feeds only through their explicit
   release process. Their availability is an external prerequisite.

## Desktop, connections and terminal behavior

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| D1 | Acceptance. Saved local-shell program/arguments/directory and launch routes preserve the integration opt-in. Supported custom Bash/Zsh/Fish startup flags and qualifying Windows executable paths use private startup wrappers. PowerShell/CMD presets are supported. Java `shellIntegrationAutoInject` is imported/exported and the legacy native flag remains readable. Supported default-shell Java project routes preserve the opt-in and validate the saved catalogue identity. Fedora ARM64 verified Flatpak Bash, Zsh and Fish host startup and required Landlock sessions inside the runtime. R `src/connection_store/local.rs`, `src/connection_window/local.rs`, `src/profile/projects/java/routes.rs`, `crates/pty/src/flatpak.rs`. | Verify native launch/edit/restart, live Java exchange, PowerShell 7 and Zsh/Fish startup on the remaining native platforms. J `core/LocalShellTtyConnector.java`, `shellintegration/ShellIntegrationInjection.java`. Preserve exact-argument native extensions. The Java portable project format covers the subset without machine-local command, directory or Teamwork identities. |
| D2 | Implementation, acceptance and bounded audit. Saved SSH, local shell and serial settings support inherited paste line delay and strict terminal overrides, including Java connection XML. HP2392 paste uses its existing line handling. Themes, independent appearance/scrollback/PTY dimensions, keepalive, project split presentation, local image backgrounds and remote hosted tool restoration also exist. J `model/ConnectionSettings.java`, `ui/ConnectionEditDialog.java`. R `src/connection_store/terminal.rs`, `src/connection_window/terminal_overrides.rs`, `src/workspace/paste_protection.rs`. | Verify tree/sidebar drag-to-folder and successive Undo shared by the list and map. [Folder move parity](PORTING-PARITY.md#november-20-connection-manager-folder-moves) Verify native editing, launch, reconnect, paste routing, Java exchange, and the theme/project/sizing/authentication workflows. The user-deferred HP and physical serial acceptance remains in V4. Verify standalone saved SSH window geometry, private Java XML import, and portable omission. |
| D3 | Acceptance + bounded audit. Named highlight sets, per-connection selection, bounded network matchers, rule editor/preview, interchange, notification and reviewed snippet triggers exist. R `src/terminal_highlight.rs`, `src/workspace/highlight.rs`. | Native pinned-set acceptance under changing output, palette, policy and snippet authority. Java XML and backup rules use a bounded Java syntax translator shared with Quick Select and Journal markers. A bounded matcher supports lookarounds, backreferences, atomic groups and possessive quantifiers. Audit remaining Java syntax and test visible behavior on native desktops. Do not reimplement the existing set picker or master controls. |
| D4 | Implementation + acceptance. Native lifecycle, placement, menu routing, hosted/detached tools and Settings/AI Manager keyboard guards exist. Selected Windows ARM64 default-selection, dirty-close and tab-label behavior is verified. Quick Select opens files in native Windows PowerShell/CMD panes. macOS has a separate status item for New Window and Quit and App Nap activity scopes for recent terminal I/O, running jobs and active AI requests. | Align both dialogs with the tagged search matcher and inventory the remaining reachable labeled controls. [Search comparison](PORTING-PARITY.md#november-22-design-editing-command-output-and-settings-search) Verify saved, manual and logger-discovered log folders on native desktops. Verify the status item with zero windows and observe App Nap activity on a packaged Mac build. Complete V1/V2 across native desktops. Cover fresh-profile size, footer reachability, preferred geometry, toolbar overflow, keyboard traversal, owner modality, dirty close and search focus in both hosted and detached forms. |
| D5 | Acceptance. General BEL/OSC, shell, highlighting, Chat, Agent/Swarm and Scheduler notifications exist. Windows OSC 777 click routing to the original inactive split was measured. The Linux LauncherEntry backend emits on a persistent D-Bus name and the portable archive stages a matching desktop entry and icon. | Verify the Linux counter on a supporting launcher and complete native sudo/password waits, moved/closed/stale targets, suppression/coalescing and revocation. Complete macOS and Linux delivery/activation under V3. |

Quick Connect and the saved local-shell editor offer detected Windows
PowerShell, Command Prompt, Git Bash, Cygwin and WSL presets, macOS Zsh and
Bash, and Linux Bash. Git Bash also has a Windows toolbar action with exact
Java backup mapping. Native interchange retains the selected preset and
automatic integration option where supported. Complete live Cygwin and WSL
acceptance, native desktop interaction and the remaining Flatpak launch cases.
See the [local shell preset checkpoint](PORTING-PARITY.md#native-local-shell-presets).

Quick Connect also accepts a custom executable, individual arguments and a
starting directory. Selecting a saved shell loads its launch settings for
one-use edits. Saving as a new connection creates a separate profile.
Verify the controls and reopening on native desktops. See the
[Quick Connect local shell checkpoint](PORTING-PARITY.md#quick-connect-local-shell-commands-and-directories).

Evidence for the completed D2/D3 work includes
[terminal dimensions](PORTING-PARITY.md#initial-terminal-size-for-local-shells-and-ssh),
[Java project dimensions](PORTING-PARITY.md#java-project-initial-terminal-dimensions),
[theme exchange](PORTING-PARITY.md#java-theme-and-project-appearance-exchange) and
[named highlighting](PORTING-PARITY.md#reviewed-changed-file-batches-and-named-highlighting-selection).

## Built-in agents and external coding-agent control

Swarm comparisons in Rust use the English `Errors` column. The tagged
Java validator requires the literal `Fehler` in the final column, even for
an English answer. Keep this difference visible when comparing transcripts.
[Java aggregator](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/core/swarm/SwarmAggregator.java) · [Rust comparison](src/terminal_agent/swarm/comparison.rs)

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| A1 | Implementation + acceptance. Built-in Run/Ask/Plan, reviewed local/SSH execution, skills/quota, bounded history, opt-in `agent` aliases, Tab completion, sudo/vault challenges and mirror holds exist. PowerShell/CMD integration and per-pane setup/status now ship in source. Native ConPTY PowerShell 5.1/CMD and Fedora SSH Agent routes passed. Permitted terminal AI initializes an in-memory SSH credential mask independently of capture, including incognito, without enabling logging. R `src/workspace/terminal_agent/shortcut.rs`, `src/workspace/ssh_access.rs`, `src/session/shell_activity.rs`, `crates/pty/src/shell_integration.rs`. | The inline root-bypass option and grant revocation passed targeted native macOS/Fedora tests; automatic read-only dispatch and inline per-run confirmation passed targeted native macOS/Fedora tests. Verify remaining supported shells, native desktops and current packages. The sandboxed SSH manual password flow passed on Fedora Wayland and X11. Verify the manual flow and local system sudo on macOS, and exercise history and run controls, mirror release, changed targets, and revocation. Direct custom Bash, Zsh, Fish, PowerShell and CMD profiles can use the Agent with validated interactive startup flags and a reviewed native runner. Verify the custom profiles in live desktop panes. See the diagnostic contract below. |
| A2 | Implementation, acceptance and bounded audit. Planning stages, design module IDs d01–d20, docking/status/approval placement, inline history, picture-in-picture, usage/repair display, seven activity export formats and Agent-to-Chat/Journal links exist. The worktree includes the in-pane Spotlight prompt with entry and submission transitions, compact step progress, animated toast entry, countdown, fade and collapse, Orbit and the Adaptive Dock countdown. A chooser switches between retained runs in the same pane from status, history and the terminal context menu. Up to five finished runs remain available after the Activity window closes, bound to the original non-private stream. The standalone editor creates a custom copy on module activation and writes committed edits and display preferences immediately. General Settings applies its draft on Save. Java restore fills both unset preferences independently. Windows ARM64 verified the earlier Settings workflow, live d14 display and cancellation, d19 layering and detach/reattach, and d20 pane resizing. [Spotlight and status checkpoint](PORTING-PARITY.md#november-20-terminal-agent-spotlight-and-status) | Verify the d06 layout and bound approvals in a live desktop run. Align the design naming flows and copy all retained masked output for the selected step. [Completion checks](#confirmed-implementation-work) Verify reopening finished runs and earlier-run selection and the contextual run and step actions. Verify Spotlight focus and profile selection, the timed displays with reduced motion and auto-hide disabled, the remaining d14 interactions, and live d19 run following. Verify inline selection and Kitty row mapping, exports, linked Journal cards, live provider usage/repair and native desktop behavior across platforms. |
| A3 | Acceptance + bounded audit. Live and saved targets, named pre-run saved SSH drafts, completed interactive rounds with renaming, reviewed continuation, connection groups and working-directory metadata, history search, Markdown/text/PDF export, saved journal defaults, estimate and journal links are implemented. A batch review binds one approval to the displayed ordinary commands and target identities. R `src/workspace/terminal_agent/swarm.rs` and its `batch.rs`/`comparison.rs`/`conversation.rs`. | Verify native renaming, selection, batch review and revocation, comparison, failure isolation, workflow drafts and continuation over pinned SSH. Evaluate Java's standing batch policy and incognito target scope against native authority requirements. Saved follow-up uses reviewed saved SSH routes. |
| A4 | Acceptance. All eight pinned Agent verbs, events, wait/start/rename and `notification.show` have native Control/CLI routes. Windows socket backpressure and native notification submission have evidence. | Native visible delivery and exact focus/target routing, stale agents, moved splits, timeouts and losses across desktops. Admission/native submission receipts alone do not prove a displayed notice or activation. |
| A5 | Acceptance. Explicit `control-api = "allow"` starts the listener and locks the setting while retaining the personal preference. The parser normalizes feature keys and values. MCP stdio keeps its independent opt-in, retains its write tools preference while disabled, and gates writes on effective policy and consent. Masked reads, reconnect identity guards and managed MCP denial exist. | Verify managed activation, lock display and relaxation on native desktops. Complete affirmative write consent and invalidation across those desktops. P1 owns shared policy acceptance. |

A detached d19 history window retains its identity and folded state
while following runs of the same terminal stream. Closing the Activity
of a reopened run or continuing an Ask in Chat hands the window to the
next eligible run. A stream change invalidates that handoff. The PiP
title menu uses the shared run actions in the folded and unfolded panel.
The folded detached window temporarily makes room for the popup and
restores its height on dismissal or selection. Fedora ARM64 passed a
native X11 Openbox run and a native Wayland Mutter run with two reviewed
Ask requests, copied responses, an archived run reopened and closed, and
reattachment. The Wayland test verified that Iced applies an inner size
returned synchronously by Winit and renders a buffer of the new size.
Repeat the workflow on Windows and macOS and verify placement across
monitors.
[Java pane presenter](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AgentDesignHost.java#L1556)
· [Fedora native desktop acceptance](PORTING-PARITY.md#november-21-detached-terminal-agent-history-follows-its-pane)

The status pill (including Spotlight and Adaptive Dock's pill), Orbit,
Minimap rail, and the toast badge open the displayed run's actions on
right-click. The same menu selects earlier runs on the pane's stream.
An open menu keeps its status visible through the auto-hide deadline and
closes when the pane grid changes. On the Minimap, a step's menu takes
precedence over the rail menu. Fedora X11 and Wayland copied answers
from all four status menus, selected an earlier run, opened its Chat
from the pane footer, and verified the disabled Chat entry. Both Linux
desktops also copied distinct answers from two live splits and reopened
the Adaptive Dock menu after a split narrowed its pane. The X11 run
verified that the left pane's footer opened a Chat with its own answer.
Windows ARM64 confirmed **Copy answer** from the Spotlight pill and
**Continue in AI Chat** from the pane footer in a native private session.
Repeat the other status presentations, earlier-run selection, split
and resize checks on Windows. The live macOS workflow remains open.
[Status-menu parity and tests](PORTING-PARITY.md#november-21-run-menus-on-agent-status-displays)

The d01 Powerline and Adaptive Dock's bottom mode show the current typed
step, command count, elapsed time and available model usage in a
24-pixel bar. A step drawer opens above the bar over the terminal rows.
It displays the latest eight typed entries and counts the earlier
entries. A pending review opens it automatically. Resolving the review
closes an automatically opened drawer. A manually opened drawer stays
available until closed or hidden. The Settings preview uses the same
presenter and respects the reduced-motion setting. Fedora ARM64 X11
verified automatic opening, Review focus, exact step output, stable pane
geometry, and hiding and restoring the bar. Shared step rows show the
typed state marker, command exit code and retained output line count,
live elapsed time, or the model's reported token count. Fedora X11
verified the marker and result labels in Dark and IntelliJ Light, the
latter with reduced animations. Fedora X11 verified F8 and
Ctrl+Shift+F7 before and after copying a step's output. The earlier
fixture sent an unintended Alt modifier with F8. The native check
now sends the level-zero X11 keycodes and holds only the modifiers
specified by the binding. The clean X11 build verified the shortcut
in standalone Powerline and Adaptive Dock's bottom mode. Adaptive
Dock switched from Powerline at 1230 pixels to the side timeline at
1400, stayed on the side at 1230 on the way back, and returned to
Powerline at 1160. Complete live Wayland, Windows and macOS checks and
the remaining narrow Adaptive Dock checks on the current build.
[Powerline step drawer](PORTING-PARITY.md#powerline-status-and-step-drawer)

The d12 Top Bar shows the current step, command count, elapsed time and
available model usage. Its track sweeps while an active run is unpaused
and uses warning, success and error colors for the corresponding states.
The drawer opens over older terminal rows, shows the recorded step list and
offers the exact step actions. The bar and drawer stay within their own
terminal column next to a docked history. Reduced animations keeps the
track steady and opens the drawer immediately. The Settings preview uses
the same 30-pixel presenter and icon actions for pause, cancel, steps,
run menu and hide. A compact header keeps the step toggle early in the action row,
places run metadata in the progress tooltip, and scrolls the action row
when it overflows. Fedora ARM64 Wayland verified Review focus, drawer
motion, the step and run menus in a 42-column split, and stable pane
geometry. It also verified the review, completion and blocked track
colors, the active sweep and the steady track with reduced animations.
Fedora ARM64 X11 verified Review focus, all three track colors, the
active sweep and the steady reduced-animation track, and both action
menus in a 41-column split. Repeat the desktop workflow on Windows and
macOS.
[Top Bar with Drawer](PORTING-PARITY.md#top-bar-with-drawer-follows-typed-steps)

The d11 Minimap uses a 14-pixel rail beside the terminal, with a colored
segment per typed step. The hover preview opens to its left and shows the
step's result and output excerpt, the run phase and reported usage. Its
segment reveal, running glow and hover transition share the reduced-motion
preference with the Settings preview. The step menu stays reachable after
the normal hide delay while it is open. Fedora ARM64 Wayland verified the
hover preview and both context menus beside a docked history in the Dark
and IntelliJ Light themes. X11 verified both themes and both menus. In
a 39-column split with reduced animations, the first visible preview was
fully rendered, and the step menu stayed available until it closed past
the auto-hide deadline. Fedora ARM64 X11 also verified wheel scrolling
and exact output copying at both ends of a 21-command run. The typed
history retains every step during the run and when reopened. Complete
Minimap desktop acceptance on Windows and macOS.
[Minimap rail](PORTING-PARITY.md#minimap-rail-shows-typed-step-previews)

The d15 Orbit ring and its step list stay in the terminal beside a side
history or above a bottom history. The 72-pixel ring eases changes in
step progress over 400 ms. Its dots take 1.6 seconds to make a revolution,
hold their position when paused, and stay still with reduced animations.
The step list shows up to twelve recent entries above the circle and keeps
their contextual actions. The retained run keeps its own ring and labels.
The list opens radially from the center of the ring in 340 ms and folds
back in 260 ms. During opening, the circle limits pointer input to the
visible part of the list. The list is inert during folding. Reduced
animations skips both transitions. Fedora ARM64 verified animated opening
and closing with the side timeline and immediate transitions with the
bottom Log Table in both X11 and native Sway Wayland. The selected step's
exact output reached the desktop clipboard in each run. The terminal grid
and ring position stayed fixed. On Windows ARM64 the user saw the list
animate in a live terminal and used **Copy output** on the command step.
Verify the exact system clipboard result and reduced-motion workflow on
Windows, and repeat both desktop workflows on macOS. The earlier
102-test runs on Windows ARM64 and Apple Silicon predate the expanded
Orbit step-menu regression. Rerun that regression on the current source.
It checks the application's internal clipboard. The Windows run stopped
during compilation and produced no result for this test.
[Orbit progress and placement](PORTING-PARITY.md#orbit-progress-and-terminal-placement)

The d04 Frosted Glass HUD shows typed progress, model usage and the current
step in the lower-right corner of its terminal. The blur samples the rendered
terminal behind the card. The terminal viewport keeps its size while Details
opens the typed list above the card. Keyboard input in the terminal dims the
card to 35 percent until the typing idle interval elapses. The Settings
preview shares the presenter and the reduced animation preference. The HUD
fades in over 220 ms and grows from 94 to 100 percent over 240 ms around its
center on the first show and after hiding. Hiding the HUD closes its step
drawer and clears its temporary typing fade. Fedora ARM64 X11 and Wayland
verified the live drawer alongside side history, typing fade and idle
recovery, and copying the displayed answer. Wayland also verified a narrow
split and that typing in its neighbor leaves the original HUD undimmed.
A Wayland follow-up verified that the details stay closed on reopen. Frame
samples showed intermediate states with animations enabled and a fully
visible card in the first painted frame with reduced animations. An X11
retest verified that the menu hides the open HUD and reopens it with
Details closed. The Wayland contrast check sampled the Details label over
a plain terminal and over two contrasting bands of colored output in the
Dark and IntelliJ Light themes. Repeat the desktop workflow on Windows
and macOS.
[Frosted Glass HUD](PORTING-PARITY.md#frosted-glass-hud-with-typed-progress).

The terminal context menu's **Last agent run** actions resolve against
the newest traced run on that pane, including retained history. Design
menus keep the user's displayed run. The action checks the pane stream
and the newest run again at invocation. Desktop acceptance still needs
an older run selected in the design, a newer retained run, and a second
split with its own answer.
[Latest run selection](PORTING-PARITY.md#terminal-context-menu-selects-the-latest-agent-run).

The Conversation history (d13) offers a follow-up field in the pane and
detached history. Send or Enter opens a fresh Agent run on the same
terminal stream through the usual setup or environment review. The
conversation keeps the most recent five earlier questions and answers
from that stream when their run history is available. Verify draft entry,
stream replacement, policy changes, and the live history on native
desktops, including a split with its own Conversation history.
The presenter uses typed command bubbles with expandable output, exit
codes, and a pending-response indicator that stays still with reduced
animations. Verify live and retained output disclosures on native
desktops. Earlier turns open their step menus directly, including
retained steps. Verify copying and the Snippet Editor from both live and
retained turns, inside the pane and in a detached history.
The inline command approval bubble uses the exact reviewed command in
the active terminal stream. It works in the pane and the attached or
detached picture-in-picture history, with the Activity window available
for reviews that need its controls. New bubbles animate from their
creation time and render immediately with reduced animations. Verify
the controls, focus changes, narrow viewports and motion live on native
desktops.
[Conversation follow-up](PORTING-PARITY.md#conversation-design-follow-up-runs).

The side timeline (d02), Plan and Execution (d06), Card Stack (d07) and
Log Table (d16) present the same bound command approval at the newest
node, in the execution column, on the front card, or above the table.
Verify pointer and numeric decisions in live panes and attached and
detached history. Card Stack browses older cards using the wheel or
trackpad and resumes following the newest card when the user returns to
it. The six standard docks share Hide and Show actions while preserving
their run. Verify scoping, resize and restored review on native desktops.
The side timeline shows typed step nodes, pending review, and the final
answer. A new entry follows the end of the timeline. Updates to a step
preserve a manual scroll position. New nodes and connectors animate
with the native reduced-animations preference. Verify the rail motion,
scrolling, and the live review in docked and detached desktops.
Plan and Execution shows typed steps and progress beside the most recent
command, its exit code, elapsed time, and an output tail. Pending review
takes precedence and the final answer takes over when the run ends.
Verify the desktop layout and progress motion in both animation modes.
Plan and Execution draws a newly completed step's check mark over
320 milliseconds. Updates to an existing result retain the original
completion time. Reduced animations show the check mark immediately.
Verify the stroke and reduced-motion behavior on native desktops.
Card Stack shows typed
cards with command results and output excerpts, a browsable final-answer
card, and transitions for the front and receding cards. Verify visible
card controls, wheel navigation, and both animation modes on native
desktops.
[History-panel approval](PORTING-PARITY.md#command-review-in-the-other-agent-histories)
· [Side timeline](PORTING-PARITY.md#side-timeline-follows-typed-activity)
· [Plan and Execution](PORTING-PARITY.md#plan-and-execution-uses-typed-command-results)
· [Card Stack](PORTING-PARITY.md#card-stack-shows-typed-steps-and-the-final-answer).

Activity entries for recorded commands, their results and model steps
keep the typed step ID through activity eviction and finished-run
reopening. The timeline, plan and execution, card stack, conversation,
tmux and transcript histories offer the same step context menu as the
Log Table. The Minimap, step toasts and the PiP step list share the
command, output, entry and snippet actions. A toast remains visible
while its step menu is open. Native widget tests cover identically
titled steps and the exact copied output across six history modules.
Fedora X11 verified that the Minimap copied the exact command output and
that the PiP step list copied the approved command. Exercise the menus on
native desktops with a restored run and masked output.
[Java step menu reference](https://github.com/chardonnay/korTTY/blob/v2.26.0/app-docs/site/docs/en/features/ai-agent-designs.md#run-menu-and-step-menu)
· [Native test results](PORTING-PARITY.md#november-21-exact-step-actions-in-terminal-agent-history)

The d14 cursor suggestion and its Settings preview reveal the text
at 18 ms per character and pulse the Enter hint. Reduced animations
shows the complete command immediately and disables the pulse. Pressing
Enter or 1 while the suggestion is still appearing reveals the full
command. A fresh press can then approve that review. Holding the key
through a review transition preserves shell input isolation. Fedora X11
verified the held and fresh Enter presses and the Tab handoff to an
editable Bash prompt. The d14 review now maps 1 to a single approval.
When the Activity window offers approval for the remaining ordinary
commands, 2 grants that approval and 3 dismisses the review. Otherwise,
2 dismisses it. The keyboard action retains the displayed decision and
rechecks it before dispatch. Fedora X11 verified both approval modes,
including the exact command output and shell input isolation. The cursor
suggestion and Settings preview fade over 180 ms when the review closes.
Reduced animations clears the suggestion immediately. A fading suggestion
has no keyboard actions. Fedora X11 verified both settings and input
delivery during the fade. Verify the cursor interactions on Windows,
macOS, and Wayland, including reduced animations and SSH takeover.
The SSH takeover binds the reported working directory to the current
shell prompt, including when the directory and prompt arrive in separate
transport reads. Fedora Wayland verified that Tab inserted an editable
draft at an authenticated Bash prompt without sending an SSH exec request.
[Java cursor approval](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AgentDesignCursorApproval.java)
· [Cursor verification](PORTING-PARITY.md#november-21-terminal-agent-cursor-suggestion-motion)
· [Numeric decision verification](PORTING-PARITY.md#november-21-numeric-decisions-in-terminal-agent-cursor-approval)
· [Dismissal animation verification](PORTING-PARITY.md#november-21-terminal-agent-cursor-dismissal-animation)

The d08 Metro line follows typed command, analysis and question steps
and reserves an Answer station. Active stations pulse, a pending review
is amber, and a light moves to the current station. Reduced animations
keeps the rail still. The visible stations fit the terminal width and
the oldest retained stations collapse into a count. Clicking a station
opens its typed detail or the current review. The live status and the
Settings preview share the rail. Fedora Wayland verified station selection,
request review, completion, hiding and restoring the rail, and a vertical
split that narrowed the source terminal from 112 to 54 columns. Standard
selects d03 in-terminal blocks and the d08 Metro line with approval in
history and a fixed arrangement. Unavailable roles in saved designs
fall back independently to the Standard modules. The Metro header
counts unfinished runs on the same stream that share its status and
history panel. The same pane accepts up to five standalone Run, Ask, or
Planning runs with individual reviews and output routing. Environment
probes, ordinary commands and sudo operations acquire the pane's worker
slot. A competing approval returns to review with a fresh epoch.
Typed shortcuts also use the same five-run limit. A recognized
submission at the limit stays local, and an edited shortcut stays bound
to its original stream, privacy state, window and prompt. On Fedora,
both direct and Landlock-confined OpenSSH pass the real sudo/PAM
contention tests. The Fedora Wayland and X11 desktops verified
mixed-mode aliases on a sandboxed OpenSSH pane, separate Ask and
Planning reviews, the busy notice on a competing sudo approval, and
successful execution after a fresh approval and masked password entry.
X11 also showed the extra-run badge on that pane. On local panes, the
live Metro light crossed intermediate positions with animations enabled
and snapped to the next station with reduced animations. On Windows
ARM64, the native direct and LPAC test routes also verified separate
Ask and Planning reviews while Run completed real sudo/PAM. Repeat the
mixed-mode SSH desktop workflow on macOS and Windows. Verify rail
motion on Wayland, macOS, and Windows.
[Java Metro line](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AgentDesignMetroBar.java)
· [Java Standard preset](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/agentdesign/AgentDesignPresets.java)
· [Metro line verification](PORTING-PARITY.md#november-21-terminal-agent-metro-line)
· [Mixed SSH verification](PORTING-PARITY.md#november-21-overlapping-terminal-agent-runs)

The d09 tmux history shows the masked goal, one entry per typed step with
its command or title and result, and a terminal-style footer. A completed
answer appears behind a block cursor at 24 ms per character. Reduced
animations displays the full answer and keeps the cursor still. The
Settings preview uses the same history renderer. The side dock defaults
to about 40% of the pane and retains custom placement and width.
The docked d09 review uses numbered buttons and accepts the same choices
from the digit row or numeric keypad. A complete visible review is
required. The choice is rechecked against the pane, stream, review
revision, and current approval policy before dispatch. Per-command
confirmation offers only a single approval and cancellation. Java keeps
the d09 history visible after completion. Its timed setting applies to
eligible status displays. The block cursor keeps blinking while the
finished history is visible. Run actions and View › AI Agent Panel
hide and restore the history, releasing and reclaiming its dock space.
A new review brings the history back. Fedora Wayland verified hide and
restore, the animated and reduced-motion cursor, and digit 1 on the
environment-probe review. The review ends the scrollable history so
the cursor does not push it past the bottom anchor. Fedora Wayland
verified that a hidden card leaves digit 2 with the shell, a complete
card at the bottom accepts approval for remaining ordinary commands,
and digit 3 cancels a complete review. Verify these interactions in the
Windows and macOS desktop windows.
[Java tmux pane](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AgentDesignTmuxPane.java)
· [Tmux history verification](PORTING-PARITY.md#november-21-terminal-agent-tmux-history)

The d18 Neon Ticker and Settings preview share the flicker, rolling
message transition and pulsing activity light. Reduced animations keeps
the complete label and light steady. Amber identifies pending input, and
Review opens the Activity window. Pause, resume, cancel and run actions
use the existing run. The close button hides the status and View › AI
Agent Panel can restore it. Fedora Wayland verified the visibility
controls and the activation of the existing Activity window from both
Review controls. The activation uses the click's serial and leaves the
probe awaiting approval. Verify the transition, review handoff and
visibility controls on Windows, macOS and X11. Exercise the Wayland
handoff on additional compositors and with keyboard and touch input.
[Java Neon Ticker](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AgentDesignNeonTicker.java)
· [Ticker implementation](PORTING-PARITY.md#november-21-terminal-agent-neon-ticker)

Timed status displays and the Settings preview share a 250 ms fade after
the configured delay. During the fade, the status retains its place in
the layout. Reduced animations removes the status at the deadline.
With a zero delay or a manually restored status, the display stays until
the next visibility change. Fedora Wayland verified the intermediate
frames, reduced motion, zero delay and manual restoration for the Metro
line. Repeat the visibility checks for the other status designs and on
the remaining native desktops.
[Java status visibility](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AgentStatusVisibility.java)
· [Status fade verification](PORTING-PARITY.md#november-21-terminal-agent-status-auto-hide-fade)

Interactive Run can review a complexity assessment after the environment
probe. The AI Manager preference and the managed override select when to
offer solutions. A selected approach, an original idea or additional
instructions enter the next reviewed request as plan context. The pinned
OpenSSH fixture verifies the reviewed directory and rejects a directory
change before sending the next request. Verify the choice card,
cancellation, timeout and policy locking on native desktops with local
and SSH targets and a configured provider.
[Java triage](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/core/TerminalAgentService.java#L927)

Run clarification offers the model's suggestions, an editable answer, optional
instructions and a choice to let the model decide. Expiry discards the draft.
Answers remain tied to the question across later decisions and the final
response. After the interactive question limit, the next review includes the
unanswered question and consumes the previous model turn. Verify response
entry, cancellation and the next masked preview in native Agent and Swarm
windows. [Java question handling](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/core/TerminalAgentService.java#L1108)

AI Manager General offers a default presentation for interactive Run. Its
Chat tab keeps the source terminal binding, reviewed requests and command
approvals. Closing the tab cancels its owned operation. Java backups fill the
unset preference. Planning opens a dedicated tab. Implement rechecks the
plan destination and selected profile, then opens a separate Run using the
current presentation preference. The accepted plan remains readable and the
new Run starts with an environment-probe review. Verify tab-based planning,
handoff, request review and cancellation on native desktops.
[Java Run tab](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/MainWindow.java#L12244)
· [Java accepted-plan launch](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/MainWindow.java#L13209)

Choose Implement this approach to request a report for a selected approach
and review it for implementation. Accept plan and open Agent checks the
original stream, profile and reviewed directory, then opens the separate
Run with a fresh probe approval. Keep plan for later retains the report in
Planning. Verify both actions and the report preview on native desktops.
[Java implementation entry point](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AiAgentPlanTab.java#L561)

An implementation run on SSH, in a native POSIX shell or in a Flatpak host
shell offers sudo authentication before its first model request when the
fresh approved probe finds a non-root account with sudo and passwordless
sudo is unavailable.
The user can authorize manual password entry or continue to model review.
SSH also offers a matching vault credential. The run retains its private
authenticated terminal and continues to review each elevated command
separately. Rejected preflight authentication permits up to three separately
approved retries. The native local PTY starts from the opened directory
descriptor and checks its working directory before privileged commands.
Flatpak checks the physical path and identity against the opened directory
before prompting on a private host PTY. Authentication stays bound to that
route for subsequent approved commands.
Fedora ARM64 passed native local system sudo/PAM and real OpenSSH sudo/PAM
for synthetic accounts in disposable namespaces. The direct and
Landlock-confined SSH paths rejected a wrong vault password, approved a
fresh retry, and reused the authenticated terminal for a separately
approved command in the pinned directory. A separate
Flatpak test used the actual host service to verify prompt handling,
directory rejection and cancellation. The system sudo/PAM fixture also
passed through the Flatpak host service. It rejected an incorrect password
and reused the authenticated private PTY for a separately approved elevated
command. The ARM64 release bundle passed installation, native Wayland
startup and a packaged CLI check of the host terminal. A separate native
X11 run of the installed bundle accepted a plan, rejected a wrong password,
accepted an explicitly approved retry and ran the separately approved step
through its private sudo terminal. Apple Silicon passed the direct and App
Sandbox client paths against the isolated Fedora server with real sudo/PAM.
Windows ARM64 passed the same server checks through direct SSH and the
native LPAC worker, including cancellation while the peer stayed open.
The macOS helper has ten seconds to confirm sandbox startup. A packaged
test also verified the separate configured SSH handshake timeout and
cancellation. Verify the remaining desktop choices and policy changes and
the local macOS system sudo route.
[OpenSSH sudo/PAM verification](PORTING-PARITY.md#november-21-real-openssh-sudopam-for-the-terminal-agent)
[Flatpak sudo and package verification](PORTING-PARITY.md#november-21-flatpak-host-sudo-for-the-terminal-agent)
[Java sudo preflight](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/core/TerminalAgentService.java#L1620)

The Planning tab shows all clarifying questions together. Users can revise
each suggested answer and add multiline details where allowed. Review
answers prepares one masked model preview for the whole set. Users can edit
their answers again from the approaches or final report. An unsuccessful
revision keeps the previous approaches and report available. Verify scrolling,
editing and returning to earlier results on native desktops.
[Java clarification form](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AiAgentPlanTab.java#L269)

The Planning tab accepts multiline refinement notes. Review revised
approaches or draft the final report from a selected approach using the
notes. Each request shows its masked content and rechecks the destination.
A failed model revision preserves the last available approaches or report.
Verify editing and repeated refinement on native desktops with a configured
provider.
[Java plan refinement](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AiAgentPlanTab.java#L329)

Ask opens a temporary tab with its existing model review. A follow-up keeps
that tab, the original masked selection, question, and answer and opens the
Chat composer. Before that handoff, the original terminal still shows the
selected Agent design. Each later request uses Chat review and requires the
same live terminal stream and current policy. Save is explicit. When the
selection is one filename, Ask loads a bounded UTF-8 snapshot from the
original local shell directory or SSH connection. The masked preview offers
removal and the send rechecks the source and file version. A missing or
unavailable file leaves the selection in the preview. The approved snapshot
can follow the conversation into Chat. Verify the native tab, attachment
review and cancellation workflows.
[Java Ask workflow](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/MainWindow.java#L12262)

Planning can collect a reviewed environment snapshot from the source local
shell or authenticated SSH session. The fixed probe covers system and account
details, the working directory, free space, available management tools and
sudo capabilities where the shell supports them. Windows Command Prompt
reports system, account and directory details. The planning tab shows a
masked summary and includes the same snapshot in the reviews for questions,
approaches and the final report. Users can also plan without a probe. The
implementation run still requires its own review. The pinned OpenSSH test
verifies the approved probe request, the reviewed model payload and rejection
of a later send after a directory change. Verify the native desktop approval
and skip flows.
[Java planning probe](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AiAgentPlanTab.java#L251)

Interactive Agent and Swarm member reviews include masked memory for the exact
saved target. Swarm workflow script generation and comparison review global
and organization memory. Member requests, workflow scripts and comparisons
revalidate the source revisions and newly enabled memory before dispatch.
Verify the desktop previews and replies with the configured local, HTTPS and
CLI providers used by each workflow.

The Agent's **Sent along** link opens the masked memory entries, skills
and external files from the last confirmed turn that included context,
with counts and estimated tokens. The receipt belongs to the exact run
and follows retained non-private history. A live interactive review can
include previously approved references under the existing memory policy,
and rechecks the included files before dispatch. Verify the disclosure
with simultaneous runs, switched panes, reopened history and Activity
windows on native desktops.
[Sent context parity](PORTING-PARITY.md#context-sent-with-a-reviewed-terminal-agent-request).

The native Agent prompt history adopts masked Java prompts and their original
timestamps and uses the Java entry limit when creating a new store.
Existing Rust entries and the local limit take precedence. Java popup width
and height fill independent unset preferences. The native history chooser
resizes from its top and right edges and saves the adjusted size in the
profile. Delete one entry from the terminal Tab chooser with Delete or the
row button. Clear all requires confirmation of the full saved set. Concurrent
changes reload the chooser for fresh review. Verify restore, recall, editing
and resizing in the native desktop flows.
[Agent prompt history import](PORTING-PARITY.md#java-terminal-agent-prompt-history)

The Swarm workflow dialog offers a multiline instruction editor and a
ten-entry history. Choosing an entry fills the editor for review.
Approved generation persists the masked instruction. Java backup review
includes masked `workflowInstructionsHistory` for available slots.
Verify the chooser and restore review on native desktops.
[Swarm instruction history](PORTING-PARITY.md#swarm-workflow-instruction-history)

The Swarm status strip shows aggregate counts and opens the current target
transcript from its indicator. Target details include elapsed time and model
tokens when available. The slow threshold starts at 180 seconds and becomes
twice the median of successful target times with a 60-second minimum after
two successes. Only active work can be marked slow, and an explicit pause
stops its clock. Saved runs show static indicators with available timing and
usage. Verify the responsive layout and mouse and keyboard navigation in a
native Swarm window, including a retry and an archived run.

### Agent shortcut diagnostic and implementation contract

The built-in Agent exists. Its terminal shortcut is enabled separately and
defaults to off. Recognition requires fresh OSC 133 A/B prompt markers, a live
UTF-8 Xterm primary screen at its bottom, and no password prompt or
incompatible full-screen application. Submission checks the stream's
standalone run limit and Swarm ownership. A previous-prompt marker alone
does not prove that OSC 133 B marked the prompt end and input start.

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
target. The menu entry **AI → AI Agent…** supports reviewed local starts,
including Windows absolute-directory review. A direct custom Bash, Zsh or
Fish start with validated interactive flags uses the fixed POSIX runner.
Custom PowerShell and CMD starts with validated flags use the corresponding
system runner on Windows. The review binds the runner and directory. A
saved plan for a custom shell also binds its launch arguments. Local Run
requires an unrestricted local shell under the effective isolation policy.
Native PTY tests verify opening a saved profile, using ordinary shell input,
and reaching the probe review through `agent` on macOS Zsh, Fedora Bash and
Fish, and Windows PowerShell and CMD. The native Windows shell and Agent
worker verify an equivalent drive-letter working directory for extended
local paths. Verify visible desktop interaction and the packaged apps.
[Java local command runner](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/core/agent/LocalShellArgv.java)

Acceptance must record the exact executable, opt-in, shell, received markers,
`agent`/Tab behavior and correct pane identity. Do not infer the user's root
cause from a repository source check or an old ZIP alone.

## AI foundations and AI Manager

Terminal Chat offers a separate ten-entry prompt history for terminal
**Ask**, with recall, clearing and masked persistence after send approval.
Private sessions have no access to it. The Java backup review includes
masked `aiPromptHistory` entries for available history slots. Verify native
selection and clearing, restart and backup review on the remaining desktops.
[Terminal prompt history](PORTING-PARITY.md#terminal-chat-prompt-history)

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| B1 | Implementation + acceptance. Managed/local, native LM Studio, OpenAI-compatible HTTPS, Anthropic HTTPS and reviewed exact HTTP(S) loopback Messages profiles, and reviewed configured CLI profiles exist. Server default can be selected in an explicit Terminal Chat review on supported HTTP transports and in standalone Agent Ask and Run for loopback and explicitly opted-in OpenAI-compatible HTTPS profiles. Advisory Planning supports loopback and explicitly opted-in OpenAI-compatible HTTPS server-selected profiles. Automatic text journal summaries support an explicitly assigned saved HTTP loopback Journal profile with server selection. The named CLI catalogue provides passive native path suggestions, fixed presets and reviewed LM Studio model loading. The Manager offers catalog-based GGUF recommendations by role and RAM, eight cloud presets with offline examples, and anonymous loopback model discovery. The separately signed model and prompt catalog verifies the current sequence and falls back to the verified cache or bundled data. The guided setup reviews the signed runtime and exact model files, runs a local test for each model, and saves the selected roles together. Standalone Agent Ask/Run, interactive and scheduled Swarm and Agent, and journal summaries use reviewed HTTPS or CLI requests. | Verify the live server-default workflows with a provider that supports omission and the loopback Messages flow with configured providers and native review. Assess Swarm, scheduled Agent/Swarm and the vision/embedding consumers that still require a resolved model. Resolve noncanonical Java paths against pinned examples. Verify the live signed catalog with the publisher trust key and audit the remaining specialized prompt consumers. Verify a real guided install, inference, cancellation and role save on native desktops. Verify the reviewed routes with enabled providers and native UI, including MiniMax login and regional endpoints, live LM Studio loading, macOS executable quarantine and owned-helper cancellation. J `core/AiCliProviderRegistry.java`, `core/AiServiceFactory.java`. |
| B2 | Acceptance. Bounded answer/reasoning streams and provisional analysis/progress are implemented for supported native providers. Generic CLI buffering until exit also matches the pinned CLI behavior. R `src/local_ai/streaming.rs`. | Real provider interruption, protocol errors, cancellation and complete saved/exported final results. Do not list streaming itself as absent. |
| B3 | Acceptance. Reviewed single/split GGUF acquisition, resume, signed runtime channels, Off/Notify/Automatic Stable, version pins, withdrawals and idle staged activation exist. | Signed-channel interruption, disk limits, withdrawn packages and actual supported runtime execution on each native target. Preserve leases and last verified usable installation through failures. U1 is the separate application-update channel. |
| B4 | Implementation and acceptance. GGUF and MLX Configure windows are implemented. Windows ARM64 verified GGUF field order, save and cancel. Apple Silicon passed signed MLX reasoning, Java setting adoption, stale-setting rejection, live cancellation, idle restart, runtime reinstallation and rollback. Signed llama.cpp runtimes passed GGUF vision and inference from external single files and split sets on Apple Silicon, Fedora ARM64 and Windows x64 under emulation. All three detected replacement of the final shard, and re-registration restored inference. Windows blocked replacement while the read lease was active. | Verify the compact GGUF Configure layout on the remaining desktops. Complete the owned import and backend reviews. [Completion checks](#implemented-features-with-outstanding-checks) Exercise publication age, sorting after restart, the MLX default and Configure in the combined inventory on Apple Silicon. Exercise native Windows GGUF vision and external references, and additional runtime versions when published. Native Windows x64 needs an x64 host and native Windows ARM64 needs a signed ARM64 package. An unsupported-model request on mlx-lm 0.31.3 reached the configured request timeout. |
| B5 | Implementation and acceptance. Persistent indexes, approved automatic refresh and per-store opt-in exist. Agent Ask/Run, Planning, Swarm members and comparison have separate query and prompt reviews. Scheduler proposals, per-firing Agent and Swarm, read-only Swarm and optional scheduled comparisons can include exact reviewed excerpts from local and anonymous or vault-backed Qdrant indexes. | Repeat the verified Windows source-status restart and recovery checks on the remaining native desktops. [Completion check](#implemented-features-with-outstanding-checks) Verify native Manager and request previews, cancellation, saved/reopened jobs and source/embedding changes with configured providers. Durable environment-key Qdrant is a native extension. Compare larger corpora under B6. J `ui/RagKnowledgeStorePane.java`, `rag/RagRuntimeService.java`. |
| B6 | Acceptance + capacity audit. UTF-8/PDF ingestion, page citations, persistent local HNSW and optional Qdrant exist. The PDF producer streams and hashes the held descriptor in 64 KiB chunks under the worker permit. New sources default to 50 MiB. Reviewed UTF-8, PDF and folder settings support up to 1 GiB with a 500 MiB scan limit. Large PDFs use private temporary input mapping. A store supports up to 5,000 source files. Local snapshots support up to 5,000,000 passages and 64 GiB. Qdrant snapshots are bounded to 16,384 passages and 96 MiB. Above the 8,192-passage HNSW limit, search and refresh read vectors from the validated local snapshot for exact scoring or reuse. New and reused vectors are staged in a private temporary file for large local builds and refreshes. Saved Search bounds the exact score heap to the requested hits. Terminal Chat consumes exact scores in rank order until the excerpt budget is full. Earlier automatic approvals retain their original format, discovery and Qdrant bounds until reviewed again. | The version 8 passage ceiling matches the Java snapshot reader at 5,000,000. Verify indexing, retrieval and refresh with real sources and realistic embeddings at higher corpus tiers. Large queries and refreshes read the previous snapshot text from the held file. Large builds and refreshes stage replacement passage text and vectors in private temporary files. Snapshot decoding and local publication support cancellation. Measure disk and CPU costs at the largest supported sizes. Verify native limit/Vault controls and configured HTTPS Qdrant recovery. J `rag/RagSourceScanner.java` and `rag/LocalHnswStore.java`. |
| B7 | Acceptance + bounded audit. Connection skills, reviewed relevance/classification, usage ledgers, forecasts and graphical profile quota meters exist. Profiles can select Estimate, CL100K, O200K, P50K or R50K for reviewed request forecasts. J `model/AiTokenizerType.java`. | Audit other consumers for exact missing forecast/usage paths. Complete native meter and real-provider accounting acceptance. |
| B8 | Acceptance + scope decision. Reviewed local/SFTP text attachments, saved masked snapshots, native flowcharts, direct Tavily and the five LM Studio MCP internet modes exist through reviewed loopback HTTP or HTTPS. Supported profiles can submit an exact reviewed webpage URL through Tavily Extract, Bright Data or SearXNG. Java migration offers supported integrations for explicit adoption. The imported Toolpack plugin requires exact tool names supplied during configuration. A standing Scheduler review can authorize a model-selected search and up to two follow-up lookups about a public topic at every firing. Guided iterative Swarm supports separately reviewed searches and opt-in page reads after command batches. J `model/AiInternetAccessMode.java`. | Verify the review, TLS and imported Toolpack workflows against installed LM Studio. Verify attachment identity, size and change, archived follow-up and actual configured MCP services. Review the data scope for automatic tool selection from later command output. |

Recent evidence includes
[LM Studio discovery](PORTING-PARITY.md#scheduler-journal-details-java-retention-and-ai-provider-discovery),
[local HNSW](PORTING-PARITY.md#persistent-local-hnsw-knowledge-search),
[Qdrant](PORTING-PARITY.md#optional-qdrant-knowledge-backend),
[reviewed Agent Run](PORTING-PARITY.md#reviewed-standalone-terminal-agent-run-requests),
[interactive Swarm comparison](PORTING-PARITY.md#reviewed-ai-comparison-of-completed-swarm-targets)
and [signed MLX acquisition](PORTING-PARITY.md#bash-declarations-signed-mlx-acquisition-journal-chats-and-attachment-quotas).

Automation closing summaries now support a separate reviewed model choice
from terminal recordings. Existing settings inherit the recording route.
Changing the automation route or the reviewed evidence scope revokes
affected queued approvals. Scheduler jobs and interactive Swarm can select
their own closing-summary profile. The run records the choice, and external
requests require the matching journal approval at the start of the run.
Policy-provisioned HTTPS profiles use exact named identities and bind the
installed definition through Agent, Swarm and Scheduler review and dispatch.
New Scheduler AI drafts offer the available global default for review, and
Swarm can refresh its provider choices. An independent HTTPS or CLI approval
can bind a Scheduler job or interactive Swarm to its selected provider,
redaction rules and journal evidence scope. Each eligible capture carries a
proof tied to its journal and run. Revocation invalidates outstanding proofs,
including when a Scheduler job is deleted. Existing global grants continue
to authorize routes they already cover.
Java Scheduler restore preserves journal options and matches the source
profile by exact Java identity and imported definition from the same backup.
The Scheduler can offer the corresponding native profile after AI Manager
review. The user selects it explicitly, and the journal still requires its
outbound approval. Missing or conflicting profiles remain unresolved.
Java Scheduler AI Agent and AI Swarm actions restore as inactive drafts with
their goal, profile source, targets, schedule, prerequisites and Swarm mode.
The native editor requires an explicit profile selection and validates the
action before conversion. Java command approvals are discarded. Reviewed
per-start Agent and read-only or iterative Swarm jobs support dependency and
cascade starts. The new workflow review binds the dependency settings.
Each firing still requires its own exact command approval for generated
SSH commands. Earlier approvals cover their original manual/calendar scope.
Live native provider acceptance remains open. Java recording and automation
journal profile assignments are restored as separate suggestions. The native
profiles match the imported ID and definition, and each role requires explicit
selection in Journal summary settings and the existing provider approval.
AI Manager can also adopt the imported regular assignment as the Session
Journal role for questions after the profile is reviewed and saved. Adoption
does not create a recording or Scheduler approval.
The Journal role for questions accepts saved server-selected HTTP loopback
profiles. The question retains the reviewed completion path and sends the
prepared messages without a model field. The local route applies to questions.
Automatic summaries and screenshot workflows still need their own eligible
profiles. Reviewed LM Studio-compatible loopback HTTPS profiles use the
existing HTTPS authority for questions and text summaries. The private TLS
fixture exercises exact reviewed requests, the evidence-free summary probe,
and revocation. Native desktop acceptance with a configured server remains
open.
Java profiles with an explicit model or LM Studio automatic selection and a
supported loopback endpoint restore as unreviewed drafts. They preserve the
OpenAI-compatible or native LM Studio transport and require endpoint review
before use. Manual includes additional localhost HTTP and loopback HTTPS
forms. Omitted selection mode follows Java's blank-model Auto rule.
Supported Java web modes appear as inactive profile suggestions. The AI
Manager requires explicit adoption after endpoint review. Verify migrated
choices against configured services on native desktops.
Native LM Studio and managed LM Studio routes read exact model metadata for
vision and reasoning. The profile editor offers the reported native reasoning
settings and the provider default. Journal screenshot analysis checks
automatic vision before preparing or sending an image. Verify the profile
editor, the loaded model switch and actual text and image inference against
configured LM Studio on native desktops. LM Studio Auto resolves the
only loaded chat model or the exact preferred model when several are loaded.
Reviewed requests validate the concrete selection again before sending.
The OpenAI-compatible LM Studio mode uses the same inventory with its own
chat transport. Java Auto imports preserve their optional preferences and
chat protocol for supported `localhost` and numeric loopback destinations
over HTTP and HTTPS. The resolver connects directly to loopback and preserves
the origin for certificate checks. HTTPS profiles use the existing reviewed
workflows. Explicitly configured native MCP uses the same loopback HTTPS
transport with a separate tool review. Native testing against installed
LM Studio remains open. Supported model-less DEFAULT profiles already
restore as server-selected chat drafts. AI Manager can adopt a reviewed
Java global DEFAULT assignment as the Terminal Chat default. The assignment
preselects the profile for request review. Standalone Agent Ask and Run
support explicitly selected profiles. The Java Text assignment can be adopted
for Terminal Chat, reviewed manual questions and journal note translation.
Saved Rust SSH and local shell profiles can select a server-selected chat
profile in Connection Manager. Terminal Chat and eligible standalone Agent
Ask and Run use that binding. Advisory Agent Planning accepts loopback and
trusted OpenAI-compatible HTTPS server-selected profiles. HTTPS requires
its own saved Planning opt-in. The masked request review displays the
endpoint and the server-selection mode before sending a request without a
model field. Verify the workflow with a configured provider and native UI.
Java connection-level AI assignments restore as inactive SSH and local-shell
suggestions. Connection Manager can adopt a reviewed profile with matching
backup provenance. Missing profiles retain the Java ID for manual mapping.
Reimport can add a missing hint to an unambiguous existing connection.
Existing native choices and explicit dismissals take priority.
The imported Default, Text, Coding, Agent, Security and Journal assignment
reviews are implemented. Remaining server-default restrictions are in the
generic Local and ReviewedHttps profile routes: interactive Swarm and
scheduled Agent/Swarm retain their existing compatible-model requirements.
Automatic text journal summaries can use server selection through a saved
HTTP loopback Journal role. The durable receipt binds the profile and exact
destination. Vision and embeddings also require
resolved model identity/capabilities. Audit each consumer separately and
record support or a justified restriction before changing these checks.
Endpoint paths outside the implemented validators also need a disposition.
Embedded Java profiles restore as inactive drafts with the source model ID.
The AI Manager maps each draft explicitly to a registered GGUF chat model or
to an MLX model on Apple Silicon. It checks the registration before enabling
the mapping and again when saving. Journal, Scheduler and Swarm use the
reviewed profile after explicit selection. Verify native MLX and GGUF
inference with a migrated profile. The AI Manager displays supported source
reasoning choices after MLX mapping and requires explicit selection. It keeps
other imported settings as provenance.
Swarm and Scheduler share the Terminal Chat review for direct Tavily and the
five LM Studio MCP search modes. A separate review includes the masked
excerpts in model requests. Standing Scheduler approvals bind the exact
excerpts for Agent, Swarm and optional comparison across firings. Recheck the
configured providers on native desktops. Java also retrieves fresh results
inside the model's tool loop at firing. Direct Tavily and the LM Studio
Tavily and Bright Data profiles support an exact reviewed page URL and a
separate masked excerpt review in the same surfaces. Page requests use
Tavily Extract or the single Bright Data remote MCP `scrape_as_markdown`
call with the submitted URL as its argument.
Standing Agent, iterative Swarm and read-only Swarm jobs can require a fresh
lookup at each start. Each run waits for approval of the exact masked query
or supported URL, then for approval of the returned masked excerpts.
The excerpts apply only to that run and its reviewed Swarm comparison.
Standing Agent and Swarm approvals can instead give one reviewed public
research topic to a planner at each firing. The selected model proposes the
query before any job output or evidence is available. The configured search
route supplies masked excerpts to that run and its reviewed comparison.
The grant binds the profile and redaction rules. A new standing review can
authorize up to two follow-up lookups. The planner receives the approved topic
and masked web excerpts from that run. Page retrieval requires a configured
reader and a source URL from supported results. The excerpts are bounded and
shared across reviewed model requests. Existing grants keep the single-search
scope. Native tests with configured providers remain open. Java's direct
Tavily integration can execute two tool rounds per completion with up to three
lookups in each round. Java can choose tools from a later diagnostic
completion that includes SSH probe details or command output. Guided read-only
and iterative Swarm jobs can review one post-probe search per target. The planner
receives the public topic, OS family and numeric kernel major/minor. Each target
keeps its own masked excerpts, while the run-level lookup and comparison retain
the original excerpts. A guided iterative Swarm can separately allow its
diagnostic model to propose one public search after a command batch. The
Scheduler reviews the exact query and the returned masked excerpts before
resuming that target and round. A separate standing opt-in lets that round
propose an exact page URL through a configured reader. The Scheduler strips
query parameters and fragments and reviews the exact URL and its masked
excerpts separately. Interactive Agent Ask and Run, including
Swarm member requests after a command batch, offer an exact query or page
review with a separate excerpt approval for the current model request. Native
acceptance with configured services remains open. Automatic follow-up selection
from arbitrary command output remains open. LM Studio controls its own tool calls.
J `core/OpenAiCompatibleAiService.java`, `core/TerminalAgentService.java`.
SearXNG profiles can configure a direct Streamable HTTP MCP endpoint for
`web_url_read`. Java's saved site URL describes the search service. Review the
separate MCP endpoint explicitly in Rust after migration. The published
SearXNG MCP 2.10.1 server passed authenticated search and page-read checks on
Windows ARM64 with a local backend. [Provider verification](target/verification/20261114-searxng-published-provider-windows-arm64.txt).
Full desktop acceptance with an operated SearXNG instance remains open.
Automation journals keep Internet access disabled.
Journal settings support per-direction line limits, a context-token budget,
context-only mode and explicit adoption of the values and chunking choice
from a Java backup. Summary passes can group adjacent complete commands and
condense or chunk oversized commands within the selected budget. Requests are
checked against the profile's tokenizer. Updated limits require fresh
automatic approval. Complete-output mode splits a retained block into
ordered requests when masking or serialization takes it over the exact
budget, preserving a single checkpoint. Verify native settings and configured
providers. Native CLI argument templates support model, reasoning and
prompt-file substitutions within individual arguments. Prompt content uses
stdin or private files. The exact built-in Java MiniMax preset restores with
the native stdin adapter after review of the local Node executable, MiniMax
package, login configuration and region. Verify a live MiniMax invocation.
Custom templates with inline prompt content require editing during migration.

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
| S1 | Implementation, acceptance and reduced-editor scope. Rust, Python, shell, Markdown, AsciiDoc, Dockerfile, SQL, Perl, Ruby, Lua, PowerShell, Java, Groovy, JavaScript and TypeScript structure scanners exist. Python recognizes `with … as`, `except … as`, unpacking and initial augmented assignments and classifies `dict()` and `tuple()` bindings. Lua syntax and completion, LuaJIT detection, and reviewed native format and syntax actions are implemented. | The adjustable library/editor divider remembers its preferred ratio across resizes and restarts. Verify the live divider on native desktops. The multi-language completion list and opt-in inline AI suggestions are implemented. Review remaining language-specific completion cases and verify preview, acceptance and dismissal in the native editor. Verify native jumps after edits, nested scopes, repeated names, Unicode, placeholders, and editor tool review, cancellation and SFTP save. Verify discovery and launch from Finder or Dock. The [Step 5 product audit](target/verification/20261112-step5-product-audits.md) records the reference structure cases. |
| S2 | Implementation and acceptance. Explicit reviewed completion, improvement, correction, translation, descriptions, selectable snippet alternatives and security actions exist, including description-only Apply/Undo, compact and embedded commands, and reviewed HTTPS and CLI assistance. Standalone Security Check offers per-finding selection and optional migration. Unsupported or unavailable local source tools can open reviewed AI formatting or an advisory syntax review. | Complete the remaining assistant controls and completion behavior described below. Verify configured providers and native selection boundaries, changed source, alternative choice and preview, discard/undo, original-profile reruns and explicit clipboard review on each desktop. |
| S3 | Implementation and acceptance. Hardening, validation, script headers, optional language migration, optional modularization, durable selective publication/recovery and workflow diagrams exist. Manual split planning and per-module generation use reviewed HTTPS or CLI alongside local and managed profiles. Outbound reviews bind the saved source, profile, credential context and current job, and are renewed after restart. Single-snippet and folder analyses accept saved CLI profiles. After a saved HTTPS or CLI analysis, the opt-in follow-up opens a separate split preview for that exact report and profile. | Verify actual configured providers, native optional-chain/option-only flow, language eligibility, migration before selected changes, collisions, cancellation, selected-file publication and restart recovery. |
| S4 | Resolved. Pinned Java project/folder Apply uses task-local preview state and saves accepted files without durable project Apply/POST records. | No new durable folder Apply/POST implementation is required for confirmed parity. Existing single-snippet durable recovery/POST and S3 modularization remain in scope. |
| S5 | Acceptance + bounded audit. Native per-snippet diagram persistence, selection/revision binding, stale previews, source jumps, regeneration/deletion, structured interchange and backup exist. Private Java JAXB `<snippets>` library migration retains diagram ownership, source digest, selection bounds and stale references. | Verify private-library migration on native desktops and review unsupported diagram formats against concrete examples. Java standalone snippet exports omit diagrams. Native focus/navigation/restart/whole-scene export remains acceptance. |

Evidence includes
[snippet HTTPS assistance](PORTING-PARITY.md#correcting-snippet-descriptions-and-reviewed-https-assistance),
[workflow publication](PORTING-PARITY.md#workflow-settings-flowcharts-and-snippet-publication),
[saved diagram navigation](PORTING-PARITY.md#folder-retry-and-saved-diagram-source-navigation)
and [Python bindings](PORTING-PARITY.md#model-policy-enforcement-and-python-structure-bindings).
The seven supported native diagram families and PDF/SVG/PNG exporters exist.
Their complete interaction and export matrix belongs to V5.

For S2, the tagged snippet editor offers optional AI formatting when the
formatter is unavailable or cannot meet a requested line width. It also
offers an AI syntax check for unsupported lint languages. Rust exposes
built-in and installed tools and the two reviewed AI fallback actions.
Verify that formatting preserves the selected range and that syntax review
leaves the draft unchanged on each native desktop.
The native snippet editor offers local completions with Ctrl+Space or
Cmd+Space and with a configurable shortcut. macOS also supports Cmd+I and
Option+Escape. The default Shift+Tab opens the list at the end of a
nonblank line. Snippet Editor settings can record
and reset the shortcut. Java's saved chord fills an unset native preference
and its primary modifier follows the destination platform. The list uses
the detected language at the caret, prioritizes structured bindings over
common identifiers, and offers common Bash, Perl, Python, PowerShell and
Ruby idioms. Lua retains its existing standard library candidates. The
native list filters while the user edits the same token. Enter or Tab
accepts the selected match. Idiom insertion selects the first template
field. Tab and Shift+Tab move between fields, and Tab leaves the template
after the last field. The completion list can request up to five model
candidates through the existing reviewed AI profile workflow. It sends a
bounded code window and known symbols, and adds matching distinct results
after the local entries. Each request opens the standard review by default.
The review can also approve automatic requests when the list opens during
the current Snippet Manager session. The approval covers the reviewed
provider, model, and instructions, and can be revoked from the list.
Inline AI suggestions can be enabled separately by reviewing a request
from the end of an editable line. After a 900 ms typing pause, the editor
can display a continuation at the caret. Tab accepts it, Escape dismisses
it, and Alt plus the physical bracket keys cycles alternatives. Multiline
previews show the continuation's lines in the editor viewport and move
the following source rows and line numbers down until the preview closes.
The same list includes native language suggestions for JavaScript,
TypeScript, JSON, CSS, and HTML. It uses common script globals and
keywords, recognized receivers and simple local declarations, JSON keys
and literals, CSS properties and values, and HTML tags and attributes.
TypeScript suggestions follow explicitly annotated local variables
through object types, interfaces, aliases, and nested member access,
including optional chaining. The same lookup applies to annotated named
parameters in function blocks and in both block- and expression-bodied
arrows. Explicit class types and local constructor calls expose public
instance members, including typed fields and getters. Completion follows
local class inheritance and uses the nearest override. Public constructor
parameter properties are available as instance members and through
inheritance. Explicit generic arguments and declared defaults propagate
through local aliases, interfaces, classes and base classes. Annotated
destructuring in local declarations and function parameters follows
object properties and array or tuple positions, including nested and
renamed bindings. Unescaped string-literal keys in computed bindings,
structural object rest, and array and tuple rest retain their types.
Member lookup follows literal string keys, decimal indices and optional
bracket access. Tuple lookup expands local generic fixed spreads and
resolves trailing array spreads through indexed access and destructuring.
Local unions suggest members shared by their resolved alternatives.
Intersections combine members across their constituent types. Nested
access follows both rules through generic aliases and parenthesized
combinations.
Imported types, runtime-computed and escaped keys, object rest on class instances,
positions whose type depends on a variable-length spread before a suffix,
inferred generic arguments and narrowing nullable or discriminated unions
remain open. Finite mapped types support local literal keys, structural
`keyof` domains, shared value types and homomorphic `T[K]` mappings.
Local completions also resolve `Pick`, `Omit`, `Record`, `Partial`,
`Required` and `Readonly`. Open string and number index signatures,
`Record<string, V>`, `Record<number, V>` and open mapped declarations
resolve the value type for known string and numeric keys. The editor
normalizes signed decimal and exponent literals and binary, octal and
hexadecimal integers within the safe integer range. Declared properties
retain their own types.
Open dictionaries list only declared member names. The editor follows
the exact decimal key produced by `.toString()` on a BigInt literal or its
local constant alias. Direct BigInt subscripts are rejected by TypeScript
with TS2538. For finite mapped declarations, `as` accepts the original
key, `never`, a fixed string or a template with one direct interpolation
of `K` or `K & (string | number)`.
Renamed properties retain the source types. If several source properties
share a replacement name, completion shows the members common to their
value types. Remapped keys also work with structural `keyof`, the supported
utility types and object destructuring. Multiple interpolations, intrinsic
string transformations, general key domains, key-dependent values and
conditional types remain open.
Bracket access also follows visible local constants and constant aliases
initialized from a supported string or numeric literal. Mutable
variables, function parameters and unresolved definitions block fallback
to an outer key of the same name. Constant expressions in declaration
patterns and general runtime key inference remain open.
JSON snippets can load a local schema and save its snapshot in the
native snippet library. Reopening the snippet uses that saved snapshot.
The schema file can be reloaded to update the snapshot. The
same list uses nested properties, array items, enumerations, constants,
defaults, examples, composition branches, and references within the
loaded schema document. The native JSON syntax tool reports parse errors
with line and column and reports mismatches against an attached schema
with instance paths. It applies the loaded schema offline. Diagnostics
with source locations underline the relevant token and can reveal it from
the report. Editing the source or replacing its schema clears stale
results. When loading a schema, an explicit option includes referenced
documents from the selected schema folder and its subfolders. The native
library keeps the loaded resources with the root schema. Completion and
validation use the saved resources offline. Remote schema retrieval and
broader schema associations remain open. Java-compatible
snippet interchange continues to use its existing
fields. Verify these controls,
rendering with long lines, and configured AI providers on native
desktops. CSS suggestions include compact offline MDN data for property
names, explicit value keywords, at-rules, pseudo-classes, pseudo-elements,
and rule-specific descriptors. Completion follows nested style and group
rules, keeps comment and string delimiters out of structural analysis,
and distinguishes custom property names from fallback values in var().
HTML supplements its common entries with
elements, global and element-specific attributes, and explicitly listed
values from the WHATWG index. Its HTML tag completion resumes after the
matching end tag of raw-text and escapable raw-text elements. The catalog
also offers concrete WAI-ARIA 1.2 roles, current state and property names,
and their listed keyword values. Embedded SVG and MathML use their own
element and attribute catalogs and selected keyword values. Completion
resumes the surrounding vocabulary after a closing tag and uses HTML
inside recognized integration points. Common optional HTML end tags
and foreign-content breakout tags update the completion stack. It also
distinguishes HTML void elements from foreign self-closing elements.
Broader attribute-value coverage, host-element and role-specific ARIA
rules, full adoption-agency and table foster-parenting behavior, and
more complete CSS value and selector context remain open.
[MDN CSS data](https://github.com/mdn/data/tree/7d81f7981cc1c7074b887bb2d16aab66b493dc13/css)
[CSS Nesting](https://www.w3.org/TR/css-nesting-1/)
[WHATWG HTML index](https://html.spec.whatwg.org/review-drafts/2026-07/#index)
[WAI-ARIA 1.2](https://www.w3.org/TR/2023/REC-wai-aria-1.2-20230606/)
[SVG 2 element index](https://www.w3.org/TR/2026/CR-SVG2-20261006/eltindex.html)
[MathML Core](https://www.w3.org/TR/2025/CR-mathml-core-20250624/)
[HTML parser](https://html.spec.whatwg.org/multipage/parsing.html)
[JSON Schema core](https://json-schema.org/draft/2020-12/json-schema-core)
[Java editor](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/SnippetEditDialog.java) · [Java completion behavior](https://github.com/chardonnay/korTTY/blob/v2.26.0/app-docs/site/docs/en/features/snippets.md#ai-code-completions)

## Session journals

Scheduler virtual-terminal screenshots are saved in the automation journal
after masking terminal text. Eligible closing passes can describe the original
saved frames with an approved local vision profile. Verify the workflow in the
native Scheduler and journal windows with a configured local vision model.
See [Scheduler screenshots and local vision](PORTING-PARITY.md#scheduler-screenshots-and-local-vision).

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| J1 | Implementation and acceptance. Per-connection automatic capture, opt-in typed input, XML settings, raw evidence, screenshots, redaction and retention exist. Managed policy can require automatic capture and lock manual stop. SSH journals span reconnects in the same tab. Automatic local-shell and SSH journals show effective isolation, and SSH reconnects record the replacement stream's status. [Serial journals](PORTING-PARITY.md#november-24-serial-session-journals) record masked output for ANSI/DEC parser profiles and continue when the same device reconnects. The editor, XML import and capture pipeline support Java's normal rotation bounds. | Add parsed journal output for the PETSCII, Wyse, TeleVideo and HP parser families. Verify the serial desktop flow and run hardware acceptance when an approved device is available. Verify managed limits on native desktops and audit values above the native 1,000-part limit. Verify native input-off/on, imported omitted/explicit values, command boundaries, duplicate/import/export and restart. Repeat SSH reconnects from the desktop and across the remaining native platforms. Preserve screenshot consent and the native typed-input privacy default. |
| J2 | Acceptance. Local/managed periodic and closing summaries, durable catch-up, re-evaluation, backfill and screenshot re-description exist. Command cards, help and failed-start filtering, the reviewed long-output mode, classified command counts in journal metadata, startup finalization of abandoned native interactive journals, reopenable progress details, and row/group status badges are implemented. F1 binds the HTTPS credential at preflight, streaming and retry. Reviewed HTTPS and CLI approvals bind the profile revision, journal root, source identity, redaction rules and output mode. OpenAI-compatible HTTPS summaries support server-selected models through the same approval checks. Live, entry and screenshot note editors offer separate reviewed HTTPS and CLI translation. The native journal viewer and compatible browsers for HTML bundles have a 40-pixel scrollbar thumb minimum. R `src/session_journal/summary_job.rs`, `src/workspace/journal_access/summaries.rs`. | Verify native close and count recovery, scheduling, cancellation, restart, pending deletion and configured HTTPS and CLI providers. Verify local vision, per-connection overrides, desktop progress lifecycle and native scrollbar dragging. |
| J3 | Acceptance. Reviewed local, HTTPS and CLI Q&A, lexical cross-journal search, reciprocal-rank fusion and exact capture/entry/chat citations exist. Saved HTTP loopback and reviewed OpenAI-compatible HTTPS profiles, including eligible loopback HTTPS, can use server-selected models for Q&A. F1 passes the reviewed HTTPS credential through question dispatch. Each question requires approval of the exact masked evidence and destination. The viewer and manager preselect an eligible saved HTTPS Journal role. Without a dedicated role, the global HTTPS default is used when no configured local answer takes precedence. Recording and Scheduler retain their separate summary approvals. Separately reviewed local semantic ranking reuses a private per-journal vector for the exact document, masked card and model. Stale and unavailable cache entries use fresh reviewed embeddings or lexical fallback. | Verify native citation routing, stale-source/policy rejection, recorded chats and configured providers. The CLI route uses one approved invocation and validates exact quotations from the reviewed evidence. The semantic scope follows the reviewed candidate shortlist. J `core/SessionJournalAskService.java`, `core/SessionJournalSemanticIndex.java`, R `src/session_journal/question/rank.rs`. |

See [HTTPS journal answers](PORTING-PARITY.md#reviewed-https-journal-answers-and-retained-scheduler-totals)
and [journal search and recovery](PORTING-PARITY.md#source-selection-managed-recovery-local-file-ownership-journal-search-and-mlx-removal).
The latter evidence distinguishes journal ranking from persistent knowledge-store indexing.

The shared summary parser checks paths, URLs and quoted commands against the
masked model input before publication. Raw evidence remains available when a
summary is unverifiable. Local closing summaries also use user notes and saved
screenshot descriptions. The approved HTTPS and CLI routes use masked terminal
evidence and summary cards. Summary generation, re-evaluation and resume use
the saved journal language. Reviewed HTTPS and CLI requests receive a
supported language code. Verify the closing workflows on native desktops.
The separate closing-title request uses the saved language and preserves an
existing custom title. Reviewed external profiles need consent for that
additional request. The wrap-up is durable before naming and can be read
while a title attempt is pending. Verify native title and managed rename
behavior with configured local, HTTPS and CLI profiles. Recognized masked
prompt evidence appears on native command cards and in the closing summary.
The startup connection test uses the effective approved journal profile.
Manual capture offers retry, recording without AI and cancellation.
Automatic recording begins immediately and offers retry, recording without
AI and ending the capture if the background check fails. Automation journals
keep the raw capture and record a failed check on their timeline. Recent
successes are reused for the same effective route and approval. Verify
desktop controls and configured local, HTTPS and CLI profiles natively.
Journal progress includes model work after the last command card, including
closing summaries, titles and approved image descriptions. It weights
command work by retained evidence size, plans subsequent journals in the
selected batch and reuses observed throughput for the same resolved model
profile within the application session. Native long-output processing measures
all model calls in a logical command together. Verify the progress surfaces
with the configured providers on native desktops and check accuracy when a
long command is split further according to the preceding response and token
budget.
See [scheduled AI Memory and journal fact checks](PORTING-PARITY.md#scheduled-ai-memory-and-journal-fact-checks).

## SSH, SFTP and serial

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| T1 | Acceptance. Hosted/detached workbench, parallel lanes, idle protection and transfer limits exist. Attached sessions use at most two transfer lanes. A managed cap of one locks the SFTP parallel preference and preserves the personal value. J `ui/SettingsDialog.java`, R `src/sftp_window/parallel.rs`. | Verify native large and colliding transfers, cap changes while lanes drain, idle settings, and hosted/detached behavior. |
| T2 | Implementation + acceptance. Durable single-file downloads/new uploads, reviewed replacement-upload resume, folder child retry and recovery exist. Settings and Java SFTP preference interchange include `sftpResumePartialTransfers` (default on) and dependent `sftpKeepPartialOnCancel` (default off). Turning resume off releases retained offers and history pins and changes waiting retries to fresh copies. The Local and Server panes offer reviewed cleanup of regular part files with active-stage protection. [SFTP cleanup parity](PORTING-PARITY.md#november-20-sftp-transfer-part-cleanup) | Verify the cleanup controls in native desktops and protected server cleanup on configured routes. Verify live Java preference exchange, native Linux/macOS interruption and restart, real parallel-lane retries and remote cancellation cleanup after a lost connection. Remote stage removal requires verification of the authenticated route and stage ownership. |
| T3 | Implementation + acceptance. Browser child-queue folder merge, selective retry, regular-file source links, changed-file batches, destination-only removal and same-server copy exist. Ordinary browser/Scheduler download inheritance and P1's transfer-policy separation are implemented. One-use Windows and Unix Scheduler downloads and ordinary login-owned private remote replacement uploads have reviewed publication, recovery and content-bound approval. Ordinary recurring Calendar and Chain SFTP replacement is implemented in both directions. Protected single-file replacement supports one-use and recurring approval; protected folder transfers support one-use and bounded recurring approval for existing or initially vacant receivers. | Verify extension absence without client-payload fallback, collisions, foreign-owner writes and changed destinations. Verify recurring first publication into vacant protected receivers through the installed helper and configured native routes; retain and verify the implemented file and existing-folder approvals. Ordinary recurring Calendar and Chain replacement already passed pinned OpenSSH fixtures; verify native approval and reapproval. Identify reference-required destinations outside the supported ownership, mode and extended-ACL boundaries and implement their metadata and recovery contract. |
| T4 | Implementation + acceptance. Native text and external file editing, browser **Open image**, Linux guarded elevated text writeback, and reviewed AI comparison with editable presets, three selectable draft comparison pairs and local/unsaved snippet drafts exist. A review can publish a new draft file to a chosen canonical directory on the same authenticated server connection. Local draft replacement reviews an existing target, can create a new `.bak`, and can replace the compared right local file and refresh the exact pair. The right server file or another selected existing server file can be replaced through the checked native Linux, macOS or Windows helper with optional `.bak` and preservation of supported access metadata. Replacing either compared file reloads the exact pair. Local and SFTP file editors offer reviewed AI workflows that use the originating saved connection profile when available. An explicit picker choice persists across actions. J `ui/SFTPManagerTab.java`, R `src/sftp_window/worker.rs`, `src/sftp_window/compare/ai.rs`, `crates/ssh/src/sftp/edit.rs`. | Verify the implemented AppKit Copy drag-out from the file manager and terminal sidebar, including cancellations, revoked transfers and staging lifetime. Verify native UI and configured providers, close/reconnect, bounded masked streaming, cancellation and publication. Verify the native macOS and Windows helpers via direct and isolated SSH. Retain Linux coverage. Verify the pair picker and existing-file chooser in the native window. Verify native image opening, editor launch/writeback/disconnect/recovery and elevated writeback with a real authorized sudo account. |
| T5 | Acceptance + bounded audit. Local PTY sandbox launch paths use Linux Landlock/seccomp, macOS Seatbelt and Windows Process Security Environments. Fedora ARM64 local-shell and worker containment, installed Flatpak local-shell containment, Windows ARM64 ConPTY containment, and native Apple Silicon Seatbelt and packaged SSH isolation passed. Windows uses CMD with Win32k lockdown and rejects the Windows PowerShell preset in sandbox mode. | Run local-shell and packaged SSH fixtures on native macOS Intel and Windows x64, including interactive behavior and containment. Track broader Windows shell compatibility. Direct, process-isolated and jump RSA passed on Apple Silicon. V6 still tracks the external review and active dependency advisories. See the [native Mac verification](target/verification/20261112-step6-macos.md). |
| T6 | Acceptance, hardware deferred. Saved serial connections, encodings, grouping/tags/favorites/pins, mixed library actions and import/export exist. | Native Manager selection/actions/interchange. Physical break/modem/flow-control tests remain user-deferred under V4. Serial library organization is no longer an implementation gap. |

Java drag transfers and global archive defaults are in `ui/SFTPManagerTab.java`,
`ui/TerminalRemoteSidebar.java` and `ui/SettingsDialog.java`. The native queue,
archive dialog and transfer preferences live in `src/sftp_window.rs`,
`src/sftp_window/archive.rs` and `src/workspace/settings_window/sftp_settings.rs`.

Java terminal-surface drops use `ui/TerminalView.java`. Text goes to the
exact pane and uses paste protection. A file drop uses the pane's open
SSH route and checks the current shell identity before starting SFTP.
On native Wayland, Rust now routes UTF-8 text offers to the pane under
the pointer through its paste protection. The imported
`terminalDragDropEnabled` preference and the Terminal setting gate
admission. Internal clipboard policy, mirrored input and stale terminal
streams block text drops. Verify compositor interaction with an external
source, tab and stream changes, and copy-only negotiation. Native Wayland
file offers now open a terminal upload review on the exact SSH split.
The review uses the canonical SFTP directory reached from the tracked shell
directory or the SFTP start directory. It binds the stream and shell
context until completion and uses the existing upload conflict controls.
Verify compositor delivery, folder uploads, cancellation and reconnection
on deployed Wayland routes. Windows, macOS and X11 use native file-drop
coordinates to select the terminal pane and send file batches to the
same reviewed uploader. Verify the real desktop gestures from Explorer,
Finder and an X11 file manager, including multiple files and drops outside
the terminal. AppKit accepts external plain-text Copy offers and uses the
same terminal target binding and paste protection as Wayland. File offers
retain the existing AppKit file route. Verify text drops from a native
macOS source into an inactive split, policy changes and no drop over tool
panels. Windows uses a shared OLE drop target for Unicode text and files.
The native client coordinates drive the shared terminal target checks and
paste protection. File offers still emit Iced file events. Verify live
text and file drops from Windows desktop sources across DPI settings,
including inactive terminal splits and drops over tool panels. X11 uses
an Xdnd proxy to receive Unicode text and file offers. Text transfers begin
on drop and follow the shared pane, stream, clipboard and paste checks.
The live GTK 4 fixture reached the intended inactive split and left drops
over the application chrome unprocessed on X11 at 100 and 200 percent
scale and on Mutter Xwayland at 100 percent. Continue live acceptance with
desktop text selections and file managers, including application zoom,
tool overlays, and the Wayland-to-Xwayland path.
Keep terminal-surface routing independent of drops on the SFTP Manager
and the remote-files sidebar.

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
metadata, exact paths and the saved SSH route. Unix one-use downloads and
ordinary private login-owned remote replacements now have reviewed publication
and recovery paths. Their supported cases require protected parents, supported
ownership/access metadata and the applicable server extensions. General SFTP v3
rename has no conditional comparison against the reviewed object.

Scheduled ordinary remote replacement requires a readable login-owned private
file without special mode bits. Unix replacement requires preservable UID/GID
and rejects unsupported extended access ACLs. Browser foreign-owner in-place
replacement is already implemented with its documented partial-visibility
contract. Identify the remaining Java-required destination cases individually
before extending these contracts. Protected single-file replacement and
protected folder transfers support both sudo modes and one-use or bounded
recurring approval. `Copy::validate_job_schedule` admits an enabled calendar
or a Chain with predecessors. Each protected folder run checks the bound
receiver, and an unconfirmed publication requires review before another
firing. The deployed helper and native Scheduler still need this acceptance
case. [Schedule validation](src/scheduler_window/sync.rs) ·
[Standing approval](src/scheduler_window/sync/standing.rs)

Initially vacant recurring rsync downloads of a single file per saved route
review the vacant name and its existing local parent. The first successful
Calendar or Chain firing creates that route's file with no-replace publication
and stores the exact successor for subsequent replacements. Grouped reviews can
also bind existing files, so the same job can be renewed after partial success.
Each route advances independently within the shared firing budget. The review
rejects overlapping physical destinations and names in the same directory that
the supported filesystem lookup checks cannot distinguish. An armed but
unconfirmed publication requires review before another firing.
Fedora ARM64 verified the grouped Calendar and protected Chain workflows over
pinned OpenSSH, including actual passwordless sudo, partial progress, renewal
of mixed existing and vacant routes and the shared per-firing byte budget.
Windows ARM64 verified independent grouped Calendar progression and recovery
over pinned OpenSSH alongside the direct-file regressions.

Direct file review can bind an absent file and its missing parent folders
to a checked ancestor. korTTY verifies and syncs the file inside a private
hierarchy before publishing the first absent component with no-replace
native rename. Grouped targets can reuse confirmed shared parents. A firing
counts the file and only the parents it creates. The first receipt binds the
original vacancy to the exact published file, which is retained for recovery
on replacement. Explicit one-use review can create a vacant file through the
same path. Portable restore keeps the option and requires fresh approval.

Recurring mixed-source downloads can also review a vacant local directory
name in an existing checked parent. The first authorized firing publishes the
verified projected tree without overwriting an arrival and records its exact
identity, contents and metadata for subsequent replacements. The plan keeps
named and contents placement and the corresponding delete scope, including
Java imports with one directory source. Each grouped route can begin with a
vacant or existing tree and has its own receipt within the shared firing
budget. Group review rejects alternate Linux mount views of the same volume
when they could hide nested destinations. Fedora ARM64 verified named,
contents and mixed projections through pinned OpenSSH Calendar and actual
passwordless sudo Chain firings. Windows ARM64 verified a vacant contents
receiver's refusal, creation, replacement and recovery over pinned OpenSSH.

Direct directory downloads use the same checked publisher for first creation
and later replacement. The effective receiver is either the configured path or
the named source folder below it. The review binds this path and its mirror
scope independently for each selected route. One-use jobs use the exact local
vacancy and current approval. Calendar and Chain jobs also require a current
firing lease before their first publication and use the confirmed receiver on
following runs. Fedora ARM64 verified direct named and contents downloads over
pinned OpenSSH and actual passwordless sudo, including mirror removal and
recovery. An ordinary one-use run also verified refusal of a repeated vacancy
approval. Windows ARM64 verified direct named and contents placement, initial
publication, recovery and preservation of a named destination's sibling over
pinned OpenSSH. When the direct or recurring mixed receiver has missing
parents, review binds the first absent component and exact target to a held
ancestor. First publication stages the complete missing hierarchy privately
and installs it with one no-replace operation. The entry limit counts new
parents in addition to the first transfer. The next firing retains the
confirmed leaf as a recovery copy and limits changes to that leaf. The
first confirmed route records the shared parent components it created.
Later routes verify those identities along their originally reviewed path
before using the parents. A firing charges a parent in its entry budget
only when a route creates it.
One-use jobs retain receipts for the batch. Calendar and Chain jobs store
them alongside the confirmed receiver until all dependent destinations have
been published. The mixed-source editor supports explicit one-use review of
vacant paths. Java maps uploads to the saved target on each route and leaves
its parent hierarchy to the server. The protected regular-file upload
accepts an explicit recurring approval for a vacant target in a checked
existing parent. The helper reviews both create and replacement scope and
returns the exact held receiver after no-replace publication. The next firing
matches that receiver and keeps the replaced file in recovery. The same
transition is implemented for protected direct and mixed trees, preserving
named and contents placement and the reviewed mirror scope. Integrated
Fedora Calendar and Chain tests passed through actual sudo and pinned SSH.
The ordinary Linux v3 helper and Scheduler now have the same reviewed
first-publication transition for files, direct and mixed trees. Native Fedora
Calendar and Chain cases passed through pinned OpenSSH, including the exact
successor and recovery. Complete native desktop review and verify configured
deployments on the remaining platforms. See
[protected file creation](PORTING-PARITY.md#scheduler-recurring-passwordless-rsync-to-a-vacant-server-file)
[protected folder creation](PORTING-PARITY.md#scheduler-recurring-passwordless-rsync-to-a-vacant-server-folder)
and [ordinary receivers](PORTING-PARITY.md#scheduler-recurring-ordinary-rsync-to-reviewed-server-receivers).

Archive formats, compression, exclusions, password ZIP and optional local
copies are implemented. The ordinary Linux Scheduler route has opt-in reuse
of the configured archive paths. Each firing binds the prior receiver and
source inventory to that publication. ZIP keeps unmatched members and
replaces matching paths. TAR, TAR.BZ2 and 7z use the current sources. Both
server and optional local replacement retain the displaced file for
recovery. Fedora passed the native helper checks and two successive
scheduled firings through pinned OpenSSH with plain and vault-protected
ZIP. The sudo update helper and Scheduler route are implemented for both
modes. Replacement requires an explicit administrator replacement root
and a distinct Scheduler approval. The native helper passed real sudo and
PAM checks in separate Fedora fixtures. The pinned OpenSSH application
transport passed for both modes and preserved confirmed publication after
a refused or interrupted copy.
Imported Java ZIP, password ZIP, TAR and TAR.BZ2 jobs preserve the
server-path member hierarchy through explicit adoption and can opt into
checked reuse. Fedora passed the helper tests and pinned OpenSSH cases
with archives created by the Java Info-ZIP and GNU tar commands. The
regressions covered disjoint paths with the same basename, disabled backup
restoration, guarded publication and exact recovery bytes. Verify the
installed helper against configured desktop Scheduler routes.
Sources are J `jobscheduler/JobSchedulerArchiveCommandBuilder.java` and
`jobscheduler/JobSchedulerJobRunner.java`, R
`src/job_executor/remote/server_archive.rs` and
`crates/server-archive/src/server/create/update.rs`.

Java's privileged file staging applies when both sudo and staging are
enabled. Its scheduled Sync action dispatches the same file upload or
download as its standalone actions. Rust supports vacant destinations and
reviewed file replacement in both directions and both sudo modes, with a
1 GiB limit for each regular file. An individual replacement approval binds
the destination's identity, contents and metadata to the pinned SSH route and
sudo mode. For an upload it also binds the exact payload and Linux replacement
roots. Recurring replacement approvals bind the same paths and route to an
enabled calendar or Chain and to explicit duration, firing and byte limits.
Each firing reads the current file at the approved source path. It retains the
original and advances the receiver fingerprint only after confirmed publication.
Java staging actions restore as disabled exact-route drafts for new files,
reviewed file replacement, new folders and merges into existing folders.
One-use privileged folder downloads and uploads add missing entries and
replace reviewed files in an existing destination. They preserve receiver-only
entries and retain replaced originals. Downloads recheck local file
fingerprints, the pinned route and current policy. Uploads use the installed
Linux folder helper to bind the server directory scope, destination and
replacement roots, receiver fingerprints and exact payloads. The helper
verifies the private stages before the first publication and rechecks server
authority for each change. Check deployment and recovery through a configured
desktop route and extend metadata coverage in the native release matrix.
The references are J `jobscheduler/JobSchedulerJobRunner.java` and R
`src/job_executor/remote/privileged.rs`, `src/scheduler_window/privileged.rs`
`crates/server-archive/src/server/privileged_rsync/tree_merge.rs`,
`crates/server-archive/src/server/privileged_rsync/file_replace.rs` and
`src/profile_backup/java_scheduler.rs`.

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

Reviewed scheduled Agent and Swarm requests include enabled organization,
global and exact saved-connection memory. The review and dispatch check
the masked memory and its source revisions. One-use proposals send
separate requests when connection memory differs by target. Verify the
workflow with configured profiles in the native Scheduler windows.
The Java reference is `jobscheduler/JobSchedulerAiSupport.java` and
`core/swarm/SwarmOrchestrator.java`.
The Java Scheduler backup importer retains compatible virtual terminal
settings for disabled, single-target command and snippet jobs without sudo.
The native Scheduler validates the restored options. Verify the restored
configuration and its PTY replay in the native Scheduler.
The Scheduler records separate masked stdout and stderr for ordinary SSH
commands, inline scripts and snippets. Java imports retain the output mode.
The run detail view and search expose both streams with independent fields.
Verify the output controls and journal detail view in native Scheduler windows.
Run details are selectable and copy through the application clipboard policy.
Verify native keyboard, context menu and managed clipboard behavior. Assess
Agent and native-tool outputs against their own review scopes.

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| Q1 | Implementation + acceptance. The job editor supports searchable multi-selection of saved SSH connections and source-qualified groups. A batch rechecks the selected routes, expands group descendants and removes duplicate targets before changing the draft. [Scheduler picker parity](PORTING-PARITY.md#november-20-scheduler-target-picker) Missing-only SFTP sync, one-use/standing/cascade/predecessor reviews, multi-target Agent/Swarm, per-round approval, comparison and saved follow-up exist. Read-only and iterative scheduled Swarms admit up to sixteen reviewed target workflows and queue the remainder of up to 32 routes. Windows and Unix have a one-use reviewed changed-file download path. Ordinary uploads can replace reviewed private files owned by the remote login. Recurring approval can explicitly authorize replacements in either direction within the firing budget. rsync supports reviewed regular-file and directory transfer on Unix and Windows, including literal batches and mixed file and folder projections. Each combined route stages and verifies one tree with named or contents placement, scoped mirror deletion and recovery. Existing downloads support protected local folders and regular files on Linux, macOS and Windows. Direct file replacement supports one-use and recurring approval, with explicit distinct local paths for grouped routes. Recurring single and mixed existing-directory plans bind calendar and Chain occurrences, a reviewed receiver per route, and aggregate byte, entry and removal limits. Recurring Linux uploads use the protected v3 helper. Passwordless elevation uses a fixed Linux helper for direct and combined transfers, including guarded existing-directory upload and download. | Verify Teamwork scheduler and rsync jobs over native OpenSSH. Verify protected operations on shared routes and add shared-route support to Agent/Swarm approvals. Verify native recovery of abandoned private rsync staging after forced helper termination. Verify native larger Swarm runs over pinned SSH with configured providers, cancellation, journal/report/reopen and saved group refresh and reapproval after membership changes. Verify the implemented recurring file replacement paths on native desktops. Verify the implemented recurring vacant-folder progression under T3 through deployed native routes and address reference-required remote destinations outside the supported upload scope. Fedora verified ordinary SFTP Calendar and Chain starts through pinned OpenSSH. Verify configured deployments and remaining native editor paths for combined passwordless transfers. Passwordless standing rsync covers existing protected remote files and remote or local directories. Fedora verified recurring direct-file downloads with ordinary and passwordless pinned OpenSSH. Windows verified the one-use direct-file receiver over pinned OpenSSH. Confirm native approval and recovery and verify the remaining platform cases. Fedora verified first publication and replacement at vacant Linux server file and directory receivers through ordinary and passwordless routes. Verify disjoint and shared file and directory hierarchies and imported group layout through native Scheduler review and adoption. Verify merge, mirror and recurring approval through native desktop interaction. Verify deletion scope, recurrence, cascade and expiry, groups and batches on Windows and native macOS. |
| Q2 | Implementation + acceptance. Elevated command/script/snippet, reviewed new-file/new-tree transfers, one-use and bounded recurring protected file replacement in both directions, one-use and recurring protected folder transfers with confirmed vacant-to-existing receiver progression, desktop outcomes and independently approved HTTPS webhooks exist. Typed Linux sudo actions cover mkdir, octal and explicit-class symbolic chmod, numeric chown, regular-file and recursive folder copy to vacant paths, no-replace file/folder rename and protected deletion. The Scheduler can resolve names through a bounded read-only command on one pinned saved route. Explicit acceptance stores the UID or GID. Recursive operations support up to 128 total entries. The browser and Scheduler support server-only ZIP, TAR, TAR.BZ2 and 7z creation with reviewed output UID, GID and ordinary mode through the updated helper. Ordinary Scheduler jobs can create vault-backed AES-256 ZIP and verify and publish an optional local copy using the same SSH session. An opt-in Linux workflow supports ordinary same-path archive updates, ZIP Add, fresh TAR output and recovery for the remote and local files. A separately installed Linux helper supports these archive formats and AES-256 ZIP through a path-constrained Scheduler sudo route with either an explicit passwordless review or a separately reviewed vault password. An administrator download root can permit a local copy of its newly published archive through the same command channel. | Ordinary SFTP symbolic permissions require a canonical path and the server's no-follow `lsetstat` extension. Verify symbolically computed ordinary modes through the descriptor-held sudo path with an authorized SSH route. Verify recurring vacant-folder progression, existing-folder standing approval, durable pending state and native recovery through the installed helper and native Scheduler windows. Verify the installed folder helper through a configured native Scheduler route. Verify recurring file replacements and reapproval from native Scheduler windows against a deployed helper. Verify native adoption of restored Java sudo staging drafts. Fedora verified ordinary and sudo client update fixtures and the Java archive layouts. Verify a configured desktop route with the installed helper. Verify the optional protected archive copy with pinned SSH, configured download roots, revocation and local publication. The native pinned client and the installed V2 helper passed separate Fedora checks, including actual sudo/PAM in an isolated test account. Verify deployment on a saved desktop route. Verify archive output metadata through the installed helper and live pinned SSH. The privileged route retains root-owned private output, consistent with Java's archive sudo controls. Verify named ownership over a live native SSH route, the installed archive helper under transfer denial, real authorized sudo, Windows/macOS publication, configured webhooks and live desktop delivery. J `jobscheduler/JobSchedulerJobRunner.java`, R `src/job_executor/remote/privileged.rs`. |
| Q3 | Acceptance. Per-target Session Journals, capture revocation independent of execution, closing summaries, retention, estimates/totals, result links and Java retention-preference XML exist. | Native viewer/details/estimate/options and import/export dialogs, actual run/provider outcomes and target lifecycle. Verify a successful scheduled command under installed capture denial while existing retention continues. Java JAXB 0/37/3650 round trips already exist. |
| P1 | Acceptance + bounded audit. Managed Control activation, SFTP separation, update feed, encrypted recovery output, scoped AI restrictions, protected/provisioned models, custom headers, automation capture and managed SFTP conflict defaults exist. Startup supports managed and personal password requirements and optional device-local vault unlock. The shared `allow-telemetry` Boolean is accepted for compatibility. Scoped `rule.ai-memory.statistics` controls local collection. | Verify the [device credential flow](PORTING-PARITY.md#november-23-native-vault-startup-unlock) on each native desktop. Verify the stricter host-pin lock contract, native worker enforcement, scoped precedence and revocation, SMB sources, signed protected inference and macOS protected-cache restart. See the [field audit](target/verification/20261112-step5-policy-audit.md) for the earlier snapshot. |
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

## Java data migration

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| M1 | Implementation + acceptance. Java password ZIP/GnuPG backups add supported connections, reviewed SSH credentials/host pins, snippets and diagrams, themes, environments, projects, AI chats, read-only Swarm archives, global AI Memory files, and exactly matched new SSH, local shell, and serial Memory scopes. Validated historical snippet and folder analyses restore additively with exact library binding and current managed retention. Supported AI profiles restore as unreviewed drafts, including Auto and server default, embedded model references and inactive Toolpack hints. GGUF and MLX registry hints can prefill a separately reviewed native registration. RAG definitions restore as inactive store and source hints. The user reviews the native backend and index sources. Command/snippet/Agent/Swarm, supported remote operations, sudo commands and snippets, privileged file actions, ordinary and staged transfers, and ordinary and privileged archives restore as disabled Scheduler jobs with exact source-group matching. Journal, default, text, coding, Agent and snippet security profile suggestions require adoption, and imported deletions require exact-path confirmation. Java OpenPGP references require a matching local public certificate and independent fingerprint confirmation. | Verify the native review of Java custom OS symbol assignments, including missing images and colliding IDs. [OS symbol assignment parity](PORTING-PARITY.md#november-20-java-os-symbol-assignments) Continue field-by-field global setting adoption. Verify AI Memory destination review and collision reporting on macOS and Linux desktops. Windows ARM64 passed the native serial assignment, omission reporting and restart fixture. Verify serial AI Memory in a native desktop terminal session. Verify read-only imported code comparisons and preserved diagrams in a restored desktop profile. Verify historical records for custom and deleted Java skills in the native report and on cross-host restore. Verify native role, model-settings and Knowledge Store adoption, explicit profile selection across actions and saved connection overrides in the file editor. Java Scheduler rsync restores as disabled native-action drafts with original options retained for review. Extend adoption to additional native rsync behaviors as they become available and review remaining execution and credential fields. Archives, sudo staging, sudo file actions and sudo shell actions with virtual terminals split into individual exact-route jobs when required. Verify native adoption and firing in both sudo modes. Include the installed helper, local destinations and ZIP credentials where applicable. Verify conflict handling, stable IDs, source provenance, omission reporting and restart replay on native desktops and another host. Keep endpoint, file, credential and execution authority subject to separate review. |

### Remaining field and identity audit

The additive importer handles individual fields. The November 18 check
compared the 346 top-level non-static fields in the tagged
`GlobalSettings.java` with the 42 readers in the
[November 17 inventory](target/verification/20261117-java-global-field-triage.json)
and the six additional Rust modules in `src/workspace/backup_data/`.
The check looked for each exact field name as a quoted string before the
first `#[cfg(test)]` in each of the 48 files. It found 197 names and left
149 for inspection through other readers, aliases, omissions and product
decisions. For example, `aiSkillsEnabled` has a working delegated import.
A literal match also requires checking its behavior. The figures measure
search coverage.

- Java `isolationMode` distinguishes inheritance from explicit `none`.
  The backup importer maps `connectionGroupIsolation` to exact new folders
  and stages other entries for review. `connectionIsolationDefault` and
  `temporarySshKeyEnabled` become Security page proposals. Existing native
  choices retain their authority, and the managed minimum applies at
  launch. Native fixtures cover restore and replay of these settings.
  Windows ARM64 verified the managed display and the blocked below-minimum
  proposal. Verify Save and restart for eligible proposals on the native
  desktops. The encrypted key material has a separate
  [custody task](#confirmed-implementation-work).
  [Java backup security checkpoint](PORTING-PARITY.md#november-23-java-backup-security-settings)
- Java restore maps `maxBackupCount` and `autoBackupInterval`. At desktop
  startup these values initialize absent native backup settings. Existing
  settings take precedence. Automatic execution requires a native
  destination and credential configuration. Verify the restored settings
  in the desktop after restart.
  [Java backup settings](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/model/GlobalSettings.java) · [Native preferences](src/profile_backup/preferences.rs)
- ASCII Art applies restored `asciiArtPictureSize`, `asciiArtImageSize`,
  `asciiArtCopyCommentStyle`, `asciiArtCopyPrintStyle` and `asciiArtCopyGap`
  when the dialog opens. Saved native values take precedence. The import
  uses Java's print-style precedence when both imported formats are active.
  Verify the restored values and copied output in the native desktop.
  [Java controls](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/AsciiArtBannerDialog.java) · [Native preferences](src/workspace/ascii_art.rs)
- Java restore resolves `snippetWorkspaceOpenTabs` and
  `snippetWorkspaceActiveTab` against the merged snippet library. Exact
  matches can have different native IDs. Missing or divergent records are
  skipped, and any existing native session takes precedence. Verify restored
  tabs in native desktop windows. The global
  `snippetWorkspaceLibraryDividerPosition` fills an unset Rust library-divider
  preference. The deprecated manager preview divider has a separate disposition
  below. [Native tab state](src/snippet_window/session.rs)
- The snippet library now has resizable Name, Language, Category,
  Operating System, Tags, Executable, Favorite, Lines, Modified and Uses
  columns. The header remains visible during vertical scrolling. A
  horizontal scrollbar makes the
  full row available in a narrow library pane. Widths save on drag
  completion and the Java `snippetManagerColumnWidths` field fills
  individual unset native widths. Private Java library migration preserves
  favorites, use counts and creation, modification and last-used timestamps.
  The Lines column follows Java line endings. Resolved saved-snippet copies
  and accepted terminal input update the usage count. Incognito sends
  leave usage history untouched. The default order puts favorites first
  and then sorts by descending usage and name. Usage refreshes preserve existing
  visible row positions in default order. Headers cycle through ascending,
  descending and default order. A toolbar or row context action can toggle
  all marked snippets in one save. The analysis status column and filters
  show open findings, stale reports and pending reviews. Status uses the
  latest report against the saved source and surfaces older pending
  reviews and retained unsaved results. A successful native snippet save
  records matching accepted runs with the source, so their saved status
  survives source-history retention. Legacy reports can also use current
  or retained source versions as evidence.
  Verify filtering, sorting, favorite toggling, usage updates, resize, scroll,
  selection, row menus and drag interactions on native desktops.
- The snippet library can insert a resolved snippet at the caret of a
  hosted file editor or a native editor opened from its main window.
  The variable prompt keeps the chosen target and uses `${cursor}` to
  place the caret. Verify the desktop gesture and focus transfer.
  The separate terminal action edits declared variables and passes one
  positional argument per nonblank line to Bash, Python, Perl or Ruby
  through the existing POSIX terminal route. It binds the target pane
  before review and records one use after terminal input is accepted.
  Verify the dialog and delivery in a native POSIX terminal and an SSH pane.
  [Java library actions](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/ui/SnippetLibraryPane.java)
- Java `workflowHeaderDefaults` fills unset workflow script header defaults
  for the eight generation languages. Each reference requires an exact
  backed Script-Header snippet in the installed catalogue. Restore can
  map an identical native copy to its current ID. Existing language choices,
  including managed references, take precedence. Language mismatches and
  policy-disabled custom headers are excluded. Verify the chooser and
  generated draft on native desktops.
- Java `aiRequestTimeoutMinutes` imports values from 0 to 1,440 minutes.
  Zero disables the profile request timeout. Connection setup, managed
  model startup, provider metadata probes, explicitly bounded workflows
  and the CLI process ceiling retain their limits.
  Java `aiCodeTextDefaultLanguage` imports codes
  available in AI Manager, including Auto. Each field fills only an unset
  native value. The import preview reports unsupported values. Verify
  indefinite HTTP requests, cancellation and inherited timeouts on the
  remaining native platforms with configured providers.
- Java `preventSystemSleep` fills the unset native power preference.
  On Windows and macOS, opted-in sleep prevention observes active model
  completions, configured CLI requests and embedding requests alongside
  terminal and Scheduler activity. Overlapping requests keep the work
  signal active through the final completion. Verify the OS assertion
  and release with real requests on native desktops.
- The snippet editor respects `aiSnippetEditorAdditionalInstructionsEnabled`
  for assistance, translation, analysis, apply, verification, and
  modularization. Java restore fills the preference when the local value
  is unset. Verify the setting and reviewed requests in native desktop
  windows with a configured provider.
- The alternatives, AI diff, syntax review, description draft and standalone
  security report have independent font controls. Java restore fills unset
  `aiAlternativesFontSize`, `aiDiffFontSize`, `aiReviewFontSize`,
  `aiDescribeFontSize` and `securityReportFontSize` preferences within each
  view's bounds. New AI Chat tabs and recorded chat transcripts start with
  the saved `aiResultFontSize`, defaulting to 13 px. Private chat zoom applies
  for the life of that chat.
  An alternative can be expanded, copied by selection, and opened in the
  existing diff approval flow. Apply proposals and generated modules share
  the AI diff font. The diff and review surfaces support the Java zoom
  shortcuts. Verify preview interaction, chat and review zoom, and
  persistence after reopening on native desktops with a configured provider.
- The Agent activity panel and plan view have independent zoom controls
  and 13 px defaults. Restore fills unset `terminalAgentPanelFontSize`
  (10–20 px) and `terminalAgentPlanFontSize` (10–22 px), including
  fractional values. The activity panel uses its saved size when the
  remember option is on. Restore uses the Java
  `terminalAgentRememberPanelLayout` flag for that option. Plan zoom
  also accepts the primary modifier with plus or minus. Private runs
  keep zoom changes in memory. Verify both views, dock and floating
  layouts, keyboard focus and restart persistence on native desktops.
- The bottom Agent activity dock supports vertical dragging and a
  remembered height. It requests a 230 px default, enforces a 130 px
  drag minimum and fits the panel to leave room for the terminal.
  Java backup restore fills unset height, initial collapse and expand-all
  options. Startup collapse applies to the main, bottom and floating
  activity surfaces. Expansion exposes hidden entries in selectable
  histories. Verify the controls on reopening and on smaller screens.
- Quick Connect remembers its expanded Connection settings, Terminal
  Appearance, Terminal log and Terminal Effect sections. Java restore
  fills unset `quickConnectExpandedSections` and last effect speed.
  The effect picker applies an available effect and fractional speed
  to new terminal sessions and can save them with new SSH and serial
  profiles. Connection Manager can edit effects on saved local shell and
  serial profiles. The korTTY XML interchange preserves both with their
  fractional speeds. An explicit quick launch choice takes precedence over
  the saved or source pane effect. Verify the rendered effects and pinned
  SSH launches on native desktops. The separate Quick Connect AI section
  selects an available profile and connection skills for the new pane.
  New SSH and serial profiles can save the selection. Verify desktop
  selection, reopening, and the reviewed request with a configured provider.
- Quick Connect offers theme, font family and size, foreground and background
  colors, and the terminal-color switch. It remembers the last appearance
  and imports the supported fields of Java `lastQuickConnectTerminalSettings`
  when the Rust preference is unset. Supported appearance values can travel
  with new SSH and serial profiles. Verify rendered colors, font controls
  and reopening on native desktops.
- Java `aiDiffDialogSummaryDividerPosition` fills the unset native
  summary divider. Alternative and migration reviews and staged snippet
  and folder applies share the position. Reviews with a summary and a
  comparison have a vertical resize grip. The initial height follows
  the amount of summary content within the available space. Completing
  a drag saves the preference. Verify dragging, focus and reopening on
  native desktops.
- Java application log retention and the update check interval now fill
  individually unset native fields using the pinned Java bounds. A saved
  disabled update preference can fill an unset field. A saved enabled
  preference is shown in Rust update settings for review and requires the
  existing native opt-in and Save. Java log folder paths require selection on
  the current machine. Old update timestamps and prompt state belong to the
  Java release channel. Java telemetry consent does not opt in to Rust
  telemetry. Verify the displayed preferences in the native settings window.
- The six Teamwork global fields now restore as inactive suggestions. Teamwork
  Settings can copy one proposed source at a time into the native editor as a
  disabled entry, retaining its Java source ID in the proposal. The editor
  checks duplicate source origins and requires a valid native path or Git URL
  before saving. Apply the proposed default interval explicitly. The Java
  credential ID, key ID, username and temporary-key preference are visible in
  the Teamwork view of Connection Manager. Its Java mapping picker offers local
  vault passwords and registered keys by the imported method, and carries the
  Java username into a selected key draft. The existing Apply step checks the
  current local choices and saved state before committing. Verify restore,
  source activation and authentication mapping on native desktops.
  Interactive SSH and SFTP can now retain password, key and temporary-key
  defaults together. They select the first available method in that order
  before opening the login form. The native password must cover the selected
  endpoint. Existing single-method defaults remain readable. Verify the
  configured fallback and vault-unlock interaction on native desktops.
  The Rust Scheduler accepts explicit Teamwork routes and groups including
  their descendants. Shared groups are scoped to a single source. Local jobs
  retain their numeric connection IDs and string group paths. A shared job
  binds its source origin, external ID, exact group, route, effective login,
  local authentication reference and approved host-key pin. Enabled shared
  files are re-read at firing, and Git sources use the last synchronized
  catalog. Hidden routes and changed routes, membership or pins block the
  saved selection. Eligible scoped vault passwords take precedence over
  registered keys. Temporary keys remain interactive.
  Commands, scripts, snippets and SFTP operations including folder sync use
  the new target resolution. For a shared jump host, save one direct local
  SSH profile with the same host, port, username and authentication method.
  The scheduler binds that profile's unattended login and both host-key
  pins. Re-select the route after a login or host-key change. Native OpenSSH
  acceptance for this route remains open. Rsync review and firing bind the
  complete shared route, with revalidation before connection and protected
  publication.
  Protected files, folders, archives and metadata actions use the Teamwork
  route in their existing single-target review and execution paths. A
  Teamwork target can use a vault sudo password scoped to its exact SSH
  account. Local connection-group sudo fallback applies to local targets.
  Ownership lookup reviews the shared route and pinned host key. Verify
  these paths with the native protected helpers. Agent/Swarm still needs
  shared-route approval support. Java backup restore excludes rows marked
  as Teamwork from local profiles and credential bindings. The source group
  inventory retains these rows so restore cannot silently shorten a group
  to its local members. Java Scheduler restore can match source-marked
  rows to active native Teamwork sources using the backed source ID and
  configured origin, exact original SSH endpoints, group, current local
  authentication and pinned host key. Mixed groups preserve Java target
  order after an exact membership check. Managed server and isolation
  restrictions apply during target matching. The import stages disabled
  jobs and rechecks the shared routes before starting publication.
  Imported archive and sudo drafts can be adopted on the exact reviewed
  Teamwork routes. Existing native action reviews still apply. Verify
  native restore using both local authentication methods. Java backups
  without usable source and connection metadata still require manual
  Scheduler target selection. Verify the implemented paths on native desktops
  against pinned OpenSSH.
  `TeamworkSourceConfig.readOnly` is documented as informational in Java and
  is not a missing write-back feature. Its `policyManaged` field is transient
  and must not become policy authority through restore. The wider global-field
  inventory remains open.
- Java `aiSkillsEnabled`, `aiSkillAutoDetectionEnabled`, `aiSkillAutoCreate`
  and `aiSkillStatisticsEnabled` now import as four independent, inactive
  suggestions for the existing native Skills switches. Explicit adoption
  retains the automatic-selection confirmation, managed learning/statistics
  overrides and read-only restrictions; normal Save establishes runtime use.
  Additive replay preserves earlier hints and native preferences. Invalid or
  duplicate fields are reported separately. Apple Silicon and Fedora ARM64
  passed the three new fixtures, 340 AI Manager tests, 147 profile-backup tests
  and 246 Agent tests. Windows ARM64 application/test Clippy passed. The
  [four-field semantic audit](target/verification/20261117-java-skill-field-audit.json)
  records the exact mappings and pinned reference. Native desktop/cross-host
  adoption and complete field-by-field reconciliation remain open. See the
  skill-preferences entry in PORTING-PARITY.md.
- `terminalAgentConfirmMutatingCommandSets` now imports as an inactive hint and
  can be adopted explicitly in AI Manager. The native preference defaults to
  false, matching Java, and controls whether a command review can grant
  continuation for future ordinary commands in the same run/target. Standalone
  and per-target Swarm reviews expose that explicit action. Grants bind the
  current profile revision and saved settings, and require managed Allow mode.
  Pause, failure, elevation requests, profile/settings changes and run closure
  end the grant. Background dispatch checks it again before starting a command.
  Windows ARM64 Clippy and targeted native Apple Silicon/Fedora Agent,
  scheduler and AI Manager tests passed; see the confirmation entry in
  PORTING-PARITY.md. Native desktop interaction and live SSH acceptance remain
  open. Automatic read-only dispatch now uses Java's command-shape checks plus native
  command-word checks after an explicitly reviewed, successful probe. Explicit
  model confirmation requests remain marked for review. Outside an explicitly
  permitted root bypass, inline `ask=true` and the per-run checkbox prevent
  automatic dispatch and continuation grants.
  Targeted native Apple Silicon/Fedora Agent, backend, Scheduler-Agent and
  AI Manager tests passed for these paths, as did Windows ARM64 Clippy. See the
  automatic-dispatch entry in PORTING-PARITY.md. The inline root-bypass option is now implemented with the current policy,
  profile and credential controls. Its grants share revocation with background
  copies, and pause invalidates pending approved-directory replies. Native
  Apple Silicon/Fedora fixtures and 246 Agent tests passed, alongside backend,
  Scheduler-Agent and AI Manager selections. Windows ARM64 Clippy passed.
  Live SSH root execution and password interaction remain acceptance requirements.
  See the root-bypass entry in PORTING-PARITY.md for snapshot and cleanup records. The native ordinary-command grant
  does not bypass elevation or password approval.
- Java `terminalAgentExecutionEnabled` now imports as an inactive AI Manager
  hint. Explicit adoption and Save control a native execution switch whose
  default remains true, matching Java. Agent Run, executing Swarm and scheduled
  Agent diagnostics enforce it; Ask, planning and ordinary snippets retain
  their separate policy checks. Existing runs are cancelled on reconciliation.
  Background Agent dispatch and scheduled execution reread saved settings
  before dispatch, including previously approved commands. Policy restrictions
  continue to apply. Windows ARM64 application/test Clippy passed with warnings
  denied. Apple Silicon passed the explicit gate fixture, 236 Agent tests
  (three ignored), 27 Agent backend tests, 58 Scheduler-Agent tests and
  335 AI Manager tests (12 ignored). Native desktop, live SSH revocation and
  other target runtime acceptance remain open. See the Agent execution entry
  in PORTING-PARITY.md.
- Java `aiMemoryEnabled` and `aiMemoryLearningMode` now import as
  independently optional, inactive AI Manager hints. Java and Rust both
  support Propose and Off. Missing fields preserve native values; malformed
  fields are reported independently without dropping a valid sibling.
  Additive replay fills only absent hint fields. The Memory page provides
  separate explicit adoption actions, with the current feature, read-only
  and managed learning-policy guards. The token budget is unchanged by these
  actions. Only the normal Save affects the loaded runtime configuration.
  Windows ARM64 application/test Clippy passed with warnings denied. Apple
  Silicon passed the explicit persisted-runtime fixture, 86 Memory tests and
  333 AI Manager tests (12 ignored). Native desktop adoption and the wider
  Memory migration acceptance remain open. See the Memory activation entry
  in PORTING-PARITY.md.
- Java `sessionJournalSemanticSearchEnabled` now imports as an inactive
  AI Manager suggestion. Explicit adoption and Save establish the native
  default for preparing cross-journal questions. Enabled preparation uses
  the configured local embedding model with masked query/search cards, keeps
  the evidence review before answer dispatch, and falls back to lexical
  ranking if no model is available. The preparation view names the embedding
  model and describes the data sent. Configuration changes cancel stale
  preparations. The setting is searchable in AI Manager. Windows ARM64
  application/test Clippy passed with warnings denied. Apple Silicon passed
  the explicit semantic-preparation fixture, 617 Journal tests (12 ignored)
  and 330 AI Manager tests (12 ignored). Native desktop/provider acceptance
  remains open. See the semantic search entry in PORTING-PARITY.md.
- Java `terminalAgentCommandName` and
  `terminalAgentCommandNameCaseInsensitive` now import as independently
  optional, inactive AI Manager suggestions. Additive replay fills missing
  hint fields without replacing native preferences. Explicit adoption changes
  the name/case settings and preserves the shortcut enablement switch; the
  normal Save performs the concurrent-configuration check. Rust now accepts
  names up to 8 KiB instead of 32 bytes, with the same ASCII character rules
  as Java. The capture budget includes the full alias and prompt budgets.
  Java names beyond this bounded input contract are reported as omitted.
  Windows ARM64 application/test Clippy passed with warnings denied. Apple
  Silicon passed 615 Journal tests (12 ignored), 328 AI Manager tests
  (12 ignored) and 36 shortcut tests (one live SSH case ignored), including
  persisted adoption and actual local-shell input with a longer custom name.
  Desktop adoption and live SSH acceptance of an imported custom name remain
  open. See the Agent command migration entry in PORTING-PARITY.md.
- Java `aiMemoryMaxTokens` now imports as an inactive AI Manager suggestion.
  The reader follows Java's 500–4,000 token bounds; malformed values are
  reported as omitted. Additive replay preserves an existing hint and all
  native Memory settings. Explicit adoption changes only the proposed token
  budget, observes the current managed cap and leaves Memory enablement and
  learning unchanged. The normal AI Manager Save persists the choice and
  rejects a concurrently changed configuration. Windows ARM64 application/test
  Clippy passed. Apple Silicon passed 41 Memory tests, eight AI backup tests,
  27 Java profile-import tests and 615 Journal tests (12 ignored). The tests
  cover portable replay, native preference preservation, invalid values,
  explicit adoption and live policy limits. Native desktop adoption remains
  open. See the Memory budget entry in PORTING-PARITY.md.
- Journal summary interval and automatic-title preferences now import as
  inactive, independently optional suggestions. Interval values follow Java
  bounds (1–240 minutes). Additive replay fills missing suggestion fields
  without replacing existing ones. Adoption in Journal summary settings
  rechecks the imported source at Save, retains native settings until explicit
  adoption, and does not enable summaries or grant outbound title requests.
  Windows ARM64 passed 609 Journal tests (10 ignored), seven AI backup
  tests and 27 Java profile-import tests, including adoption, changed-source
  rejection, bounds, omission reporting and portable replay. Application/test
  Clippy passed with warnings denied. The later Apple Silicon Memory-budget
  snapshot also passed the Journal adoption fixtures in its 615 Journal tests
  (12 ignored). Native desktop adoption and remaining cross-host acceptance
  remain open. See the Journal preferences entry in PORTING-PARITY.md.
- Scheduler dependency diagram orientation and Auto, Light, and Dark color
  mode now restore from Java into individually unset fields. The native view
  reads the saved settings, and changes in the diagram persist for the next
  view or restart. Auto follows the native system theme. Verify import,
  native interaction and export on the remaining targets. Windows ARM64
  passed both focused preference tests and 207 Scheduler tests (one ignored).
  [Scheduler record](target/verification/20261117-scheduler-diagram-windows-suite.log).
- The Scheduler journal's table/detail divider accepts the saved Java
  position when the native field is unset. Rust starts at 72 percent for a
  fresh profile, clamps drags between 20 and 90 percent, and saves the position
  after a brief pause or at shutdown. New hosted and separate Scheduler views
  read the saved value. Verify dragging and reopening in native desktop views.
- Java's `sessionJournalLiveTailHeightVh` maps to the native Live Journal
  dock's log allocation when the native preference is unset. The dock
  exposes a height slider from 15 to 85 percent of the space shared by
  the timeline and output. It saves the profile preference on release
  or after a short pause and preserves it when reopening the log.
  Verify the control and anchor behavior during a live desktop capture.
- Java's `journalLivePanelPlacement`, `journalLivePanelWidth`, and
  `sessionJournalViewerEditDividerPosition` fill unset native layout
  preferences independently. The importer applies the native width and
  editor split limits. Saved native values win on restore and replay.
  Verify the imported placement, width and editor divider on native
  desktops after restarting kortty.
- The explicit Java window bounds for Journal Manager, Journal Viewer and
  Screenshot Editor now restore as independent placement fallbacks.
  Existing native per-dialog bounds take precedence. Detached windows
  apply the native screen placement and size limits. Verify all three
  dialogs on native desktops, including an unplugged secondary monitor.
- Imported Java dialog bounds cover AI Manager, Saved Chats, ASCII Art,
  Snippet Manager, Job Scheduler, the Guide Viewer, and the Agent
  workflow script. Exact named keys also cover Settings, Quick Connect,
  connection management, credentials, Teamwork, badges, known hosts,
  SSH keys, log files, recording management and replay, session
  processes, shell integration, environments, terminal effects,
  snippet comparison, highlight rules, and the three Journal dialogs
  with named geometry. Duplicates and unknown keys are reported as
  omissions. Existing native placement wins. Verify hosted and detached
  openings on native desktops after restore. The separate Java
  snippet-edit, AI review and language-migration dialog geometries
  still need a disposition because current Rust views are embedded.
- Java's last main-window bounds, maximized state, and Dashboard
  visibility now restore as startup fallbacks. Active fixed bounds and
  the native last-window record take precedence within their respective
  settings. The local remember-geometry and remember-Dashboard switches
  still apply. Verify restart, maximization, and display changes on
  native desktops.
- Java `sessionRestoreMode`, `sessionRestoreScrollback`, and
  `sessionRestoreScrollbackLines` now restore as independent suggestions.
  The backup preview identifies them separately. In Window settings,
  **Use imported settings** copies the available values into the draft.
  Save applies the draft through the existing managed mode and output
  checks. The native vault requirements still govern encrypted output.
  Verify adoption, restart, and vault-unlock behavior on native desktops.
- Java's `fileBrowserPosition`, `fileBrowserWidth`, and
  `fileBrowserShowHidden` now restore as local file browser defaults.
  Existing native side and width take precedence. An older native dock
  file can still inherit the newly persisted hidden-files choice until
  the user changes it in Rust. The Java last-root path is reported as an
  omission and requires selection on this device. The native file
  browser now remembers the last folder it opened successfully on this
  device and falls back to the home directory if the folder disappears.
  Verify local navigation and restart on native desktops.
- The SFTP file comparison AI panel supports the Java 9–32 point font
  range and 13-point default for questions and answers. The ordinary
  view saves changes as a global preference for newly opened SFTP views.
  Incognito changes remain local to the view. Java `sftpDiffAiFontSize`
  fills the unset native preference with Java's clamping behavior.
  Verify the font controls and reopen behavior on native desktops.
- The SFTP Manager remembers the local and server hidden-entry toggles
  independently for newly opened standalone, attached, and project views.
  Java `sftpShowHiddenLocal` and `sftpShowHiddenRemote` fill unset choices.
  Both panels show hidden entries by default. Changes in incognito views
  stay in the view. The focused file listing accepts Command+Shift+Period
  on macOS and Ctrl+Shift+Period on Windows and Linux. Verify restart and
  the toggles in standalone and hosted views on native desktops.
- Command+L on macOS and Ctrl+L on Windows and Linux focus the active
  SFTP pane's path field and select its existing path. The same field
  handles ordinary editing and navigation. Verify the shortcut with
  native keyboard focus in standalone and hosted views.
- The focused SFTP list accepts F2 for Rename and Delete or
  Command/Ctrl+Backspace for the existing Delete confirmation. The keys
  share the buttons' availability checks. Rename requires a single
  selection. Delete accepts multiple selected files, links or empty
  folders in either pane and shows the directory and selected names.
  The worker checks the selection before starting and reports partial
  completion if an operation fails. Running deletion has a Stop action
  and a close confirmation. Cancellation stops before the next removal.
  The result distinguishes confirmed removals from a server removal
  whose reply was lost. Recursive deletion previews regular files,
  folders, server symbolic links and recognized POSIX special entries,
  whether selected directly or included in a selected server folder.
  A mixed selection containing a special entry can be reviewed for
  deletion. Queued and drag transfers require every selected entry to
  be copyable. Recursive deletion revalidates the reviewed snapshot
  and removes children before their parents. Server links are removed as
  entries using their reviewed link targets, including dangling links.
  Recognized special entries are reviewed and unlinked using metadata
  without opening them.
  The reviewed path requires protected server directories and the SFTP
  POSIX rename extension. On Unix, local recursive removal also reviews
  links and recognized special entries without following or opening them.
  It verifies their identity and metadata through a private quarantine
  in the protected parent and restores a relocated entry on cancellation
  when its original name is vacant. Apple Silicon passed the native
  local link, FIFO and cancellation checks. Windows reviews local
  symbolic links and drive-path junctions by file ID and their reparse
  data. It holds the parent chain and removes the reviewed entry by
  handle. The review excludes volume mount points and unknown reparse
  types. Native Windows ARM64 passed junction removal and stale-review
  refusal. The fixture account could not create a Windows symlink.
  Fedora ARM64 passed local recursive deletion of links, a FIFO and a
  Unix-domain socket, and the authenticated OpenSSH server-side test.
  Verify Windows symlinks. Verify shortcuts,
  deletion review and cancellation in native standalone and hosted views.
  Apple Silicon passed recursive deletion against the native OpenSSH
  SFTP subsystem over pipes, including rejection of a stale review and
  a refreshed listing after removal. The regular SFTP
  Manager worker also passed the same review and removal scenario through
  native loopback OpenSSH on Apple Silicon with public-key authentication
  and a pinned host key. The same native test covered changed link
  targets, dangling links and preservation of a directory link's target.
  Separate runs covered a FIFO inside a selected folder and a FIFO
  directly selected with the folder. The worker refused confirmation
  after the FIFO became a file and removed a nested Unix-domain socket
  and the restored FIFO after a fresh review.
  macOS and Fedora ARM64 also passed the same worker test through a local
  OpenSSH jump, with distinct pinned keys for the jump and target server.
  Acceptance on configured deployment routes and with native desktop
  interaction remains open.
- The Java `showMenuBar` property has no runtime consumers in v2.26.0.
  The in-window menu toggle is session-only in both implementations, so
  a legacy stored value requires no import mapping.
- Java v2.26.0 declares the deprecated `snippetManagerPreviewDividerPosition`
  and the old `dashboardDividerPosition` in its settings models. Their
  accessors have no application consumers in the pinned source. The
  active snippet workspace and Dashboard retain their native layout
  handling.
- Java `asciiArtPreviewFontSize` fills an unset native ASCII Art preview
  size. The importer keeps fractional values and clamps finite sizes to
  the Java range of 6 through 40. Existing ASCII Art zoom settings take
  precedence. Verify the restored preview size and zoom controls in the
  native dialog.
- Java `snippetTranslationTargetLanguage` fills an unset native choice.
  A successful snippet translation review remembers the target for the
  next dialog and across restart. Other snippet actions use their own
  prompts. Verify the imported choice in the native editor and after a
  restart.
- Java `aiSnippetAlternativeSolutionCount` fills an unset native
  Snippet Editor preference. The reviewed request asks for up to 1–10
  distinct replacement candidates and defaults to three. A candidate
  opens the existing diff review before it can replace the selected
  range. Verify the choice, discard, apply and undo workflow on native
  desktops with a configured provider.
- Java's `aiAgentPanelPlacement` and `aiAgentPanelSideWidth` fill
  individually unset Rust Agent Design preferences. Regular pane
  history can use the design default or dock at the bottom, left, or
  right. The native Settings preview reflects the preferred side width.
  Inline history and the floating, adaptive and picture-in-picture
  arrangements retain their placement behavior. The View menu exposes
  the placement choices. A native divider previews the width during
  drag and saves it at completion for the still-current pane and
  layout. Verify menu navigation, drag, and restart on native desktops.
- Java's `defaultTerminalSettings` now fills unset native terminal font,
  initial dimensions, scrollback, SSH keepalive, bold mode, timestamps,
  color and cursor fields. Explicit `terminalCursorBlink` takes precedence
  over the blink encoded in older cursor styles. The Java customization
  flag gates ANSI and selection colors, and explicit native ANSI colors
  keep their individual entries. The Java global theme selection follows
  exact built-in and restored custom IDs. The separate theme-font flag
  restores when unset, and the Java base cursor style remains effective.
  Existing native selections and custom base colors take precedence.
  Edited built-in theme definitions restore under their original IDs
  when the native preset has no saved edit. Stock definitions need no
  local override. Java image paths are retained as inactive metadata.
  Verify inherited defaults and edited presets in desktop terminal sessions.
- Java's `defaultTerminalSettings.closeWithoutConfirmation` fills an unset
  native busy-terminal close preference. The separate global
  `closeActiveTerminalWindowsWithoutConfirmation` fills the independent
  Terminal Settings option for windows, application quit and tab batches.
  Its enabled value bypasses the ordinary terminal and Close All prompts.
  Active serial sessions and hosted editors retain their existing safeguards.
  Window close and application quit now include idle connected terminals
  when the global skip option is off. A single tab, pane, and the Close
  Other Tabs and Close Tabs to the Right actions use the busy-terminal
  rule. SSH without a known prompt is treated as busy. Saved and Teamwork
  connections can override the busy-terminal preference.
  Local busy detection uses the shell's live child processes, including
  background jobs, together with observed prompt and alternate screen
  state. Windows uses the terminal's job object. macOS and Linux query
  live children of the local shell. Flatpak host shells use terminal
  activity because the host process tree is unavailable through the
  sandbox-side launcher. Application quit asks for each affected window
  before closing any terminal sessions. Cancel discards the quit
  approvals. If a previously reviewed window gains a new or reconnected
  session, it needs another review. Verify the close dialogs on native
  desktops, including idle SSH, multiple windows and editor guards.
- The global SSH encoding preference now applies at connect and
  reconnect. Saved and Teamwork connection overrides take precedence.
  Rust preserves the emulation default when no override is saved.
  Java backups apply supported encodings only if
  `terminalEncodingConfirmed` was set. Older non-default choices
  appear as suggestions in Terminal Settings and require Save. Verify
  real input and output on configured SSH connections and reconnects.
- Java's Journal AI summary and screenshot analysis switches now import into
  independent native settings. Existing native choices take precedence.
  These settings gate the corresponding workflows, including scheduled
  summaries and automatic screenshot descriptions. They retain the existing
  requirements for a configured profile, managed policy, image input and
  reviewed routes or recording consent. Verify both controls in native
  settings and with a configured provider.
- Java's `swarmSessionJournal` now restores as an inactive suggestion in
  the interactive Swarm window. Explicit adoption copies recording, closing
  summary, retention, and deduplication choices into the current draft.
  Saving defaults remains a separate action. An imported summary profile
  stays unresolved until selected in the native editor. An exact source
  match from the same backup can prefill the existing profile suggestion.
  Verify the desktop restore, adoption and recording flow with a configured
  provider.
- Interactive Swarm journals offer optional input capture for reviewed
  commands. The recorder distinguishes output from command previews and
  uses the execution backend's start signal for local, attached SSH, and
  saved SSH targets. Probes and commands stopped before dispatch are
  omitted. Persistent defaults and Java adoption both retain the input
  choice. Known secrets are masked again at publication. Verify the
  desktop workflow on a live attached and saved SSH target.
- The saved Journal note translation language now fills an unset native
  preference using the existing native language validator. The migration
  preserves a native choice and trims Java's saved text. Apple Silicon passed
  the valid/invalid, replay and precedence fixture together with 613 Journal,
  206 Scheduler and four TLS tests. Windows ARM64 application/test Clippy
  passed. [Native record](target/verification/20261117-journal-language-native-macos.log).
- Complete the remaining global-field inventory. Distinguish portable
  preferences from machine paths, transient geometry/history, provider
  secrets and execution/clipboard/Control/MCP consent. Explicit native
  adoption is required where a setting would grant new authority. JVM-only
  fields and the recorded product decisions need dispositions, not automatic
  copies into unrelated native fields.
- Keep Java model-registry adoption separate from native portable-backup
  model reattachment. Standalone and split GGUF registrations preserve model
  IDs through a reviewed local selection, structural validation, stored size
  checks, and a fresh local file fingerprint. Portable registrations contain
  no historical content digest. Vision pairs publish the reviewed weights
  and projector together as a private asset directory. MLX reattachment
  validates and reviews the local data files, matches saved filenames and
  sizes, and publishes them under the original model ID. Keep source paths
  and execution approvals local.
- Toolbar restore binds new profiles and unique existing profiles with the
  same imported settings. SSH matching also checks the main and jump routes
  and excludes unavailable authentication evidence. Group buttons require
  exactly the archived direct members. The restore binds the reviewed
  connection catalogue through restart without rewriting an unchanged
  catalogue. Unresolved Java connection and group buttons can be assigned
  to a saved target in the toolbar editor, in the original slot. Review
  other ambiguous group, connection and Memory identities where their
  import workflows still require it. Preserve reported collisions and
  incomplete archive mappings. Verify the desktop workflows.
- Java Scheduler restore can also reuse unique, exact saved SSH profiles.
  The imported jobs remain disabled. Group membership must match before
  the restore binds the archive's execution order to the saved profile IDs.
  The backup preview can explicitly assign unresolved SSH references,
  including group members, to saved profiles with matching main and jump
  routes. It can also assign an archived local group to a different saved
  path when every SSH member matches uniquely. Shared groups still
  require their original Teamwork source. The reviewed catalogue is
  checked at queue time and startup. Verify desktop review and adoption
  of these drafts.
- Keep residual Java regex semantics and resource limits as a bounded audit.
  The recorded 3,251-case comparison and UTF-16/Unicode/class/backreference
  work are implemented. Confirmed content-derived legacy highlight IDs
  upgrade in place. An ambiguous source-ID collision preserves the local
  rules and links and allocates a separate deterministic ID for imported
  references. The restore preview reports the collision and the reviewed
  mapping is checked at queue time and at startup. Verify the preview in a
  native desktop session.

Read-only analysis snapshot comparisons and preserved-diagram viewing are
implemented in `src/snippet_window/analysis.rs` and
`src/snippet_analysis/model/java_history.rs`. Their remaining work is restored
desktop inspection and reference-backed handling of unsupported syntax.

The archive reader recognizes `llm/models.xml`, `llm/mlx-models.json`,
`rag/stores.json`, `snippet-analyses/`, `memory/` and `stats/`.
The Java Memory importer accepts the global document, matching global history
and archives containing only global scopes. It preserves the original bytes,
screens them with native memory validation and leaves existing files intact.
For a newly inserted, unambiguous SSH, local shell, or serial profile, it
rebinds the connection document, dated history and a mixed archive whose
scopes all match. The pending restore ties these files to the exact
connection catalogue and rechecks managed policy. Java digest state and
reference approvals are excluded.
Retained snippet history offers read-only comparison between available base,
result, accepted and checkpoint snapshots from the same apply run. The
viewer displays preserved diagrams without a native analysis flowchart
using the bounded Mermaid renderer or the original source. Verify both
workflows in a restored desktop profile. Sources are
`src/snippet_window/analysis.rs` and `src/snippet_analysis/model/java_history.rs`.
Unmapped Java skill usage already restores by exact source ID.

Canonical hashed and supported legacy IDs already map Memory documents,
history and fully matched mixed archives to exact newly imported SSH, local
shell, and serial profiles. The backup preview supports explicit assignments
to existing saved profiles of the same type. It marks verified exact matches
and offers a single explicit selection action while keeping individual choices.
Colliding Java filenames are reported and omitted. Archives use complete IDs
and require every scope to resolve uniquely. The Rust AI Memory editor lists saved SSH, local shell, and serial
profiles.

Ordinary Java Scheduler upload, download and sync retain transfer intent
in disabled drafts. In the Action editor, choose a path on this device and
adopt either a file, new folder or reviewed folder comparison for a single
saved SSH route. Native publication and approval limits apply. Java's
automatic file/folder discovery and overwrite behavior need review where
the equivalents remain open under Q1. Ordinary server archive jobs restore
as disabled exact-route drafts. One Java action with several targets splits
into one draft per mapped route, preserving all split prerequisites. Adoption reviews the helper's
source basename layout, exclusion patterns, vacant destination and optional
local path. Password ZIP requires explicit selection of a local vault entry.
A Java rsync single-directory upload can adopt its named or contents layout
and merge or mirror a reviewed existing Linux receiver. A single-directory
download can adopt those same settings for a reviewed existing local receiver.
For a direct single-target file download, ordinary and passwordless
existing-file adoption prefills the literal Java local directory and remote
basename. Newly imported grouped downloads prefill per-route folders using
the original Java display name and connection ID prefix. File and named
directory placements include the source basename at the corresponding
native destination. Paths must fit the current platform and every route
requires fresh review. Portable backup retains the receiver mode and paths, strips the
device-local proofs, and restores these replacements as disabled jobs.
A combined draft can adopt ordered mixed sources, named and contents layouts,
and the original delete option. Adopted drafts keep both the job and the
schedule disabled. Verify these implemented adoption paths in native
Scheduler windows; verify recurring protected folder workflows on deployed routes and implement any
reference-required destination contracts identified under T3/Q1/Q2.

Java Scheduler upload, download and sync with both sudo and staging selected
restore as disabled exact-route drafts. A multi-target transfer splits into
one draft per saved route, with each split job kept as a prerequisite of its
dependents. Adoption reviews both paths, selects the native sudo mode, and
configures a new file, reviewed file replacement, new folder or merge into an
existing destination folder. The job stays disabled and retains its schedule
for review. Folder schedule and dependency restrictions apply before approval.
Verify native desktop adoption on a configured route with the installed helper.

Java sudo commands, snippets and directory creation, rename, permission,
ownership, copy and recursive-delete jobs restore as disabled drafts. Commands
and snippets preserve their working directory and journal modes. File actions
and shell actions with a virtual terminal split by route, and dependencies
include all split prerequisites. Adoption checks the exact saved route, host
keys and sudo policy. Passwordless adoption can retain the virtual terminal.
Vault-backed adoption uses exec. Native file review covers paths, ownership,
permissions, copy type and exact deletion confirmation before saving the
job. Unsupported execution options and unresolved targets or
snippet definitions continue to be omitted. Verify adoption and firing on
configured native desktops in both sudo modes. See the
[Java sudo adoption checkpoint](PORTING-PARITY.md#java-scheduler-sudo-command-snippet-and-file-restore)
and `src/profile_backup/java_scheduler.rs::parse_action`.

For global settings, supported display defaults fill individually missing Rust
preferences. These include manager tabs, connection colors and split focus,
tab switching order, terminal scrollbars, workflow font size, guide zoom,
app design and animations, and remote sidebar position and width. The app
design importer accepts all 17 pinned Java IDs and their enum spellings.
The design and animation flags fill separate unset native choices. A
saved choice of Normal or enabled animations survives restore.
Verify their appearance after restart on native desktops.
Java's `guideFontScalePercent` uses the same 70–250% limits as the native
reader and fills only an unset native zoom. The general Java UI font
scale imports into the separate Appearance preferences.

Java `recentProjectPaths` and `openRecentClearedAt` fill an unset native
Open Recent preference. Compatible absolute `.kortty` paths retain their
order and appear as imported entries. The menu accesses an imported path
only when the user selects it. After opening and approving the project,
the native recent list records it as an ordinary project file. Verify the
backup review and menu interaction on native desktops.
[Open Recent migration](PORTING-PARITY.md#java-open-recent-project-history)

Coding Agents detection, notifications,
badge and panel settings restore with the same merge rules. Supported Java
menu action chords and explicit removals fill unset native keyboard overrides.
Existing native assignments and reserved key combinations take precedence.
Java's `shellIntegrationEnabled` fills the independent global setting for
automatic local startup wrappers. The connection still needs an explicit
wrapper opt-in. The global wrapper setting leaves mark handling for
already-integrated shells and SSH under the separate command-marks control.
Verify the saved-profile and Quick Connect presentation on native desktops.
Recording scope, color capture, idle pause and threshold fill unset preferences.
A fresh recording profile can restore availability after restart. Java
v2.26.0 captures replay files for either persisted format value and offers
WebM export. Recording folders and FFmpeg executables are selected locally.
Journal marker definitions and ordered rules restore additively. Custom labels,
colors and severity map to the native registry. Rules can target built-in
markers and imported custom definitions. Different definitions with the same
ID retain independent identities with their rules bound to the matching
definition. Native settings retain their automatic application preference and
existing rule order. Java regular expressions retain their text. Supported
patterns preserve Java shorthand classes, line terminators, and end-of-input
matching. Patterns outside that translation remain disabled for review.
Java toolbar layouts restore in their saved section order with icon size,
row visibility, settings shortcuts, separators, and legacy button additions.
Exact newly imported SSH, local shell and serial profiles can activate
connection shortcuts. Folder buttons require a complete match of direct
members. Existing native layouts have precedence. Unresolved buttons retain
their original targets as disabled entries. The Java Git Bash action maps
to the native Windows toolbar launcher. Remaining work covers settings
sections, shortcuts and actions
without exact native equivalents, resolution of existing or ambiguous
connection and folder references, advanced Java regex semantics, other
global fields, and native desktop acceptance. Preserve separate
interchange paths such as highlight-rule XML. See the
[Java preference checkpoint](PORTING-PARITY.md#java-window-keyboard-and-recording-preferences),
[Journal marker checkpoint](PORTING-PARITY.md#java-journal-markers-and-ordered-rules),
[toolbar checkpoint](PORTING-PARITY.md#java-toolbar-layouts-and-connection-buttons),
`src/workspace/backup_data.rs` and J `model/GlobalSettings.java`.
Favorites attach to newly inserted SSH, shell and serial profiles using exact
unique Java IDs. Exact normalized group colors apply to newly created groups.
The explicit Java default, text, coding, Agent and
security-check profile references can become inactive suggestions keyed
by imported profile provenance. Populate those Rust roles only through
explicit selection of a currently reviewed profile. The Security role is
used by snippet security review. Verify role adoption on native desktops.
The `rag/stores.json` importer now preserves portable hints keyed by exact
Java store and source IDs. Adoption creates a native store and reviews selected
sources and their resulting manifest, including filters, PDF handling and
synchronization. The user selects the embedding model and backend. Verify
these workflows in native windows and rebuild a configured Qdrant index with
the separately reviewed destination, collection and credential. A Java
Qdrant collection uses a different payload layout.
The separate `gpg-keys.xml` migration retains full fingerprints as pending
references. Reattaching a public certificate needs independent verification.
The imported Java registry preserves metadata. Native model registration and
profile mapping use separate reviews. Verify Java archive and native equivalent
backups with their respective fixture formats.

Source references are J `core/BackupManager.java` and
`app-docs/site/docs/en/features/backup.md`; R
`src/profile_backup/archive/java.rs`,
`src/profile_backup/java_migration.rs`,
`src/profile_backup/java_scheduler.rs`, and
`src/ai_manager/profiles/java_backup.rs`. Stored execution approvals,
model/file paths, provider credentials and temporary keys require their
applicable migration reviews. A remembered unlock credential belongs to
the local device and profile. A disabled imported job requires explicit
review before activation.

## Distribution, help and residual product audits

| ID | State and implemented coverage | Remaining work |
| --- | --- | --- |
| U1 | Acceptance with external release prerequisite. Manual and opt-in application checks, managed metadata feeds, native archive selection, size/SHA-256/native executable validation and reviewed download exist. Rust persists a version-specific skip or reminder and records a verified download only after publication. Automatic checks use local calendar days. Startup notices repeat on a later day and periodic notices after seven days. [Update prompt parity](PORTING-PARITY.md#november-20-application-update-prompts) | Verify the About actions and background notices on native desktops, the managed feed under P1 and the default channel against an owner-published stable release with compatible assets and digests. The public [Rust release list](https://github.com/chardonnay/korTTY_rust/releases) had no stable release on November 20. Package verification is tracked in V7. |
| U2 | Acceptance. Bundled rich manual chapters/anchors/figures, validated links, persisted zoom, Settings Help and reviewed Ask the manual with exact numbered source navigation exist. Ask the manual keeps ten distinct, masked questions. Recall fills the input for a fresh review. Java backup review includes masked `guideAskHistory` entries for available slots, and native encrypted backups carry the local history. [Manual question history](PORTING-PARITY.md#manual-question-history) | Verify Java restore review, native history selection and keyboard focus, zoom, layout and reviewed answers across desktops. |
| U3 | Acceptance + bounded audit. Native SSH connection monitoring, worker process views and all eleven supplied effects/previews exist. Fractional effect speeds survive Java connection exchange and uniform-tab project exchange. GitHub, SkillsMP and direct Markdown provider profiles support discovery, reviewed disabled import and revision updates with local edit protection. Local skill usage is recorded, displayed, and included in encrypted profile backup. JVM heap/GC and JAR ServiceLoader mechanisms are Java-specific. | Verify native provider and Skills UI acceptance, cross-host native AI backup and restore (Java data conversion is M1), hosted/detached monitor and effect lifecycle. Java projects represent one effect per tab. Make a product decision before implementing telemetry or third-party native effect plugins. |
| U4 | Acceptance with release prerequisites. Native Windows/macOS ZIP and Linux TAR.GZ, optional Windows MSI and Linux ZIP/DEB/RPM/Pacman/Flatpak staging, and macOS DMG packaging from a prepared signed app exist. An earlier Developer ID/notarized Apple Silicon ZIP has native evidence. The installed Flatpak passes host launch and Landlock PTY tests on Fedora ARM64. | Verify clean-system install, upgrade, removal, signing, runtime dependencies and sandbox/helper paths for each format. Complete the Flatpak x86-64 checks. Repackage a fixed source snapshot per target. J release assets and `package/`. R `packaging/src/main.rs`, `packaging/src/dmg.rs`, `packaging/src/msi.rs`, `packaging/src/linux/flatpak.rs`. |

For U4, the macOS disk image command verifies a prepared app bundle's
architecture and signature and checks that the app ticket survives staging.
Apple Silicon passed read-only mounting, app copy, signatures, CLI launch,
a Seatbelt session from the copied app and image removal. A separate pass
preserved the existing staple and Gatekeeper acceptance of the historical
Developer ID app. Complete Developer ID signing, notarization, stapling and
Gatekeeper assessment for the release disk image, and regenerate its checksum.
[Native DMG checkpoint](PORTING-PARITY.md#macos-disk-image-packaging).

Flatpak launches ordinary local shells and configured programs through
`flatpak-spawn --host --watch-bus` with the host environment and the requested
working directory. Terminal integration stages private startup files in a
host-visible cache. Terminal file actions and attached Agent commands use a
fresh prompt directory report backed by a shared host directory. The native
Fedora ARM64 tests cover exact arguments, host environment and directory,
Bash login and prompt marks, PTY input and size, and normal and cancelled
process exits. See the [Flatpak host-shell checkpoint](PORTING-PARITY.md#flatpak-host-shell-and-local-agent-commands).

Required Landlock sessions use the runtime-contained launch path. The verified
runtime system files are opened without symlink traversal and checked on their
read-only mounts. Fedora ARM64 passed the installed desktop launch from saved
Session Security settings on Wayland and X11. The package probes verified
workspace and scratch writes, blocked profile and out-of-workspace access,
and blocked host execution. Repeat the checks on x86-64 and retain the
corresponding package evidence.
[Flatpak required isolation checkpoint](PORTING-PARITY.md#required-landlock-isolation-inside-flatpak).

Pacman uses the native Linux install tree. The ARM64 candidate passed clean
installation with disposable signature enforcement, a synthetic Rust upgrade,
desktop and CLI startup, and removal in Arch Linux ARM userspace. The SSH
server archive helper was verified separately. The same ARM64 candidate
also passed a downgrade from the signature-verified Java v2.26.0 package
with both desktop launches and preservation of the Java profile and local
policy. The shared name `kortty` and lower Rust release number require an
explicit `pacman -U` operation. Repeat acceptance on native x86-64 and verify
the Rust publisher signatures.
[Java v2.26.0 package assets](https://github.com/chardonnay/korTTY/releases/expanded_assets/v2.26.0)

U1 evidence is recorded in
[shared MLX policy and release checks](PORTING-PARITY.md#shared-mlx-update-policy-managed-download-restrictions-and-python-parameters).
Run native format checks, Clippy, tests and release staging locally on
macOS, Linux and Windows. GitHub's equivalent Native Rust CI matrix is
available on manual dispatch. The PR #7 merge passed the hosted matrix
at `e3c1760`. See [local verification](README.md#verification) and
[current evidence and distribution](#current-evidence-and-distribution).
Publisher packages and clean-system acceptance remain in V7.

## Native acceptance and release gates

Existing results close only their recorded build/platform/conditions.
Cross-compilation, loopback fixtures and emulation must be labelled as such.
External prerequisites do not turn implemented workflows into absent features.
The [Step 6 checkpoint](target/verification/20261112-step6-release.md) links
the recorded platform, package and audit evidence from its earlier snapshot.

| Gate | Remaining evidence |
| --- | --- |
| V1 | An earlier [Windows ARM64 live transfer](PORTING-PARITY.md#windows-arm64-live-window-transfer) preserved three ConPTY processes, scrollback, split ratios and focus across a chooser cancellation and a programmatic move. Verify physical drag and cancellation, the current release candidate, native Windows x64, macOS, Wayland and X11. Include transfer under output, blocked broadcast, restore and unsaved-close cancellation. Record Xwayland separately. |
| V2 | Verify [GUI dialog design parity](#gui-dialog-design-parity) against Java v2.26.0 and record the visual result for each dialog in the parity record. Cover fresh profile/restart, small screen, DPI changes, large→small→large monitors, owner modality/focus, modeless re-entry, hosted/detached tools, menu/toolbar keyboard use, fullscreen/transparency and macOS Dock reopen. Preserve compositor control of Wayland placement. |
| V3 | Windows ARM64 already has measured activation of the exact inactive Gemini split and original window. Complete native macOS permission/display/activation, Linux compositor-qualified delivery, and moved/closed/modal/revoked/stale targets on each backend. General and programmatic notices need their own cases. |
| V4 | All 21 emulations exist. Complete GUI/key/reply/charset/mode/resize/reflow and supported Scheduler comparisons per profile. Restore the pinned SithTermFX checkout before fixture regeneration. Verify audible Windows/Linux bell and retain documented Wyse differences. Real TN3270/TN5250, HP application/device and physical serial break/modem/flow-control remain explicitly user-deferred. |
| V5 | Enabled real providers and signed runtimes, every AI Manager section hosted/detached, editor source jumps after edits, seven bounded diagram families, clicks after zoom, full SVG/PNG/PDF beyond the viewport, retained analyses after restart and consent/cancellation at mutation boundaries. Include current A/B/S/J changes. |
| V6 | The separate SSH source review led to two transport fixes with native wire regressions. Apple Silicon passed the packaged App Sandbox, changed pin, tampered helper, approved tunnel, cancellation and jump fixtures. Resolve the recorded `crypto-bigint` and OpenPGP RSA findings after refreshing the dependency audit, obtain external independent review, and complete native Windows x64 and macOS Intel packaged isolation. Keep the SFTP external replacement race as a separate hardening risk. See the [SSH audit](target/verification/20261112-step6-ssh-audit.md). |
| V7 | Merge CI passed for `e3c1760` on Windows, macOS and Ubuntu. The November 19 Windows ARM64 ZIP passed checksum, payload, startup, restart and packaged LPAC worker readiness verification. Its exact source also passed 4,526 native workspace tests with 96 ignored and passed strict workspace Clippy. [Windows portable release](PORTING-PARITY.md#november-19-windows-arm64-portable-release) PR #8 recorded the November 18 full native suites and release builds on Windows ARM64, Apple Silicon and Fedora ARM64 at `fb8870149949`, including ad hoc signature verification on the Mac. These runs predate the current worktree. Build the release candidate from one fixed revision, run the full suites and native gates, and verify clean installation, upgrade, removal, signatures and macOS notarization, stapling and Gatekeeper. Measure same-host startup, idle memory and p95 input latency, including 5K macOS. [Source and package evidence](#current-evidence-and-distribution) |

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
The 2026-11-15 lockfile and offline dependency-tree recheck still finds
`crypto-bigint 0.5.5`,
`elliptic-curve 0.13.8` and OpenPGP `pgp 0.21.0` with `rsa 0.9.10`.
The advisories were rechecked on 2026-11-15. Patched versions are
`crypto-bigint >=0.7.6` and `rsa >=0.10.0`. SSH's separate `rsa 0.10.0`
provider is already corrected.
[Bigint advisory](https://rustsec.org/advisories/RUSTSEC-2026-0387.html),
[RSA advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).
This source review does not establish practical exploitability or replace a
new full dependency audit. The upstream
[RustCrypto migration](https://github.com/rpgp/rpgp/pull/573) remains open
at this review. The published rPGP 0.21.1 manifest still uses RSA 0.9.10
and the same elliptic-curve generation.
[rPGP 0.21.1 dependency manifest](https://github.com/rpgp/rpgp/blob/v0.21.1/Cargo.toml)
Evaluate the NIST curve migration separately against the existing
cryptography constraints and all private-key and route fixtures. Review
OpenPGP decryption blinding and legacy backup restore before adopting a
new provider.

The previously recorded Windows ARM64 dependency tree confirms the affected NIST
generation is active through Sunset even when inspecting `kortty-ssh` alone.
OpenPGP and the application's direct RSA dependency select `rsa 0.9.10`
in that record. The 2026-11-18 local lockfile inspection still contains
`crypto-bigint 0.5.5`, `elliptic-curve 0.13.8`, `pgp 0.21.0` and `rsa 0.9.10`,
alongside the newer SSH RSA dependencies. Upstream status and advisories
were not refreshed in this documentation reconciliation. Repeat the current
audit before selecting remediation; these records are not an independent review.

The current SSH-only dependency tree resolves the old private NIST operations
through Sunset's `ecdsa_portable` and `p256_portable`, `p384_portable` and
`p521_portable` adapters. The active `ssh-key` ECDSA feature parses key
representations. The stable RustCrypto curve generation reaches patched
`crypto-bigint` through `elliptic-curve 0.14`, with `ctutils` and `cmov`.
`cmov 0.5.4` selects the AArch64/x86-64 assembly backends without a portable
Cargo switch. The existing exception in [HANDOFF.md](HANDOFF.md) covers the
RSA provider. The ECDSA use requires an explicit scope decision or a
separately verified backend. Keep the existing encrypted OpenSSH and PKCS#8
key parsing behavior and verify all three curve algorithms over the wire.
[RustCrypto elliptic-curve manifest](https://github.com/RustCrypto/traits/blob/elliptic-curve-v0.14.3/elliptic-curve/Cargo.toml).
[RustCrypto cmov backends](https://github.com/RustCrypto/utils/blob/cmov-v0.5.4/cmov/src/backends.rs).

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
