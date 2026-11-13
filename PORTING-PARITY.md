# Porting parity checklist

The current remaining-feature audit and implementation sequence are in
[PORTING-PLAN.md](PORTING-PLAN.md). This file retains the
historical baseline and chronological verification evidence; baseline-only
missing items must be reconciled with that plan and the later entries below.
Links under `target/verification/` point to local build records.

This inventory records implementation and acceptance separately. It starts from
Rust commit `f463f9ce2d14d1805d8c5fd170467490ed155427` and the scope in
[HANDOFF.md](HANDOFF.md). **Present** means that the named Rust path exists.
**Partial** identifies a known gap. **Audit** means that a feature-by-feature
comparison is pending. A test target listed as coverage describes an existing
check. A historical result identifies an earlier run. Current acceptance stays
open until a result is recorded for the tested revision and environment. Both
Mosh modes are out of scope. The editor has a reduced native scope with structure
navigation for classes, functions, variables, and placeholders.

## Java snippet migration, password commands and rsync directory upload

The ordinary snippet import accepts the private Java `snippets.xml` library.
It resolves Java folders, retains supported diagrams under their original
snippet, and records their source digest and range. Stale and ambiguous
positions keep their preview and suppress source jumps. The import reports
skipped diagrams and invalid references.

Manual SSH and SFTP login can select a scoped encrypted password command in
the vault. The review displays the native executable, literal arguments and
exact target and jump host fingerprints. The worker rechecks the recipe and
pins and reads one bounded output line in a private work directory. The
password follows the same protected authentication path as a typed password.

Scheduler rsync directory upload copies an exact reviewed local inventory
into a new server directory. The pinned SSH connection runs the rsync receiver
and the fixed native server helper on separate channels. The helper verifies
the private staged tree and publishes it with an atomic no-replace rename.
The ordinary SSH channel budget retains its existing defaults.

On Windows, AI socket shutdown cancels blocked TCP I/O for local, native TLS,
and supervised model connections. This releases the request worker promptly
when a streaming file comparison is stopped.

See the [verification record](target/verification/20261113-porting-next.md).

## Outbound editor workflows and saved connection window geometry

The live, timeline and screenshot journal editors can translate note drafts
with the saved Text and translation profile or the default profile. A saved
HTTPS or CLI profile shows the exact masked request before a one-use approval.
The worker checks the persisted profile and policy and the bound journal
source before dispatch and before returning a result. A live source is tied
to its original recording and terminal attachment. The editor still applies
only to its current draft and offers undo.

Snippet AI assistance can review a saved CLI profile and its exact input,
including prompt files. The request remains tied to the editor revision and
policy. An accepted model response is a proposal until the user applies it.
The SFTP AI comparison streams bounded, masked partial replies from compatible
profiles and can stop the active request. It enables a new file draft only
after the stream finishes and a complete code block passes validation.

The shared native formatter normalizes Groovy indentation and preserves
multiline string and comment contents. Saved local SSH connections can use
private per-connection window geometry via **Connect in New Window**. Java
private XML can supply the geometry. Duplication and portable exchange omit
it. The standalone window uses native placement constraints and leaves the
ordinary remembered main-window geometry available for other connections.

See the [verification record](target/verification/20261113-journal-outbound.md).

## Scheduler groups, Swarm command sequences and browser journals

JobScheduler stores the ordered explicit targets and selected connection group
paths separately from the effective reviewed targets. Groups include descendant
groups and deduplicate existing routes in the same order as the Java resolver.
Fresh resolution before save, review, and dispatch compares the current routes
with the saved set. Added, removed, renamed or changed routes require a new
review. Single-target operations accept a group whose effective set has one
route. Existing route limits apply. Group selection participates in the Agent
approval scope while existing jobs retain their original scope encoding.

Interactive Swarm can review and approve the ordered ordinary command prefix
already generated for each displayed target. The normal SSH or local command
approval checks are used for each launch. Each additional command requires a
successful previous exit. Pause, restart, stale state and policy revocation
retire the remaining authority. A future command or a privileged command uses
its own review. A managed per-command policy disables batch approval.

The journal viewer can save a standalone HTML page at a reviewed local path
and then open the verified file in the registered desktop application. The
page uses the journal's bounded capture preview and validated embedded PNGs.
The file contains no external resources or links. The desktop handoff checks
the current journal, policy and identity of the saved file.

See the [verification record](target/verification/20261113-scheduler-groups.md).

## Swarm controls, remote drafts and desktop exports

Interactive Swarm can pause, resume and restart an individual Agent target,
and restart the entire round. A paused queued target gives its slot to an
eligible target. A running target pauses at a safe operation boundary.
Restarts wait for the previous attempt, keep its journal, and allocate fresh
attempt tickets and reviews. Restarting a saved conversation also reopens the
target review. The current command batch remains bound to the commands the user
has reviewed.

One saved SSH route is sufficient for a scheduled read-only or iterative
Swarm. The action, approval, per-target report, optional comparison and saved
Swarm follow-up retain the Swarm schema when the target count is one.

SFTP file comparison has editable prompts and can open an unsaved Snippet
Manager draft. A generated file can also be published as a new private file
in the active server pane after separate review. The worker binds the exact
draft and destination to the authenticated peer, verifies the staged bytes
and uses a no-replace hard link. This requires the matching server extension.

AI Chat can save and open an export. Session Journal can open its verified
PDF, Markdown or unencrypted archive after export. Both check the published
file identity and current policy before requesting the desktop handler.
Fenced LaTeX and `$$` display math render through a bounded Rust typesetter
using the embedded STIX Two Math font. Unsupported formula source is visible.
Embedded raster images can be copied at their original dimensions to the
native image clipboard or to the managed internal clipboard.

See the [verification record](target/verification/20261113-remaining-workflows.md).

## Swarm scheduling, file comparison AI, tables and the Linux badge

**Schedule…** in an idle Swarm opens a disabled JobScheduler draft with
the current task, profile, saved SSH identities, action mode and enabled
journal settings. A clean hosted Scheduler can receive the draft. An
unsaved Scheduler draft remains open in its original view. The transfer
has no execution approvals. The existing Scheduler accepts two through
32 saved routes. The shared working directory starts at `/` and can be
edited before review.

SFTP file comparison can send a reviewed masked explanation or
new-file request to a local, managed, HTTPS or configured CLI profile.
Private comparisons use local or managed profiles. The exact snapshots
and their masked diff are shown in the review. A generated file opens
in a native editor and requires a new local destination to save.
Streaming, snippet export and a server destination remain open.

Supported Markdown tables render in the terminal AI chat. The whole
table copies as tab-separated text, columns include the heading and
cells copy their displayed text. Malformed and oversized input remains
available as source. Copy authority is bound to the current chat turn.

Linux application windows use the same ID as the staged desktop entry.
The launcher counter emits count, visibility and urgency on a persistent
session-bus name. The Fedora fixture observed three ordered updates
from one sender. Dock rendering needs a launcher that supports
LauncherEntry.

The Windows SFTP stage cleanup uses handle-based POSIX disposition
when available. A bounded retry covers transient sharing and access
errors. The regression verifies removal while a shared reader stays
open. See the [verification record](target/verification/20261113-workflow-parity.md).

## Manual CLI, chat previews, memory cleanup and editor tools

Ask the manual prepares one masked model-family prompt for review and
dispatch. The selected Text or default profile may use an approved CLI.
The review shows the exact invocation and the prompt files when used.
The answer keeps exact source navigation. The reviewed HTTPS transport
also checks credential identity before automatic model metadata requests
and at completion dispatch.

Journal note translation, screenshot descriptions, topic exports, ASCII
Art and skill shortening apply the selected profile's prompt preset.
Journal note translation uses the local Text or default profile and keeps
the earlier Journal fallback when neither is assigned.

Completed terminal AI answers can render static SVG, supported Mermaid
and embedded PNG, JPEG, GIF or BMP in memory. The preview retains a
source view and copy action. Inputs that exceed preview limits remain
available as source. LaTeX, Markdown table rendering and copying the
rendered image remain open.

AI Memory has separate reviewed actions to merge managed entries and
shorten user paragraphs. The approved proposal checks the exact original
revision and stores the previous version in history. Native desktop and
enabled-provider acceptance remain open.

The shared editors support built-in JSON and INI/Properties formatting
and approved installed formatters for XML, YAML, TOML, HTML, CSS,
JavaScript, TypeScript, Java, Perl, SQL and Terraform. Groovy and parity
with the Java bundled runtimes remain open. SFTP archives use saved
remote-directory and compression defaults. The Java transfer-settings
exchange carries the archive fields when supplied.

See the [Windows verification record](target/verification/20261113-native-parity-follow-up.md).

## Signed catalog, Swarm renaming and journal status

The bundled model and prompt catalog matches the canonical Java v2.26.0
file. Builds with the publisher's independent Ed25519 key check the fixed
stable channel anonymously and verify its detached signature. The private
cache keeps the accepted sequence and rejects replays and collisions. The
verified catalog feeds guided and manual model recommendations and automatic
profile prompt mappings. Effective prompt changes invalidate dependent
reviewed Agent, Swarm and Scheduler work and journal summary grants.

Completed saved Swarm conversations can be renamed from their open window.
Publication compares the saved revision and preserves the conversation ID,
turns, comparison and scheduled origin.

Native journals persist the classified command count at close and rebuild it
from the capture when recovering abandoned or older journals. The full viewer
and exports use that count. The filtered raw bundle retains its input counter.
The manager shows active and waiting close or re-evaluation passes in journal
rows and their groups.

[Windows verification](target/verification/20261113-signed-catalog-swarm-journal.md)

## Guided local AI and knowledge scale

The guided setup reviews signed llama.cpp runtime metadata and the exact
Hugging Face model downloads. It tests each selected GGUF model locally
before saving the selected Text, Coding and Embedding roles together. A
shared Text and Coding recommendation uses one model and one profile.
The signed model prompt catalogue refresh remains open.

Explicit review can authorize up to 32 MiB per UTF-8 file. Existing saved
source limits continue to apply. Local and Qdrant snapshots support up to
8,192 passages within 96 MiB, with up to 32 source files. Scheduler reuse
streams the snapshot and source identities without decoding the vectors.
Native guided download and inference acceptance remains open.

[Windows verification](target/verification/20261113-guided-ai-knowledge-scale.md)

## AI setup and scheduled Qdrant

The AI Manager offers the nine pinned Java GGUF recommendations by role and
detected physical memory. The recommended route reviews an exact Hugging Face
commit, the selected quantization, size and SHA-256 before download. The existing
manual search and verified download flow remain available. Cloud profiles offer
eight endpoint presets and offline model examples alongside live discovery.
Choosing a different destination clears trust and credential references. The
setup wizard can probe configured and standard numeric loopback addresses for
model names without transmitting credentials.

The snippet and file editors offer reviewed Black and RuboCop formatting.
Native programs and script launchers use the existing bounded process and
preview. Python and Ruby script launchers are reviewed with the exact native
interpreter and launch script. The source reaches the formatter on stdin.

Scheduled approvals can retain excerpts from anonymous and vault-backed Qdrant
indexes. The Qdrant route, receipt, local snapshot and source contents are checked
before using the approved text. A vault-backed review also binds the encrypted
vault envelope. An unchanged envelope survives restart, and a vault write
requires renewed review. Firing does not query Qdrant or decrypt the vault to
reuse the approved text. Environment-key Qdrant remains available for live
retrieval and needs a durable credential witness for saved approvals.

The later guided setup and signed catalog checkpoints above extend this
initial recommendation and profile setup work.

[Verification record](target/verification/20261113-ai-setup.md)

## Reviewed autonomous knowledge

Each knowledge store has its own opt-in for Agent, Planning, Swarm and
Scheduler, independent of Text and Coding assignments. The live review shows
the exact bounded masked goal, embedding destination and Qdrant destination
when configured. A second preview includes the actual excerpts in the model
system prompt. The dispatch guard checks the current store, index, sources,
profile and credential authority. The model receives the excerpts as
untrusted evidence. Command review remains in the existing workflow.

Scheduler model proposals, standing Agent and Swarm requests, read-only
Swarm and its optional comparison can use reviewed excerpts from a local
index. A saved model approval retains the exact masked excerpts and binds
them to the index snapshot and its source digests. The worker verifies
that evidence before dispatch without re-embedding the question. Removing
or changing the store or rebuilding its index requires a new review.
Portable Scheduler backup removes model approvals and their excerpts.
Durable Qdrant approvals and native provider/UI acceptance remain open.

[Verification record](target/verification/20261113-autonomous-knowledge.md)

## Model-family prompt presets

AI Manager saves one model prompt preset per personal profile. The pinned
Java catalogue supplies family aliases and precedence. New profiles use
Auto. Older profiles without the field use Generic and retain their legacy
prompt bytes. A resolved preset is included in the profile authority for
saved workflows.

Terminal Chat, Agent, Planning, interactive and scheduled Swarm, Scheduler
proposals, journal questions and summaries, and the reviewed snippet
workflows include their selected guidance. Request previews, context
budgets and dispatch use the same text. Existing provider JSON instructions
remain last where they apply. Summary grants identify the selected guidance
in settings. Fixed connection probes use the provider's existing prompt.

[Verification record](target/verification/20261113-model-prompt-presets.md)

## Reviewed CLI journal answers and snippet analyses

The journal viewer and manager can prepare an answer from a saved CLI
profile. The review shows the executable, expanded arguments and exact
masked input. The request uses the currently reviewed evidence and can be
started once. The answer accepts exact quotations from the evidence and
keeps the original citation targets. Local ranking and evidence display
remain available.

The Snippet Manager can analyze one snippet or the selected files of a
folder with a saved CLI profile. The report keeps the original source
identity and file order. The reviewed invocation receives masked source.
Profile, policy, vault or source changes invalidate the request and its
pending result. When the split preference is enabled, a saved HTTPS or CLI
report opens a separate planning preview for that report and profile.

[Verification record](target/verification/20261113-cli-journal-analysis.md)

## Reviewed HTTPS and CLI snippet splits

A saved code or project analysis can plan a split with its original local
profile, a trusted HTTPS profile or a reviewed CLI profile. The outbound
preview shows masked source and the exact prompts. For CLI it also shows
the executable fingerprint and the expanded launch arguments. Each module
and any repair have a separate approval. The provider and its saved revision
are bound to the plan. A changed provider requires a new plan.

The native workflow checks the library, analysis source hash, stored split,
current policy, vault and credential before dispatch and again before
retaining the result. The HTTPS transport compares the credential resolved for
the request with the reviewed credential. Previous modules and plan fields
are checked before serialization when continuing a split. Generation
checkpoints survive reopening. Selected-file publication and publication
recovery work locally after review.

The initial checkpoint covered manual split planning. See the
[verification record](target/verification/20261113-outbound-snippet-split.md).

## Scheduled Swarm target capacity

The Scheduler can review up to 32 ordered saved SSH targets and parallelism
from one to sixteen for both read-only advice and iterative command proposals.
New jobs default to four slots. Jobs saved without a parallelism value retain
the legacy review receipt and execution behavior. The exact parallelism and
ordered routes are bound to new approvals.

Read-only target workflows refill available slots as targets complete. Model
workers share the existing global admission limit. The request is checked at
dispatch, and explicit parallel runs check current authority again before
retaining successful advice. Iterative target workflows retain their slots
while waiting for subsequent commands or reviews. Existing iterative SSH and
journal resource caps still apply. Ordered reports, comparisons, per-target
journal links and saved conversations retain the larger set. The interactive
follow-up UI allows a fresh selection of up to eight of those saved targets.
The Scheduler store evicts eligible oldest records if new finishes would
exceed its existing 16 MiB file limit.

See the [verification record](target/verification/20261113-scheduled-swarm-capacity.md).

## Named Swarm drafts and command batch review

AI Swarm can save a named draft before starting a run. It stores the current
AI profile and ordered saved SSH connection hints, with eligible directory and
group metadata. It keeps unsent task text in the open window and excludes it
from the saved record. The library identifies drafts and reopens them for
editing. A first run reviews the current saved profile, target directory and
host keys before the usual model and command reviews. Its masked results
replace the empty draft history under the same title and record ID. Completed
rounds can also receive a title during their first save. The encrypted backup
retains drafts and drops connection hints that do not match the restored
connection identity.

In AI command review mode, the batch panel shows the currently ready ordinary
commands and fixed probes with their targets, paths, and available host-key
fingerprints. Confirmation checks the exact displayed entries against the
current round and destination and sends one ordinary approval to each. Later
commands and typed elevation use their existing individual approval paths.
Each dispatch still rechecks the destination and policy. Native desktop
operation and live SSH batch execution remain acceptance checks.
[Verification record](target/verification/20261113-swarm-drafts-batch-journal.md)

## Session Journal scrollbar thumb

The Java v2.26.0 viewer and HTML export share a renderer with a 40-pixel
thumb minimum. The native Rust journal viewer uses the same minimum on its
scrollable panels, limited by the track length. The Iced patch uses the
available track travel for both rendering and pointer interaction. The HTML
bundle applies the minimum to the page, capture panel, and image dialog in
WebKit and Blink. In forced colors the browser provides the scrollbar style.
Native desktop and browser dragging remain acceptance checks.
[Java renderer](https://github.com/chardonnay/korTTY/blob/v2.26.0/src/main/java/de/kortty/core/SessionJournalHtmlRenderer.java#L1281) ·
[Iced patch](vendor/iced-widget/KORTTY-PATCH.md)

## CLI catalogue, LM Studio model loading and profile preferences

AI Manager and its setup wizard present all eighteen Java CLI provider names
and a custom entry. Seven provider routes offer passive, bounded path
suggestions. The MiniMax suggestion identifies Node and the package module.
Applying a candidate still requires executable and argument review. Generic
providers retain literal arguments and do not acquire invented presets.

An LM Studio downloaded-model list can authorize a separate preview of
`lms load <model> -y`. The selected key must come from that reviewed list.
Loading rechecks the original executable fingerprint and policy. Its owned
CLI process is cancellable and it leaves the configured model untouched.

Reviewed CLI profiles can be the default and appear in the role pickers.
Eligible Terminal Chat, Agent, Swarm and journal dialogs use those preferences.
They still require an explicit reviewed request or separate automatic grant.
Existing local automatic journal settings and Scheduler job bindings retain
their own selections.

See the [verification record](target/verification/20261113-cli-catalog-roles.md).

## Reviewed CLI profiles for Agent, Swarm, Scheduler and journals

Saved CLI profiles are selectable for Agent Ask and Run, interactive Swarm
targets and follow-up, comparison and workflow generation, and scheduled
Agent and Swarm. Each review identifies the approved executable, fingerprint,
arguments and payload. The guarded launcher rechecks policy and authority
before the process can use input. Scheduled reviews bind the profile revision,
redaction rules and saved SSH route. Command proposals retain their separate
execution approval. Invalid structured CLI responses do not trigger the
HTTP format fallback or an automatic repair request.

Manual journal passes and automatic summaries have separate CLI authority.
The automatic grant binds the source location, profile, redaction policy and
long-output mode. Live and closed journal writers recheck the CLI grant before
publishing. CLI summary passes send masked text. Existing local vision routes
continue to handle screenshot descriptions. MiniMax refuses requests that
contain the current login credential and checks for login changes before
activating its reviewed program.

See the [verification record](target/verification/20261113-reviewed-cli-workflows.md).

## Reviewed HTTPS profiles for Swarm, Scheduler and journal summaries

Interactive Swarm offers explicitly opted-in HTTPS profiles for regular
live panes and saved SSH targets. Each target uses its own masked model
review. Saved follow-up, completed-round comparison, and workflow draft
generation have separate reviews. An unavailable or changed saved profile,
policy, target or credential prevents dispatch under the old review.

Scheduled Agent and Swarm can save reviewed requests for trusted HTTPS
profiles, including fixed diagnostics, one-use proposals and per-firing
model proposals. Saved requests bind the model profile, target routes and
redaction rules. Per-firing and iterative command approvals still govern
the exact commands executed over SSH.

Journal summary settings provide a revocable HTTPS approval for masked
text from eligible recordings started after approval. Manual re-evaluation
and catch-up approve the listed source journals separately. The private
queue rechecks the saved profile revision, journal directory, redaction
rules, source and receipt before dispatch or retry. Screenshot
re-description uses the existing local vision route.

The native HTTPS transport masks echoed request credentials in text and
streaming previews. A structured response or generated artifact containing
the credential is rejected before application. The completion usage record
also filters echoed credential values.

Native Windows ARM64 passed 3,346 workspace tests with zero failures.
Two separately run native TLS tests verified the OpenAI-compatible and
Anthropic protocols, masked journal summaries, and revocation at a
loopback fixture. See the
[verification record](target/verification/20261112-ai-provider-routes.md).

## External skills, Python structure and Java effect exchange

External skill providers in AI Manager cover GitHub repositories, GitHub code
search, SkillsMP keyword search, and direct Markdown URLs. Provider credentials
are referenced by environment variable and bound to the provider endpoint.
SkillsMP references resolve through a separate GitHub route. An external skill
requires Markdown review before it enters the library draft disabled. Its
stored provenance records the source, revision and imported content digests.
Update review preserves local changes through disabled-copy and skip options.

Python structure collects alias and unpacking bindings and initial augmented
assignments. The tests cover scoped references, multiline targets, Unicode
selection and dictionary and tuple assignment kinds.

Fractional effect speeds survive Java connection XML, native profile storage
and Java project XML for tabs whose panes use the same settings. Typewriter
pacing uses the stored multiplier. Java project export reports mixed pane
effects because the format carries one setting per tab.

Native Windows ARM64 passed all 2,760 application tests. Two existing SSH
library tests failed in the parallel workspace run. All 26 library tests
passed when run serially on the same build. Formatting and Clippy passed.
See the [verification record](target/verification/20261112-external-skills-editor-effects.md).

## Command summaries, Lua editor tools, and AI profile backup

Journal summaries now publish one card for each recognized command. Help-only
calls and recognized failed starts advance the durable checkpoint without a
card. Interactive replies stay with the original command. The optional long
output mode streams verified capture groups through bounded masked requests
and combines the results into one card. Running previews retain the existing
15-minute threshold and use sampled output. An automatic HTTPS approval binds
the output mode as well as the profile, source, and redaction rules. A change
to the output mode revokes older automatic receipts. A closed capture resumes
at its durable checkpoint if the live writer stops before publication.

Manual passes expose a reopenable modeless progress window with the current
stage, completed summaries, elapsed time and a remaining-time estimate when
the planned work is known. Closing the window leaves the pass running. The
window's cancel action is bound to the batch displayed on it.

The snippet and file editors recognize Lua and LuaJIT, highlight Lua syntax
and offer keyword and standard-library completion. The reviewed native tool
path can format with StyLua, shfmt, rustfmt and gofmt or check Lua syntax with
`luac -p`. Source travels on standard input. Applying formatted output is a
separate action and requires the exact editor revision. The same editor path
serves SFTP, where route and policy are checked before tool use and apply.

Personal and managed startup requirements use the same pre-workspace vault
setup and unlock path. A managed requirement locks the setting. The personal
default is off. Shared policy accepts the Java Boolean `allow-telemetry` field
for schema compatibility. Local skill usage records accepted, retained skill
blocks in a private ledger with scoped managed control and a native report.

Encrypted native profile backup now carries saved masked AI and Swarm chats,
eligible memory files, skill statistics and AI profile, model registration and
knowledge-source metadata. Restored connection and skill references are
checked against the installed library. Endpoint, executable and automatic
source approvals require a new review. The archive contains registration
metadata for locally managed model assets. Weights and indexes use their
separate native acquisition and regeneration paths.

See the [verification record](target/verification/20261112-command-summary-editor-backup.md)
for native tests, actual editor tool fixtures, and the remaining platform checks.

## Scheduled changed-file downloads, rsync directories, and MiniMax CLI

Windows one-use Scheduler SFTP downloads can review replacements alongside
missing entries. The review hashes matching regular-file pairs even when size
and modification time agree. Existing approvals for missing-only jobs retain
their scope. Replacement approvals bind the exact plan, the saved SSH route,
source and destination digests, and local identity and Windows DACL. The job
rechecks the route and approval at mutation boundaries. Replaced originals
remain in private sibling folders under a common recovery pattern recorded in
the Scheduler run result. This mode applies to Windows destinations. Remote
replacement and Unix publication retain the open T3 requirements.

Unix Scheduler rsync can download one ordinary directory tree into a new local
destination. The bounded rsync protocol and the SFTP manifest use the same
pinned SSH connection. The file set and contents are verified before local
publication. Publication uses ordinary destination inheritance and a
no-replace directory rename. Directory upload, multiple sources, optional
deletion and Windows execution remain in Q1.

AI Manager includes an explicit MiniMax CLI preset for the official pinned
`mmx-cli` releases 1.0.15 and 1.0.29. The reviewed native Node executable runs
the verified package with JSON input through stdin, a fixed command and one
regional HTTPS destination. The launcher rejects redirects and exposes only
the fixed package modules and virtual input and login data. Expired OAuth
sessions are refreshed through the official CLI. This preset follows the
existing Terminal Chat and manager-test review path.

See the [verification record](target/verification/20261113-scheduled-sftp-rsync.md).

## Reference baselines

- Java 2.26.0 at `1ebbe724ee84`. The pinned [documentation manifest][java-manifest]
  maps menus, settings, and tool flows to Java sources and screenshots. Use its
  English pages, then confirm behavior in the running reference application.
- SithTermFX v1.2.7 at `8b02de38b7c6396f1af2558b0b00477cfb9a2871`. Start with
  sections 11–13 of the [integration guide][sith-guide] and
  the [v1.2.7 and v1.2.6 change entries][sith-changelog]. Use the separate pinned
  checkout when generating fixtures or running Maven.
- The pinned Java commit is available in the sibling repository's object
  database. Use the commit ID when reading the reference because the checked
  out Java files may be at a different revision. The separate pinned
  SithTermFX tree is the reference for core and UI replay.

## Follow-up to the baseline

- [The CI run for the handoff commit][handoff-ci] passed on macOS and stopped
  at Clippy on Linux and Windows. The working tree fixes the platform-specific
  OpenPGP helper and test setup, and boxes the log file manager enum variant.
  Windows CI uses a disposable profile and keeps Unix scripts on LF.
  [The latest pushed run][head-ci] at `f463f9c` shows the same failures. It
  predates the working-tree fixes and the local checks recorded below.
- The AI Manager keeps the skill actions below the scrollable form and bounds
  the status area beside the dialog actions. Layout regressions cover editor
  growth, wrapped actions, and long model identifiers at compact sizes.
  Native resize, scaling and monitor transitions remain acceptance cases.
  AI Skills imports a batch of up to 32 Markdown files from its file chooser
  or from absolute paths.
  It reads the full batch before updating the library draft, clears the
  search, and selects the last imported skill. A read or capacity error
  leaves the library unchanged.
- The AI Manager verifies the exact runtime index bytes with the pinned Ed25519
  key, checks host compatibility and the archive SHA-256, and extracts the
  package into a private installation. A bounded `--version` check precedes
  activation. Before launch the installed tree is compared against the signed
  archive again. The replay fence and signed withdrawals survive restart. The
  first managed chat test confirms the new runtime. A failed first startup
  restores the previous verified runtime or deactivates the failed one.
- The Local Models page registers private single-file GGUF copies. Regular
  profiles select a registered model and acquire a supervised runtime lease
  when sending a text request. The kernel must attribute the exact accepted
  TCP connection to the runtime process before it receives credentials or
  prompt data. Overlapping leases for the same model share its process.
  Start, Stop, Configure, and confirmed Remove manage the resident process
  and its saved settings. The default ten-minute idle timeout unloads the
  model tensors until the next request. Online updates, external and multipart
  model files, managed vision, and MLX remain open. Knowledge stores can use
  a separately registered managed GGUF with the Embeddings purpose. Its
  private index and request preview identify the selected model, and the
  dedicated authenticated server uses the pinned embeddings route.
- A whole-tab recording follows a live tab across windows and includes new
  splits. It captures visible IBM cells through the same screen path as local,
  serial, and SSH panes. Playback shows up to four panes per page with a common
  timeline. An exported excerpt includes the state of each pane visible at its
  start. Java recording interchange and real IBM host acceptance remain open.
- Saved serial connections have native Connection Manager and Quick Connect
  paths, share the connection ID space with SSH, and enter Open Recent after a
  successful connection. The pane retains its serial settings for reconnect.
  Serial groups, tags, favorites, toolbar pins, and Java interchange remain open.
- Stored PRE analyses can be exported as a single PDF, a combined PDF, or
  PDF entries in a ZIP. A4 pages reuse the journal's embedded fonts and export
  branding. The current renderer includes report bookmarks and a full-source
  option. Saved flowcharts reopen without an AI request and export as images
  in PDF and HTML or as bounded Mermaid in Markdown and JSON. Combined PDF
  contents show the final chapter page numbers and link to those pages.
  Wide diagrams use landscape A4 when the full image fits more than 15
  percent larger. Report content resumes in portrait. Native vector charts
  on report covers use the exported severity, dependency, and apply-result
  counts.
  HTML shares the configured watermark and footer, and Markdown shares the
  footer. The full-script appendix preference starts off and is shared by
  the single-report dialog and Export settings. Combined and ZIP dialogs
  start from the saved value and keep their changes local.
  Markdown exports include native PNG companions for renderable stored
  diagrams in all three packings and retain the bounded Mermaid source.
  Standalone and combined companions are published without replacement
  before the Markdown commit. Ordinary failures roll back owned assets.
- A single snippet supports selecting improvements and dependency changes,
  previewing the AI request, and reviewing a persisted proposal. Acceptance
  updates the editor draft. Rejecting or deferring a proposal leaves the editor
  intact. Verification uses the original analysis profile and classifies
  resolved, still-open, and new items. After reports in HTML, Markdown, JSON,
  and PDF retain the reviewed changes and any verification of the exact
  accepted source. A verification of subsequent edits is kept separately.
  Folder apply previews each affected file request with the analysed
  project as context. Review displays successful file diffs and failed
  proposals separately. Acceptance atomically saves the explicitly selected
  existing snippets after checking source identity, revisions, and editor
  drafts. Folder proposals are held in the open review. Interrupted-stage
  recovery and persistent folder After evidence remain open.
  Single-snippet apply keeps validated stages in the open analysis window
  when generation stops or fails. Resuming previews the remaining request
  and requires a new approval with the same source, analysis, and AI
  configuration. The completed proposal still requires diff review and
  acceptance. Resuming after a restart remains open.
  The AI Manager's default code text language is included in the reviewed
  single and folder apply prompts. It can preserve existing prose or request
  a specific language for comments and messages. Change reasons follow the
  report language. New stored apply runs carry the reviewed language policy
  into After reports and verification of the exact accepted source.
  Apply reasons can now open and highlight a uniquely anchored block in the
  proposed or reopened After diff. The model can supply a bounded changed
  source line. A legacy finding can link through its original line only
  when the source revision still matches. After exports use the same
  resolution rules to associate findings and dependencies with changed
  hunks. JSON records both directions, HTML links from hunks to the report
  cards, and Markdown and PDF include the corresponding IDs and reasons.
- Knowledge Stores keep private, atomic indexes bound to each store's
  sources and embedding configuration. Reopening requires consent for a
  search. Manual folder sync previews a bounded scan and records the
  approved files and content hashes. Changed content requires another
  review before embedding. Updates reuse vectors for the same store,
  embedding endpoint and model when the path, full file hash and passages
  match. New and changed files are embedded, and confirmed removals are
  dropped when the updated scope is saved. Source availability is saved per
  file or folder. Disabled sources retain their configuration and existing
  vectors, and are excluded from retrieval, file reads, and embedding.
  A rebuild retains disabled vectors with compatible dimensions. Previously
  saved snapshots remain readable, and a newer snapshot can represent a
  source that has yet to be indexed. A stale AI preview rechecks the saved
  source configuration before sending. Index publication and settings
  replacement share the profile lock. Use Rebuild index after replacing
  a model under the same service name. Automatic sync, additional workloads,
  HNSW, and Qdrant remain separate gaps. A saved store can be bound
  independently to the assigned Coding profile for PRE snippet and folder
  analyses and to the assigned Text profile for Terminal Ask and chat
  follow-ups. Each path has separate approval for the bounded embedding query
  and for the exact model prompt with cited excerpts.
- The geometry preference now gates restoration and recording for named
  dialogs. Global Settings search keeps its results scrollable over the
  section body and preserves the footer. Global Settings and Quick Connect
  keep their initiating owner. The workspace routes focus and close requests
  to the dialog. On Windows the two dialogs have a native HWND owner and close
  before their owner. Attached dialogs disable native input on their owner and
  restore it after the last attachment releases. The application routes input
  while the dialog opens and when the native hook is unavailable. Interactive
  desktop acceptance remains open.
  The Settings search now handles result navigation, Enter reveal and
  Escape clearing within the focused input. It scrolls the selected row
  into view in the settings section.
  About follows the pinned Java application's invoking-window ownership
  and application modality. It uses the shared native placement path,
  returns focus to its owner after dismissal, and closes before its owner
  during teardown.
  Quick Connect handles Escape in its dialog view even while a text field
  has focus. It uses the regular Cancel path and returns focus to its
  owner. An active overlay can consume Escape first, and saving retains
  its existing cancellation guard.
- Global Settings has a searchable Snippet Editor section. Font family,
  font size, foreground and background can each inherit the terminal setting
  or use a snippet-specific override. Save applies the draft to hosted and
  standalone snippet editors. Cursor shape and color can independently
  inherit the file editor settings or use snippet-specific values. Cancel
  discards the draft.
- The native file editor inherits the terminal font, size and colors, with
  editor-local zoom. Global Settings exposes the pinned red block default,
  line and underscore shapes, and an editable cursor color. The Iced editor
  keeps ownership of focus, selection, IME, input and layout.
- Xterm, the six DEC VT profiles, Sun CDE, SCO ANSI, and DEC CTERM now answer
  primary and secondary device identity queries like the pinned core. The
  supplemental Java replay and the Rust stream tests passed the device
  replies, DEC graphics, cursor report, and keypad and cursor mode case.
- The six DEC VT profiles materialize blank cells with active attributes up
  to a horizontal tab stop. The shared pinned replay covers custom and
  cleared stops, existing cells, cursor replies, and pending wrap.
- ANSI `CSI 2 J` archives the visible primary page on the ten ANSI profiles
  that share the parser. The pinned replay checks retained rows and styles,
  cursor replies, and isolation of the alternate screen.
- PETSCII matches the pinned core for the covered right-margin editing and
  cursor transitions. The replay compares thirteen byte sequences and
  17 explicit key encodings. It includes both character modes and blank
  cells after reverse-mode edits, clear and scrolling by Return or wrap.
- HP 2392 native insert and delete line and HP 700/92 ANSI insert and delete
  line preserve the cursor column. The pinned captures check the screen and
  cursor, including the HP 700/92 cursor position replies after both edits.
  Both HP stream profiles also fill tab gaps with active attributes, retain
  pending right-margin wrap, and put the visible page in scrollback on the
  two HP page clears and ANSI `CSI 2J`. The separate HP 2392A block-form path
  keeps its existing behavior. The pinned Java replay generated the shared
  screen and scrollback fixture.
- TVI 910, 920 and 925 fill gaps up to the next tab stop with styled blanks.
  A second tab preserves text and previously materialized cells. The shared
  Java capture covers reverse and underline attributes, a distant stop and
  the cursor state after a right-margin tab. The pinned wide replay covers
  fragmented UTF-8 cursor addresses and the one-past-right cursor reply on
  132 columns. A tall replay covers both UTF-8 cursor operands and scroll
  bounds in TVI 920 and 925. Single-byte serial sessions retain raw protocol
  coordinates.
- The ANSI text-grid size query works. Remote `CSI 8` resize remains open.
  The pinned Java path updates the terminal view, and Rust needs to route the
  request through the pane geometry owner so terminal, rendering, and
  transport sizes stay consistent.
- ANSI conformance selectors `ESC SP L`, `ESC SP M`, and `ESC SP N`
  restore the pinned G0 and GL selection. L and M also designate DEC
  Supplemental in G1 and select it for GR. The replay covers all ten ANSI
  profiles and checks that the DEC control width stays in force.
- ANSI modes `CSI ?47` and `CSI ?1047` start a fresh alternate page after
  returning to the primary page. Repeated activation preserves the live
  alternate page. The pinned replay covers page contents, primary
  scrollback, and mixed mode switches across the ten ANSI profiles.
- Xterm, Sun CDE, SCO ANSI, and DEC CTERM use the same horizontal tab
  behavior as the six DEC VT profiles. The shared ANSI path preserves
  occupied cells and a pending wrap, and fills unused columns with the
  active attributes. The pinned tab snapshot covers all ten profiles.
- Wyse 50, 60, and 160 have a shared pinned replay for 24 byte streams and
  three key tables. The right-margin cases exercise movement and editing
  while wrap is pending. The fixture records deliberate WY-50 insert-mode
  and WY-60 wrap-off differences. All three fill tab gaps with active
  attributes, WY-50 does so for its escaped tab as well, and WY-160 accepts
  a single-row scrolling region in its native margin command. Wide
  addressing accepts UTF-8 coordinates in cursor, field, and margin commands.
- Windows directory exports keep the staging and parent handles through
  atomic publication. The same handle-based rename restores staged journal
  directories. A short retry covers a transient open descendant while
  preserving no-replacement behavior. A native probe reproduced error 5
  with an open descendant and successfully published the same staging
  directory after the handle was closed.
- A regular SFTP download to a new local path can resume after connection
  loss, including after closing and reopening the window in the same
  application run. One private partial file of up to 1 GiB is retained.
  The reopened window must use the same SSH route and verified peer key.
  Resume checks the authenticated peer key, remote size and
  modification time, and the last 64 KiB on the same remote handle before
  appending. Publication verifies the local stage and uses a no-replace
  operation. The 64 KiB overlap follows the pinned Java behavior. Earlier
  changes with identical reported attributes and overlap can evade it.
- An SSH pane now admits fourteen forwarding streams while reserving slots
  for the shell and one SFTP channel. Four remote listener registrations can
  coexist with that load in the process worker.
- The offline locked `kortty-ssh` normal and build dependency graphs resolve
  for ARM64 and x64 on Windows, macOS and Linux. The selected cryptography
  providers are documented in the [Sunset patch](vendor/sunset/KORTTY-PATCH.md).
  The transport closure excludes `ring`, OpenSSL, `aws-lc`, libssh and
  libsodium. The RSA/SHA-2 provider adds `ctutils` and the approved `cmov`
  selection primitives documented below. Independent SSH security review
  remains a release gate.
- The serial connection dialog can override the global default with UTF-8,
  ISO-8859-1, ISO-8859-15 or Windows-1252. Layout and Java project restoration
  preserve the override. The codec handles serial text and status titles at
  protocol boundaries. PETSCII keeps its native encoding. DEC sessions reserve
  the C1 range for control sequences when eight-bit controls are active.
- The Scheduler persists a remote working directory for shell actions. The
  same quoted directory change applies to commands, scripts and shared
  snippets, including batches and PTY jobs. A failed change stops the action.
  The SFTP folder browser is available for one saved SSH target. It selects
  working directories and remote path fields of the supported SFTP actions.
  Changing a deletion path clears its previous confirmation.
- AI Manager search finds the existing fields in General, Profiles, and
  Local Preferences. Enter or a click reveals the result and scrolls to its
  highlighted field. Profile results reflect the selected connection type.
  The focused Skill and Memory Markdown editors use the shortcut for their
  own literal find. The Skill Markdown editor resizes with the panel open.
  AI Memory compares the current file with the proposed content before
  clearing, restoring a version, or archiving selected managed entries over
  180 days old. Cleanup preserves user-written text and unchecked entries.
  Reviewed writes compare both bytes and metadata, and cooperating memory
  writers share one lock across the active files and archive.
- [Terminal AI selections][java-terminal-ai] open temporary native chat tabs
  for Ask, Summarize, Solve Problem, and Summarize Recent Output. The latter
  uses the last finished command when available and otherwise a bounded
  primary-buffer tail. Masking joins soft wraps and omits hidden cells.
  Each request previews the exact role-tagged messages and the chosen local
  destination. Ask and follow-up reviews can include excerpts from a
  selected store bound to the Text profile. The first approval sends the
  masked question to the embedding model. The second shows and sends the
  complete chat request. The worker checks the reviewed source hashes and
  index before sending. Retrieved prompt context is excluded from the
  persisted conversation and replay history. Follow-up requests keep
  bounded history. Closing the tab
  cancels the transport. Explicitly saved conversations use private native
  JSON records. Follow-ups update the same saved record. The Saved Chats
  manager opens, renames, deletes, and refreshes those records in a modeless
  window or hosted tab. Mutation checks cover revision and file contents.
  Reopening applies current redaction and requires an explicit replacement
  if the original profile is unavailable. The manager imports and exports
  ordinary Java `ai-chats.xml` libraries with explicit confirmation and
  current masking. Imports keep message order and optional terminal context.
  Assistant reasoning remains local to the transcript and XML exchange.
  A conversation's response language appears in the reviewed request and is
  retained through save, reopen, and Java XML interchange.
  They preserve local edits on duplicate IDs and require an explicit native
  profile choice before continuation. Java attachments, agent and tool
  histories, and per-message metadata remain outside this interchange.
  Swarm chats, streaming, cloud and CLI providers, and terminal actions from
  responses remain open.
- Windows native ownership covers Close All Tabs, terminal link mismatch
  confirmation, and tab and group naming. Pending child opens are discarded
  after cancellation, and tracked dialogs close before the owner.
- On X11 the owner-scoped dialogs set their exact transient parent and
  modal state before mapping. On Wayland the pinned Iced patches set the
  exact mapped `xdg_toplevel` parent before the first renderer buffer and
  acknowledge creation cancellation if the parent closes while queued. See
  the [Wayland trace procedure](vendor/iced-winit/KORTTY-PATCH.md). Native
  compositor and X server acceptance remain open.
- On macOS the same owned dialogs attach to their AppKit parent before being
  shown and detach on the main thread before closing. The application gates
  owner input. Workspace cross-Clippy passed for `aarch64-apple-darwin` on
  Windows. Interactive verification requires a macOS WindowServer.
- Standalone AI Manager has its initiating main window as a modeless owner.
  Reopening restores and focuses the same manager. Only closing that owner
  checks the manager's unsaved state before closing the native child.
- Standalone Known Hosts attaches modally to the invoking window. Each main
  window keeps its own dialog instance. Closing the dialog restores the
  initiating window's focus, and forced teardown closes the dialog first.
- Run now and Run with successors save the current Scheduler draft before
  checking dependencies. A failed save leaves the draft available for editing.

### Rust verification on Windows ARM64

On 2026-11-08, the working tree passed formatting, workspace Clippy with
`-D warnings`, and the locked workspace test run on Windows ARM64 build
26200 using Rust 1.99.1 and MSVC. The test run used a disposable profile and
reported 1,562 passed and six ignored. It covered the compact AI Manager
layouts, independent and durable knowledge indexes, named dialog geometry,
settings search, stored and exported analysis flowcharts, native ConPTY,
and the terminal reference replays. An earlier run exposed CRLF conversion
in embedded Unix scripts. The Git attributes now preserve the LF bytes in
those scripts and in the byte-compared HP and TeleVideo snapshots.
After a journal test timeout, the background-tab fixture was changed to wait
for actual worker replies and to read the journal after the writer drains.
The revised test passed three isolated runs and the final workspace run.

The follow-up for reviewed apply, verification, After reports, and native
dialog ownership passed formatting, Windows ARM64 workspace Clippy, and
the added regression tests. Its full test run reported 1,570 passed, two
failed, and six ignored. Both failures were in the existing directory
export cases under `snippets::archive::tests`. One reported Windows error 5
during the directory export. The other reproduced on the second serial
run of the three folder export tests. The earlier ConPTY dashboard timeout
passed in isolation. The closing-capture metadata test now awaits the
actual save response and passed in the next full run.

The follow-up for native owner input blocking, folder knowledge review,
PETSCII, and handle-based directory publication passed formatting and
workspace Clippy with `-D warnings`. The full Windows ARM64 suite reported
1,580 passed, zero failed, and six ignored. It included the journal rollback
case that had failed to restore a staged directory in an earlier run. The
four folder export tests and the journal rollback test also passed 30 serial
repetitions. The two knowledge review cases and the queued dialog close case
passed a separate focused run.

The next Windows ARM64 workspace run passed 1,587 tests with six ignored.
It includes the Settings keyboard and measured-scroll cases, PRE knowledge
retrieval with a loopback embedding and analysis fixture, the Wyse replays,
and the live ConPTY transfer regression. A second real ConPTY receives
broadcast input when the first ConPTY's input queue is full.

The subsequent Windows ARM64 run passed formatting, workspace Clippy with
`-D warnings`, and 1,593 tests with six ignored. It covers the Snippet Editor
appearance draft and preference round-trip, SFTP resume with verified peer
binding, the refreshed HP captures, and the PETSCII scroll cases. The focused
HP and PETSCII run passed all twelve tests.

The next Windows ARM64 all-targets run passed 1,600 tests with six ignored.
The focused file-editor run passed 35 tests. The rendered cursor test
compares its anchor with an unmodified Iced TextEditor, checks block glyph
contrast and verifies that a selection hides the caret. The serial tests
cover all four encodings, fragmented input, protocol operands and control
bytes, settings save and cancel, and Java project import. A physical serial
device was unavailable on this host.

The next Windows ARM64 all-targets run passed 1,605 tests with seven ignored.
The first run encountered Windows error 5 once during the existing editor
overwrite test. Twenty-four isolated repetitions and the full rerun passed.
The knowledge update regression uses a local HTTP test service to verify the
submitted passages, reject mismatched vector dimensions, retain the snapshot
identity for an unchanged update, and require an explicit rebuild after a
corrupt snapshot. The Snippet Editor test covers independent cursor overrides,
older profiles, and Save and Cancel.

The ignored scheduler test ran separately on Windows ARM64 against a private
Win32-OpenSSH 10.4.0.0p1-Preview daemon. It selected an actual SFTP directory
with an apostrophe in its name and verified that the job action stored the
server's path. DEBUG3 confirmed that the mismatching host key ended the first
connection before `SSH_MSG_USERAUTH_REQUEST`. A separate local Git Bash run
checked shell quoting and the exit before subsequent commands on failed `cd`.
The POSIX OpenSSH job execution cases still require a native Unix test host.

The native build environment uses Rust and Cargo 1.99.1, Visual Studio Build
Tools 2022 17.14.44 with ARM64 MSVC, and Windows SDK 10.0.26100.0. Standalone
C and Rust ARM64 programs compiled and ran. The locked offline `kortty` and
`kortty-cli` builds succeeded, and the native CLI printed its help.

The subsequent Windows ARM64 workspace run passed 1,615 tests with seven
ignored. The run covered saved Serial storage, Quick Connect protocol switching
with a managed incognito block, interleaved playback and range export, and
offline archive staging with signed test fixtures. The first run encountered
an empty directory value in the existing terminal logging format test while
the Linux cross-check ran. The Windows run without the cross-check passed that
test. Formatting and Clippy with `-D warnings` passed on Windows ARM64. The
`x86_64-unknown-linux-gnu` cross-Clippy check passed. Physical serial acceptance,
a release-signed archive import, and native Linux runtime checks remain open.

The next Windows ARM64 workspace run passed 1,622 tests with eight ignored.
The whole-tab recording test moved live ConPTY sessions between windows, added
a new split, and checked that the replay contained every pane. The runtime
tests covered GGUF registration and leases, authenticated ZIP extraction,
tampering and path rejection, the version output bound, and keeping the
previous activation pointer after a failed version check. The published stable
release `llama-b11727-kortty2` supplies Windows x64 runtime packages.

The release-signed smoke passed under Windows x64 emulation on ARM64 build
26200. It imported and activated `llama-b11727-kortty2-windows-x86_64-cpu`,
reopened and reverified the installation, and received a response from
`stories15M-q4_0.gguf` through the managed loopback process. The test verified
first-launch confirmation and removal of the one-time credential. The ZIP
SHA-256 was `2d10cbb02a4aa05f4e756bbf16c297b5387c379f55721baf509c1379cb3c1c2f`.
The GGUF SHA-256 was `66967fbece6dbe97886593fdbb73589584927e29119ec31f08090732d1861739`.
Native Windows x64, native ARM64 runtime packages, and macOS and Linux
managed model acceptance remain open.

The following Windows ARM64 workspace run passed 1,626 tests with eight
ignored. The profile tests covered saved managed IDs, legacy external profile
compatibility, and inclusion in coding workflows without an external URL.
The live socket tests verified exact accepted tuples and rejected a different
PID, a different source port, and wildcard listeners. The Linux x64 PTY crate
passed cross-Clippy. Native Linux and macOS socket and runtime checks remain
open.

The release-signed test then passed with the same package and GGUF under
Windows x64 emulation. Two ordinary profile sessions shared one runtime. The
second session continued after the first was cancelled, and another model
registration started a separate runtime while the shared process was active.
The final leases closed both listeners and the test found no temporary key
files. Native x64 acceptance remains open.

The resident lifecycle and SFTP window reopen changes passed 1,628 native
Windows ARM64 workspace tests with eight ignored. In a separate release-signed
x64 emulation run, llama.cpp reported its actual sleep state through
`/props` and reloaded the test model on the same process after the next
request. Explicit Stop closed both listeners. Native x64 runtime acceptance
is a separate check.

The AI Manager search, owned-dialog, Scheduler draft, and PDF contents
changes passed formatting, workspace Clippy, and 1,634 native Windows ARM64
workspace tests with eight ignored. Workspace cross-Clippy also passed for
`x86_64-unknown-linux-gnu`. The Iced layout regression verifies the
highlighted profile field inside a measured 760 by 300 viewport. The PDF
regression checks internal link targets after a multi-page contents section
and report. Native interactive window acceptance remains open.

The next native Windows ARM64 run passed 1,638 workspace tests with nine
ignored. It covered managed embedding routes and index scope isolation,
mixed-orientation PDFs, and SFTP action directory selection. Workspace
formatting and Clippy passed on Windows. Workspace cross-Clippy passed for
`x86_64-unknown-linux-gnu`, including the X11 modal tests. Both
release-signed llama.cpp tests passed under Windows x64 emulation on ARM64.
The managed embedding run used the same verified runtime ZIP and GGUF
fixture recorded above. It indexed a file with the authenticated embedding
route and reopened the saved snapshot for cited retrieval. The chat run
checked continued support for independent processes and shared leases.

The extended Scheduler browser test passed with a private native ARM64
Win32 OpenSSH 10.4p1 server. It selected the working directory and the two
remote paths of a rename action. The server saw no user-authentication
requests on the connection with a mismatched host pin. The confirmed
connection authenticated by public key and served the SFTP listings.
This run covered path selection. A subsequent execution test covers
drive-qualified directory creation and regular-file download.

The native Windows ARM64 execution test passed against Win32 OpenSSH 10.4p1.
It verified repeatable directory creation, exact download bytes,
preservation of an existing local target, and rejection by policy before
opening an SSH connection. The checked download accommodates the observed
synthetic drive-file mode differences only at the first handle check.
Subsequent handle and path checks keep their original metadata baselines.

The final native Windows ARM64 workspace run passed 1,646 tests with ten
ignored. It covered focused Markdown find, owned dialog lifecycle, PDF cover
statistics, and the measured SFTP mode pairs. The TCP retry regression also
passed after the SSH connector began rejecting loopback self-connections.
Workspace Clippy passed on Windows and for `aarch64-apple-darwin`.

The next Windows ARM64 run passed 1,654 workspace tests with ten ignored.
The Memory tests checked preview cancellation, archived and retained bytes,
restoration, a same-length edit with its original modification time, and
the comparison layout at compact and expanded heights. The report tests
covered escaped branding and the persisted full-script choice. The HP
replay matched both profiles against the pinned core. The isolated Maven
replay passed both tests with Microsoft JDK 21 targeting Java 17. The Git
attributes preserve the fixture streams byte for byte on Windows.
Formatting and Windows Clippy passed. Workspace cross-Clippy passed for
`x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`. The Wayland tests
were compiled for Linux, and the compositor trace remains open.

The subsequent Windows ARM64 run passed 1,663 workspace tests with ten
ignored. The terminal AI loopback fixture compared the reviewed role-tagged
messages to the actual HTTP request, exercised a smaller profile limit, and
observed socket closure when the chat tab closed. The recent-output tests
covered command marks, compressed history, hidden cells, and capture bounds.
The Windows owner test checked that the modeless AI Manager kept its native
owner enabled. The Markdown tests decoded PNGs for every packing and verified
rollback when the final report name was already taken. The Wyse Rust replay
matched the extended pinned Java capture. Formatting, Windows Clippy, and
cross-Clippy for Linux x64 and macOS ARM64 passed. Interactive compositor and
WindowServer checks remain open.

The next Windows ARM64 run passed 1,672 workspace tests with ten ignored.
Saved Chats coverage includes explicit save, follow-up updates, reopening
with a missing profile, current masking, rename and delete, external edits
with an unchanged revision, and asynchronous completion after close. The
Known Hosts lifecycle test exercised two owners and close ordering. Apply
tests checked persisted source anchors and jumps after reopening. The ANSI
erase replay matched ten profiles against the pinned SithTermFX core. The
isolated Maven reference run passed five tests. Formatting, Windows Clippy,
and cross-Clippy for Linux x64 and macOS ARM64 passed. Native compositor and
WindowServer interaction remain separate acceptance cases.

The Windows ARM64 toolchain check built the full workspace with Rust 1.99.1,
MSVC 14.44, and Windows SDK 10.0.26100.0. Rust and C probes compiled, linked,
and ran for ARM64 and x64 on that host. The workspace CLI returned its
version successfully. The pinned TeleVideo Java replay passed both tests
across all three profiles.

The pinned Java 2.26.0 chat manager generated the
[JAXB fixture](src/workspace/saved_chats/fixtures/java-2.26.0.xml).
The Windows Rust test imported it and exported a new library. The pinned
Java manager loaded, saved, and reloaded that exact Rust output. It preserved
chat metadata, Unicode, escaped content, and assistant-first, user-only, and
consecutive-role transcripts. Message timestamps in the Rust output use the
chat creation time, and individual message profile hints are omitted.

The final Windows ARM64 suite passed 1,684 tests with ten ignored. It
covered XML transfer, modeless Recording Manager ownership, After report
hunk links, and the wide TeleVideo replay. A transient Windows editor
replacement failure reproduced in two of 72 parallel runs. The editor now
rechecks the saved snapshot before retrying a Windows sharing denial.
The final binary passed all 72 replacement runs and 24 simultaneous-save
runs. Formatting and workspace Clippy passed on Windows ARM64.
Cross-Clippy passed for Linux x64 and macOS ARM64.

The Windows ARM64 desktop launch check measured the actual Win32 client area
at 96 DPI. A fresh profile opened at 1000 by 640 logical pixels. Restarting
with saved dimensions of 872 by 538 opened at that exact size. Both isolated
application processes closed normally. The modeless ownership check also
used real Win32 windows. Interactive manager and viewer actions remain
separate acceptance cases.

The next Windows ARM64 suite passed 1,688 tests with ten ignored. The
Logfile Manager tests checked modeless ownership, same-window viewer reuse,
close order, and unsaved badge protection. The folder apply loopback test
checked per-file request review, partial proposal failure, selected-file
publication, stale revisions, and cancellation after an AI configuration
change. Terminal chat tests checked the selected language in the reviewed
and transmitted request, save and reopen, and stale browser actions.
Formatting and Windows Clippy passed. Cross-Clippy passed for Linux x64
and macOS ARM64. The final Windows executable repeated the measured
startup and geometry restoration check.

The next Windows ARM64 suite passed 1,693 tests with ten ignored.
Coverage includes owned Terminal Effects modality, the shared code text
language in apply previews and outbound requests, and the pinned ANSI
conformance selectors. Windows formatting and Clippy passed, as did
Cross-Clippy for Linux x64 and macOS ARM64.

For an independent PDF check, run
`cargo test --locked -p kortty --bin kortty report_formats_use_the_stored_flowchart_and_surface_rendering_failures`
with `KORTTY_SAVE_ANALYSIS_PDF_FIXTURE=1` and a private temporary directory.
Poppler rendered both A4 pages. Text extraction confirmed the report text
and the diagram title on the page containing the embedded flowchart. The
rendered pages were inspected for the diagram, caption, and footer placement.

The native development executable opened a responsive main window with a
fresh profile and exited with code zero. The Windows ARM64 LPAC kernel probe
also passed. It exchanged bytes through the private pipe and denied private
files, inherited handles, other TCP connections, and child creation.
Interactive desktop acceptance remains open.

Clippy also passed with `--target x86_64-unknown-linux-gnu` on this Windows
host. Linux runtime tests, native Wayland and X11 acceptance, and CI for the
updated tree remain separate checks.

### Windows ARM64 live window transfer

On Windows 11 ARM64 build 26200, a disposable debug build opened two real
Iced windows in a fresh profile. The probe invoked the normal pane and
window commands programmatically. It opened three ConPTY shells in two
nested splits, set ratios of 0.43 and 0.62, and focused the original pane.
It canceled the move chooser once, then used the same move action as the
chooser while the original shell was emitting output.

All three shell PIDs, split ratios, and the focused pane were preserved. The
original shell kept its variable and returned the same PID after the move.
A marker emitted before the move was found in the retained history. The
target received native focus and rendered an 840 × 570 framebuffer at scale
1. Closing the tab and quitting reaped all three shell processes. The
source copy and profile were removed. This covers the Windows chooser path.
Physical drag, canceled mouse drop, and other desktop environments remain
open.

### Windows ARM64 release archive

The release was built with `--target aarch64-pc-windows-msvc` on Windows
ARM64 build 26200 with `RUSTFLAGS` set to `-C target-feature=+crt-static`.
`dumpbin /DEPENDENTS` on all three archived executables confirmed the absence
of dynamic CRT imports. The extracted CLI printed its help, the packaged
SSH worker reported readiness under LPAC, and the packaged relay connected
to its approved loopback test peer and stopped promptly on cancellation.
The extracted desktop opened its main window with a fresh profile and a
Windows-system-only PATH, then exited with code zero.

Two disposable Win32-OpenSSH ARM64 10.4.0.0p1-Preview loopback instances
verified a public-key login, PTY shell output, a byte-exact SFTP download,
and a jump to the second pinned host. The fixture used Command Prompt and
the archive's `sftp-server.exe`. The DEBUG3 logs show that the connection
with the mismatching host key ended after key exchange and before the first
`SSH_MSG_USERAUTH_REQUEST`. The LPAC kernel probe also passed on this host.
The packaged worker exchanged traffic on fourteen simultaneous forwards with
four remote listeners registered. It rejected additional local and remote
connections and opened an attached SFTP channel while the shell and all
fourteen forwards were still live.

The retained archive is `kortty-0.1.0-aarch64-pc-windows-msvc.zip`.
SHA-256 `e67aa21dc05daad7f408d61d3c7aa19c9d9bf86fb02e7b82c42a9458db90b8ab`.
This run used the Windows development host. Clean-system installation and
the other platform and architecture package checks remain open.

### Windows x64 release candidate on ARM64

The x64 release was built on the same Windows ARM64 host with
`--target x86_64-pc-windows-msvc` and `-C target-feature=+crt-static`.
All three archived executables have the x64 PE machine type and lack direct
dynamic CRT imports. The unpacked CLI printed help. The desktop opened a
responsive main window with a fresh profile and a Windows-system-only PATH,
then exited cleanly under x64 emulation.

The packaged LPAC worker exited before readiness with `0xc0000017` with
both the native ARM64 and emulated x64 test launchers. The relay test ended
before dialing. These checks establish the emulated startup failure and
leave the LPAC and relay results on native x64 Windows open.
The archive and checksum are retained for that exact follow-up.
SHA-256 `1deb295960ca63172f246ee6096e2c3de111ae70e464e94c0ffe7d370ac55b7f`.

### Fedora 44 ARM64 native verification

The 2026-11-08 runs used Fedora Rust 1.99.1, rustfmt 1.10.0, and Clippy
0.1.99. The first build at 3.8 GiB usable memory was killed by the kernel.
After the VM restart, Fedora reported 7.7 GiB. The repeat used the standard
development profile, one Cargo build job, a checksum-verified source copy,
and private scratch directories. Workspace formatting and Clippy passed.

The parallel suite exposed a transient saved-chat lock failure. The store
now explicitly unlocks its file before closing it, even when a forked
process retains the descriptor until exec. Reopening a chat also masks its
title before display while retaining the stored revision for the next save.
The lock regression, Java import, and reopen and autosave tests all passed
on Fedora. On Windows ARM64 the full suite passed 1,693 tests with ten
ignored under the normal user token, and workspace Clippy passed.

The Fedora workspace run covered 61 test targets, with 1,766 passes, 68
ignored tests, and one failed PTY test. The PTY test now uses a live
separate process for its ownership check because a neighboring Linux ID
can be a thread of the owning process. The correction changes only test
code. The follow-up passed all 18 enabled PTY tests and PTY Clippy on
Fedora. All 16 enabled PTY tests passed on Windows ARM64 as well.

The native Fedora development executable opened under a private Mutter
50.6 Wayland compositor with a 1600 × 1000 virtual monitor and Xwayland
disabled. A seeded profile requested 914 × 572. Client and compositor logs
showed the same titled xdg toplevel, configure and acknowledgement serial,
and a committed 914 × 572 shared-memory buffer for the main surface.
Executable SHA-256 `842e0503056afee448dfd9f768a175b82808540879e168abc724d37e3ff0eb63`.
Interactive dialog ownership, window transfer, and native X11 remain
separate acceptance runs.

The follow-up for About ownership, Markdown skill batches, and ANSI alternate
pages passed 1,771 tests with 68 ignored on Fedora 44 ARM64 and 1,697 tests
with ten ignored on Windows ARM64. Workspace formatting and Clippy passed
on both platforms. Cross-Clippy passed for macOS ARM64. The pinned
SithTermFX reference ran seven replay tests and reproduced the alternate
page snapshot byte for byte.

The explicitly enabled `x11_modal_hints_bind_only_the_exact_withdrawn_dialog`
test passed against a private Xvfb 21.1.25 server. It exercised real window
properties, including the exact transient parent, modeless and modal hints,
and rejection of a mismatched PID or replacement owner. Run it on an isolated
X server with Xauthority.

```sh
bash scripts/with-scratch.sh xvfb-run -a \
  -s '-screen 0 1600x1000x24 -nolisten tcp -noreset' \
  cargo test --locked -p kortty-windowing \
  x11_modal::tests::x11_modal_hints_bind_only_the_exact_withdrawn_dialog -- --ignored --exact
```

The current Fedora executable also ran in private Xvfb 21.1.25 and Openbox
3.6.1. With two main windows open, the command palette opened About and
Quick Connect from the second window. X11 reported the second window's
exact `WM_TRANSIENT_FOR` plus `_NET_WM_WINDOW_TYPE_DIALOG` and
`_NET_WM_STATE_MODAL` for each child. Attempting to activate the first
window during About returned focus to About. Escape dismissed About, and
the window manager close command dismissed Quick Connect. Both returned
`_NET_ACTIVE_WINDOW` to the second main window. One Escape press while
the initial Quick Connect text field held focus left that dialog open. The
focused Iced text input captures Escape to clear its own focus before the
workspace's ignored-event handler can dismiss the dialog.
The test used XTEST input, an isolated profile, and a 1600 × 1000 screen.
Executable SHA-256 `6e524ec2083e359b8a5531bf483937217280709cc1a86e4333e72c694b1865ea`.

The same executable passed native X11 tab transfer with three live local
PTY panes arranged as two nested splits. The test opened the Control API
through its per-run consent button in a private profile and sent the drag
through XTEST. Releasing over the free root area preserved the source
window. A second drag moved the tab to the target window while one shell
streamed output. The original pane IDs, shell PIDs, focused secondary pane,
and off-screen scrollback markers survived. The destination received X11
keyboard focus and the original secondary shell answered a typed PID probe.
The active stream emitted a new marker after the move. Save Layout captured
identical nested split trees and ratios before and after the transfer.
Native X11 compositor transparency and the complete desktop matrix remain
separate acceptance cases.

### Quick Connect on native Wayland and X11

The follow-up sends one Escape from the Quick Connect view after typing
into its initially focused SSH host field. On Fedora 44 ARM64 it closed
the dialog and returned keyboard focus to its invoking second main window
on both native Wayland and X11. The X11 run used private Xvfb 21.1.25 and
Openbox 3.6.1 and also confirmed the transient owner and modal hints.

The Wayland run used private Mutter 50.6, a separate session bus, and two
virtual outputs. A temporary `ApplyMonitorsConfig` call selected the active
output, and the compositor's virtual keyboard opened Quick Connect from
the second main window. The 1600 × 1000, 800 × 600, and 1600 × 1000 sequence
produced committed dialog buffers of 1100 × 760, 784 × 552, and 1100 × 760.
The private `dialog-geometry-quick-connect.json` retained 1100 × 760
throughout. In client and server logs, Quick Connect and About sent
`xdg_toplevel.set_parent` for that exact second main window before their
first buffer attach. About also restored the owner after Escape.
Executable SHA-256 `4e04876815ce3d32fed560ec62f5bc2ade2075a567c75973e06cf221aed4c3ce`.

The same executable reopened Settings at 1110 × 760, 784 × 552, and
1110 × 760 and kept its preferred dimensions. AI Manager opened at
1100 × 740 on the large output and 784 × 552 on the small output. Its
saved dimensions were still 1100 × 740 after closing it on the small
output. On returning to the large output, the sampled committed buffer was
1100 × 705. Capture the initial size hints, compositor configure sequence,
and size after the opening constraint is released when reproducing this
remaining AI Manager geometry case.

Workspace formatting and Clippy passed on Fedora and Windows ARM64. Fedora
passed 1,772 tests with 68 ignored. The Windows repeat passed 1,698 tests
with ten ignored, including the focused Quick Connect widget test and the
two-stage loopback apply test. The Windows apply fixture now checks for
unexpected requests on its original nonblocking listener. An earlier
Windows run reported a transient access-denied error on editor replacement.
The same workspace suite passed on the repeat. Seven pinned Java tests
confirmed the unchanged horizontal tab snapshot across all ten ANSI profiles.

### Remembered Wayland dialog height

Three native Fedora 44 ARM64 runs used the private Mutter 50.6 setup above
and alternated between 1600 × 1000, 800 × 600, and 1600 × 1000. The baseline
reproduced a 35-pixel content height loss in Settings in the first run and
in AI Manager in the third. The AI Manager trace started with equal
`xdg_toplevel` limits of 1100 × 740 and a first buffer of 1100 × 740.
The compositor then configured an outer size of 1100 × 740, and Winit
subtracted the 35-pixel client titlebar. The settled buffer and the size
saved on close were both 1100 × 705. Baseline executable SHA-256
`83fb95d149501b57d9d15d762cd8cfb407a1b55972dc17befe3f43797a193d31`.

The Iced shell now refreshes the equal size limits after Winit negotiates
client decorations and before the first renderer buffer. It restores the
requested inner size immediately if the initial outer size already matches
that request. The app releases the temporary limits after initial placement.

All three patched runs restored Settings at 1110 × 760 and AI Manager at
1100 × 740 on both visits to the large output. Both used 784 × 552 on the
small output while retaining their saved large sizes. AI Manager refreshed
its large-output limits to 1100 × 775 before the first attach. Mutter
configured that outer size, and the committed content remained 1100 × 740.
Every open set the exact invoking second main window as the native parent
before the first attach. Window-manager close returned keyboard focus to
that owner. Patched executable SHA-256
`84551fc8bf628e9cd85049b4ca50867e3a735ca55d5f95439d1a4763a6115441`.

On the same executable, Quick Connect opened at 1100 × 760, 784 × 552,
and 1100 × 760, keeping the saved 1100 × 760 throughout. The invoking
second main window was its native parent before each first attach. After
typing into the SSH host field, a single Escape closed the dialog and
returned focus to that owner on each output.

### Partial snippet apply and HP right-margin replay

An interrupted snippet apply with completed stages can save those stages
as a proposal for explicit review. The local review checks the exact source
revision and works with AI disabled. Reopening the proposal preserves the
completed findings and stage counts. Accepting it uses the regular snippet
save, and the After report and JSON identify the partial result. Recovery
of an in-progress single-snippet checkpoint in this first version was
limited to the open analysis.

The [HP right-margin capture](crates/terminal/tests/fixtures/hp/README.md)
matched the pinned SithTermFX core in all three HP replay tests. The new
streams cover native editing and CSI erasure, insertion, and deletion on
HP 2392 and HP 700/92, including HP 700/92 ANSI mode. A pending wrap preserves
the last styled cell and the next character still wraps. Explicitly
addressing the last column keeps it editable.

Windows ARM64 passed formatting, Clippy, and 1,701 workspace tests with ten
ignored. The editor save tests passed. Save errors now identify the failed
step and preserve the underlying operating system error. The earlier
transient access-denied failure still has no attributed cause.

Fedora 44 ARM64 passed workspace formatting and Clippy. The final test run
with the corrected report fixture passed 1,775 tests with 68 ignored,
including the interrupted apply review and export, the cumulative source
loss guard, and the HP replay. The transferred source manifest SHA-256 was
`da72c4b9a6e9187412a1b0bd87d77f95a6abd3e3969302aa82491eb57aa6033a`.

### Durable single-snippet apply and HP scroll regions

The pinned Java controller persists completed stages and reconstructs an
interrupted request against its exact source, selected findings, and stage
plan. The Rust apply worker now commits each validated boundary in the
private analysis history before sending another stage. It retains the final
boundary until it atomically publishes the review proposal. Reopening the
analysis rebuilds the approved plan from the exact editor source. The user
can review completed changes locally or explicitly approve the remaining
request for the currently configured model of the original profile ID.
The saved boundary records the model that produced its latest validated
stage. Provider routing and credentials come from current configuration. One
checkpoint per analysis record is retained until proposal publication or
explicit discard, up to four checkpoints per snippet.

The loopback workflow checks the persisted first stage while the second
request is live, then recreates the workspace and verifies that nothing is
sent until renewed approval. It also exercises local partial review with AI
disabled and verifies that publication consumes the checkpoint. The store
test covers changed source and plan, stale revisions, and idempotent full
proposal publication.

The [HP scroll-region capture](crates/terminal/tests/fixtures/hp/README.md)
matched all four pinned core replay tests. The new cases verify HP 2392
memory lock and HP 700/92 ANSI margins. Relative cursor moves cross the
region boundary. Explicit line edits can begin above the region and stop
at its bottom, while rolling and reverse index still honor the lock.
The native HP 2392A hardware-form path has separate control coverage.

Windows ARM64 passed formatting, Clippy, and 1,704 workspace tests with ten
ignored. Both reopened-workspace recovery workflows, atomic checkpoint
conversion, and the two HP scroll region tests passed.

Fedora 44 ARM64 passed formatting, Clippy, and 1,778 workspace tests with
68 ignored, including the same recovery and HP tests. The transferred
source manifest SHA-256 was
`b36bc2702eea64abf2393d18c957a2acc35b65c4c1ea75595d201543c74e1bf3`.

The native Fedora 44 ARM64 restart check used private Mutter 50.6, a seeded
PRE report, and a loopback provider. The old process persisted the first
validated stage while the second request was outstanding. The driver sent
SIGKILL, waited for that process to exit, and launched a new process on
the same isolated profile. The manager restored one of two stages. Opening
the preview sent no request. The explicit approval sent the same second
request body and skipped the first stage. The resulting diff retained all
four findings. The history consumed the checkpoint on proposal publication,
and the library kept its original source through acceptance. Saving the
editor changed the library to the reviewed source. Executable SHA-256
`5cecf88fc40d6c860f4148ed812e2831bf79328739c5dae2374f93a953961bdc`.

### AI Skills Markdown chooser and HP virtual cursor

AI Skills opens a Markdown chooser in the current AI Manager surface. It
lists folders and regular `.md` and `.markdown` files, accepts up to 32
selected paths, and keeps the existing absolute-path entry available. The
full batch is read before the library draft changes. Escape routes to
the chooser before the focused input consumes it, and an existing close
confirmation takes precedence. The shared Escape handler also serves Quick
Connect.

The [HP virtual-cursor capture](crates/terminal/tests/fixtures/hp/README.md)
matched five pinned core replay tests. The new 21 streams cover HP 2392
and HP 700/92 native and ANSI motion immediately past the right margin.
Vertical motion preserves the virtual column and left motion counts from
it. HP 700/92 ANSI cursor reports include the virtual column, and printing
with autowrap disabled resets to the first column of the same row.

Windows ARM64 passed formatting, Clippy, and 1,708 workspace tests with
ten ignored. The focused tests cover chooser filtering, bounded selection,
late list cancellation, batch atomicity, Escape precedence, Quick Connect
Escape, and the HP cursor cases.

The native Fedora 44 ARM64 smoke used private Mutter 50.6 sessions with a
1600 × 1000 virtual output and isolated profiles. The standalone manager
opened from the second main window. The hosted manager used the active
tool tab in that window. In both presentations a single Escape with focus
in the folder field closed the chooser and kept the manager visible. The
standalone run also kept a pending library setting change after cancellation.
It filtered out a text file, selected two Markdown files with different
extension casing, selected the last imported skill, and restored both
entries after Save and reopening. An invalid second file left the library
draft and the saved skills unchanged. Patched executable SHA-256
`d1602934ab939d130cf8bf3589750043263a70ac189f905b5ad2b44857904c4e`.
Fedora passed workspace formatting, Clippy, and 1,782 tests with 68 ignored.

### AI Skills batch export and HP last-row lock

AI Skills exports the selected entries as separate Markdown files to one
existing folder. The selection initially contains the current skill, or
the whole library when no skill is open. Single-skill export keeps the
absolute file path. Batch filenames are portable and receive a numeric
suffix for each existing or selected name collision. Existing files keep
their contents. The batch writer uses the same no-replace publication and
identity-checked rollback as Markdown report companions.

The pinned HP 2392 stream treats a memory lock on the last row as an empty
scroll region. The Rust replay now preserves the full page for scroll,
line feed, and autowrap. A lock one row higher still permits the actual
one-row region to scroll. Five pinned Java tests passed, including eight
new streams for the last-row case. Rust also checks grid resize and the
transition into HP 2392A hardware block mode.

Windows ARM64 passed formatting, Clippy, and 1,713 workspace tests with ten
ignored. The run covered bulk selection, unsaved form contents, native
filename checks, rollback, and the new HP cases.

The native Fedora 44 ARM64 Wayland check opened export with two skills
and no active skill. One Escape with the absolute folder field focused
cancelled without writing. The integrated folder chooser then selected an
existing directory. The batch wrote `smoke-alpha-2.md` and `smoke-beta.md`
and preserved the original `smoke-alpha.md` byte for byte. Both exported
files contained their expected Markdown, and the saved AI configuration
kept its original digest. Executable SHA-256
`7e0ecc6aebf7d1b8814371d872e1392beceda6157301a63bc13cfd73b127f38e`.
Fedora passed formatting, Clippy, and 1,787 workspace tests with 68 ignored.
The verified source manifest SHA-256 was
`732515127300e93dab619b1ad72e3416c195c2ec7dec72fbd365ad303be54b8f`.

### Terminal Ask knowledge and wide Wyse addresses

The Terminal Ask workflow test uses separate loopback embedding and chat
services. It checks that selecting a store does not send a request, that
canceling an active retrieval closes its socket, and that retrieval sends
only the masked question. The chat service receives the exact reviewed JSON
messages. Retrieved fields are masked before encoding. The test replaces a
source, an index, and the active AI configuration in separate runs. Each
change prevents the previous review from sending. The saved record and the
next prompt replay use the original conversation messages.

Three additional [Wyse streams](crates/terminal/tests/fixtures/wyse/README.md)
cover WY-160 wide cursor and margin addresses and WY-50 field addressing.
The pinned SithTermFX core passed the capture and replay on Windows ARM64
with Microsoft JDK 17 and Maven 3.9.16. The expected snapshot SHA-256 is
`2c3731a2c694dee610edcdda5fb31e7699959de720b6f8d8c5932bce44ef7a3d`.

The initial parallel Fedora run failed two existing application tests. Both
passed individually. Strict escape changes now take priority before the
session actor selects subsequent PTY output, including inside a frame batch.
The focused test queues a Kitty packet after requesting strict mode and
checks that the packet is filtered. The journal metadata writer explicitly
releases its file lock before closing the descriptor. Its regression test
retains a duplicate descriptor during the next edit. The manager test
checks the real worker result and the saved XML.

The corrected workspace passed formatting and Clippy on Windows ARM64 and
Fedora 44 ARM64. Windows passed 1,717 tests with ten ignored. Fedora passed
1,791 tests with 68 ignored, including both previously failing tests in the
parallel suite. The verified Fedora source manifest SHA-256 was
`2adc7368bbe7251a1cc57170aee38ce0db100e71be0c7d6228dd4b3136a70887`.

### Connection Manager ownership, network map, and SCO/Sun keyboard modes

Each main window opens its own Connection Manager. Connection drafts and
selection are local to that manager. Committed changes refresh other managers,
and a stale draft cannot replace a newer connection. The editor and exchange
dialogs belong to their manager. Connect and SFTP wait for the native close
event before opening the selected connection in the initiating main window.
Shell Integration Setup is modeless and belongs to the terminal or Settings
window that opened it.

The Connection Manager's List and Map controls share the active scope,
search, selection, and inspector. The native map places this computer
beside colored folder zones, matches saved jump hosts by host and port,
and shows distinct jump and enabled tunnel layers. It supports selection,
double-click connection, zoom and fit, and folder moves with one-level
undo. The store checks concurrent edits before publishing a move.

A Fedora 44 ARM64 X11 run used a private Xvfb display with Openbox. It checked
`WM_TRANSIENT_FOR` and `_NET_WM_STATE_MODAL` for two managers, two simultaneous
Import dialogs, and the SSH editor. Owner close requests were gated and focus
returned to the corresponding owner when each dialog closed. The executable
SHA-256 for that ownership run was
`0f96aa4dea8f3c98aaccf349da930538ac22ec9337bad94880d419536df7687d`.

The final X11 run opened Map with three saved connections, including a
saved gateway and a tunneled connection. The rendered map showed both
colored folder zones, the orange jump route, and the teal tunnel route.
The test returned to List and repeated the native ownership checks.
Executable SHA-256
`a07433ff0db8cc376d1424581c3bc1c9e6178b8c5a232934d822db0a353c6d83`.
The inspected temporary screenshot had SHA-256
`be0da03572b3bb4a6e8b8ccdc2a2525bcc63f536c2f3245f15520db4d7b2207f`.

The pinned SithTermFX core matched 23 SCO ANSI and Sun CDE keyboard cases
covering profile-specific keys with modifiers and the Home/End keypad
transitions. The [keyboard fixture](crates/terminal/tests/fixtures/sithtermfx/sco-sun-keys.tsv)
has SHA-256 `7cacc0d524569db114d0c866d9ee46e803f6b0a8145e957d06dd6c7322f0c07f`.

Windows ARM64 passed 1,726 tests with ten ignored. Fedora 44 ARM64
passed 1,800 tests with 68 ignored. Both runs covered 61 workspace
targets, formatting, and Clippy. The final Fedora source manifest SHA-256
was `d77b6fdd151ef2808c6151dda82a5dab30c7e9a81d568cd992f4ee201b03bb12`.

### Connection Map PDF and HP ANSI scroll margins

Map's **PDF…** action exports a snapshot of the active Connection Manager
scope and search. The manager owns the native dialog, which offers an editable
title and A4 landscape or portrait. Jump routes are included and the Tunnels
switch controls tunnel routes. The file browser reviews the destination before
publishing and checks that a file approved for replacement has not changed.
PDF labels are searchable and the routes and folder zones are vector graphics.
Oversized zones continue onto subsequent pages between rows.

On Fedora 44 ARM64, a native X11 run with Xvfb and Openbox verified the
dialog's transient parent and modal state, then saved and visually inspected
the same 130-connection map in four A4 landscape pages and three A4 portrait
pages. PDF parsing found each connection once in both exports and confirmed
the custom portrait title, vector routes and the enabled tunnel. The disabled
tunnel was absent. The source manifest SHA-256 was
`4590f5af62a8c19e579eba4002e2b42cca7bcf920e9d815e8b156e1e01d59426`.
The executable SHA-256 was
`ca395657d58e4ef7b899fdbcdcfd2be1fb0623cee38ce021a0cdec9636b2f9a1`.
The landscape and portrait PDF SHA-256 hashes were
`ab2b6e371559ca8ece4270a870b31821a4346a1538c730ad51af83a2c3345673` and
`d9bb5163646bad53d6456811df58580c48c9601fed3c537c2df57525e0173f0c`.

The HP 700/92 ANSI replay covers equal scroll bounds, scrolling within the
one-row region, explicit line edits above the region and homing after a reset.
The new cases were captured and replayed on Fedora ARM64 against the pinned
SithTermFX core with JDK 17.0.21.0.2 and Maven 3.9.16. The new capture SHA-256
was `f4f90e20612f5ff1889fc28b174fb12425efc7fadab2ee0435a9c3afccb7d335`.
The corresponding expected-results SHA-256 was
`7224fb374a72d16714484edbe86f60281eee7cad03d3889037123978f65fe12c`.

The workspace passed formatting and Clippy on Windows ARM64 and Fedora 44
ARM64. Across 61 targets, Windows passed 1,729 tests with ten ignored and
Fedora passed 1,803 tests with 68 ignored.

### Shared Teamwork files, Map symbols, and HP softkey text

The Java 2.26.0 reference places shared connections in a separate namespace
and keeps them read-only. The Rust **Teamwork Settings…** dialog manages
enabled shared XML files and retains edits after cancellation of a close or
a failed save. The loader checks the regular file's identity before
publishing an entire bounded snapshot. A source's last good snapshot stays
available in memory if refresh fails. Source identity and the XML connection
ID jointly identify a shared row.

The Manager projects shared connections into All, Teamwork, and tag scopes.
A matching local folder keeps its own zone, color and drop target. Shared
nodes use a people symbol and dashed border in the canvas. The PDF uses a
dashed border and Teamwork legend. Both outputs use resolved built-in or
custom OS symbols. The PDF reuses each badge image resource and scales it to
fit the node. Its snapshot keeps the symbol captured before export.

The shared SSH sign-in uses the fixed source route, a local credential
selection, the normal host-key check, and the stricter of the source and
managed isolation floors. The source snapshot carries no imported credential
binding or local saved connection ID. Shared tunnel approvals and remaining
session preferences are open.

The HP 2392 and HP 700/92 softkey replay now counts decoded UTF-16 units
for UTF-8 text, including across fragmented input. Single-byte modes retain
their byte counts. The six new cases passed record and replay on Windows
ARM64 with the pinned SithTermFX core, JDK 17.0.21.0.2 and Maven 3.9.16.
The stream fixture SHA-256 is
`47b37efcb62222fc66f49de30a3555dfa7e5d7528bd7fcd65cdc14dff7c6c8af`.
The expected results SHA-256 is
`fbf99464ab097349cf08e6c195df4713d92d222a827fe7b97e1528931dec5d10`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64.
Across 61 targets, Windows passed 1,738 tests with ten ignored and Fedora
passed 1,812 tests with 68 ignored. The Fedora source manifest SHA-256 was
`f4081f5c35afc07332b2b5304bca3c4019fbd1b6d7e9e0d3636b98a0efbb1c40`.

On native X11 with Xvfb and Openbox, a disposable profile used a preconfigured
shared XML source and a local connection in a same-named folder. The PDF
dialog was modal and transient for the Connection Manager in both scopes.
Poppler recovered the separate Teamwork folder and legend in both PDFs,
the local and shared connections in All, and only the shared connection in
Teamwork. It counted two OS badge images in All and one in Teamwork.
Executable SHA-256
`f8ff695044c6fd8627937773ca0e1ab3131b361dbde0226e6e36449d91f1b1a2`.
The All and Teamwork PDF SHA-256 hashes were
`12dda5dfc6a729d99de972bb0f5d658e0d4fe46a9431fd2c38e234791b48b310` and
`96ae5986395b4a2927971f1c29302ddfcd68bac081fbf05c902ac59ef09bfeca`.

### Automatic Teamwork refresh and HP erase saved lines

The shared-file scheduler uses the shortest enabled source interval at startup,
or the configured default without an enabled source. It refreshes at startup
and waits the same delay after each scheduled refresh completes. A settings
save queues an immediate check without shifting the periodic deadline.
Earlier settings files default to 15 minutes. The interval editor accepts
1 through 1440 minutes.

The bounded private cache holds the last good definition for each enabled
source. Its format excludes local profile IDs, authentication bindings, key
material and logging paths. The loader checks the configured source location
and revalidates the entries before publishing them. Configuration writes and
cache publication are serialized. A refresh result from superseded settings
cannot replace the active catalog. SSH and SFTP recheck the selected shared
definition before consuming local credentials. A changed or removed definition
returns the user to selection. Enterprise feature denial blocks the workflow.
Custom-source restriction limits it to administrator-provided sources.

The pinned SithTermFX `CSI 3 J` cases cover HP 2392, HP 700/92 and HP 700/92
ANSI. The sequence clears scrollback and the visible page while preserving
the cursor and a pending right-margin wrap. A one-row terminal retains its
visible text, as in the reference. The Java pin was compiled with Microsoft
OpenJDK 21.0.12 using `--release 17`. All seven HP replay methods passed.
The new expected-results SHA-256 was
`925db5d67932734715e74bd6e93abda3b9c8c446f10d9a24664151ede6910de4`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. Across
61 targets, Windows passed 1,751 tests with ten ignored and Fedora passed
1,825 tests with 68 ignored. The Fedora source manifest SHA-256 was
`9d541cb1c4965149534ac37e2801b36267e0725bfa6d115872aa4f8b0d6e4d30`.
One Windows repeat hit the previously observed access-denied error in
the simultaneous editor-save test. Twenty isolated repetitions and the
final full suite passed. The source of the access denial remains open.

With native X11, Xvfb and Openbox, the application created the cache at
startup before opening a Connection Manager. After changing the fixture
source while the manager was closed, the one-minute schedule updated the
cache after 60.0 seconds. A fresh process started with invalid XML and
republished the identical last-good cache, SHA-256
`271ae5476a77cdeecf430b6e992f45b8e030d4f3cdbfa702ed1fe1c9cd250bbc`.
The manager's native PDF modal exported the retained shared entry, and
Poppler confirmed the older row was absent. Executable SHA-256
`36755a71ae792c16fa67ff01221b48f20ff81311e2979726b6696ca5e4290b30`.
Offline map PDF SHA-256
`77594d74effef27c0b08d06df65c378ee030c17353a164c28675bedb1a9d124d`.

### Managed and Git Teamwork sources and HP row positioning

The Java 2.26.0 policy reference defines `[[teamwork-source]]` with
`name`, `type` and `url`. Rust adds those declarations to the effective
source set at runtime, keeps their settings read-only, and selects the
managed entry if the same origin appears in both lists. With
`allow-custom-sources = false`, only managed sources can populate the catalog.
The profile retains its existing custom settings for a later policy change.

An enabled Git source runs the installed Git in a bounded private bare
repository. It follows the initially advertised default branch and refreshes
that branch after a force update. It checks the preferred root XML blob and
the legacy fallback. The same bounded XML parser serves file and Git sources.
The profile cache keys snapshots by typed origin and records the Git branch,
including for a repository without connection definitions. Version-one
shared-file cache entries migrate to version two. Removing a source or changing
its URL removes the previous snapshot from the active catalog. On normal exit,
the app cancels and waits for active Git subprocesses.

The explicit URL transports are HTTP, HTTPS, SSH and `git://`. The private
Git environment supports HTTP(S) repositories readable without authentication.
SSH uses local OpenSSH credentials and host trust. Live HTTPS and SSH repository
acceptance remains open.

The HP row-only address cases preserve the pending right-margin wrap when
moving the cursor vertically. An explicit column clears it. All seven Java
replay methods passed against the pinned SithTermFX core using Microsoft
OpenJDK 21.0.12 with `--release 17`. The updated expected-results SHA-256 was
`eca4b9919f581ee4e3ca8ef364642b3d2a09d908fd241390e3b87b406859154d`.

The native Fedora 44 ARM64 X11 run used a local Git 2.56 daemon, a separate
managed XML file, and a policy that allowed managed sources only. The app
selected the primary Git XML and recorded `refs/heads/managed-main`. It
excluded the legacy decoy and the custom source. After the daemon stopped
and the XML file was removed, a new process reused both snapshots with the
same cache SHA-256
`3564badd612e63391a0403d589750b5a3a589dc90b3a8bd6314843a988b94b3f`.
Poppler confirmed both retained rows in the map PDF and the exclusion of
the decoy and custom rows. The native binary SHA-256 was
`713f3116e053f6f60a6f57f120033afdd91321bb29d49e0812c872b2b9611e16`
from source manifest
`b2ccddd276b51edd8f562756d90befb41abbdcfa1f28d949ec3713688afcca42`.
The PDF SHA-256 was
`af29c74dcb59de84170747bd1ae90e4f8b3727ed883e05eebddd31ce41b17dc7`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. Across
61 targets, the default suite passed 1,765 tests on Windows and 1,839 on
Fedora, with 11 and 69 ignored respectively. The opt-in Git 2.56 loopback
passed on both platforms and covered branch tracking, a force update, and
legacy XML fallback. Fedora used a privately extracted `git-daemon`.
The final Fedora test manifest, after marking the daemon-dependent test
opt-in, had SHA-256
`46149bdd17550dc790e2525a79cfdd15ab157c324c9dc231e84f00feff1c727d`.

### Local Teamwork authentication and TeleVideo UTF-8 text

The Connection Manager exposes a local authentication default in the Teamwork
scope. Apply commits the choice for all manager windows. The local profile
stores only the method, a local credential or key identifier, and an optional
SSH username for key-based methods. A separate edit cannot overwrite a newer
saved choice without first discarding its stale draft.

The shared XML importer records whether the target declares a credential or key
reference and discards the foreign identifiers themselves. Cache version three
retains this presence information. Version-one and version-two snapshots keep
their routes and require manual authentication until a refresh establishes
eligibility. The default applies to the target only. Vault passwords must
match the effective username and host scope. The source host, port, and jump
endpoint remain fixed. The SSH form checks the source snapshot and the
preference again before submission and during host verification.

Temporary-key authentication accepts an explicit clipboard paste into the
shared login form. It uses a lease of 1 to 1440 minutes with a default of 60.
The key bytes stay in memory and can be reused for a retry of the same route
while the lease is current. Clipboard access is subject to managed policy.

The TeleVideo 910, 920 and 925 now preserve decoded Latin characters in UTF-8
streams while continuing to map the ASCII graphics shift. The five Java
TeleVideo replay methods matched the checked-in snapshots against the pinned
SithTermFX core. The Windows ARM64 TVI target passed all 11 tests. The new
UTF-8 snapshot SHA-256 is
`e83d7bfd3d9f7d353d48885e98dd9867d3ca482c411ae3aeaacd562b21ff8ead`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
default workspace suites passed 1,774 tests on Windows and 1,848 on Fedora
across 61 targets, with 11 and 69 ignored respectively. A regression test
covers two Connection Managers retaining a concurrent local store update
while a Teamwork authentication preference is saved.

The native Fedora 44 ARM64 X11 run saved a registered key and username,
then reopened the manager in a new process. The SSH form applied both to an
eligible target. A different row with a foreign key ID equal to the local ID
kept its source username and stayed on manual authentication. A separate run
changed the default to a temporary key and opened the corresponding paste
form with the effective username. The automated click on Paste was not
verified in the X11 run. The Rust tests exercise clipboard result handling,
expiry, and route-bound retry. The native binary SHA-256 was
`14fb71347f91a5f74ba0519e3a64000b9d139ef23bb0c3342280f8bbd5249e2f`
from source manifest
`1d71f4e54472c5f33242d26180cc5c2a1f4c6632553a2f89cd796b1a5cccddeb`.

### Teamwork favorites, vault unlock, and HP arrow keys

The Java 2.26.0 Connection Manager toggles all selected connections together.
The native manager uses one store transaction for local and shared favorites.
The star and shared row context menu update a profile-local mark keyed by
typed source and XML ID. The Favorites count and the list, Map, and PDF use
the active catalog. The store accepts up to 512 shared marks. Marks are
excluded from portable backups.

For an eligible Teamwork target with a Vault password default, a locked vault
opens the existing unlock dialog. The selected SSH or SFTP target waits for
that dialog to close. A successful unlock uses the current scoped credential
and can supply a username missing from the source. Cancelling offers manual
authentication when the source provides a username. The deferred launch checks
the preference, source, and policy again before opening a connection form.

HP 2392 and HP 700/92 use the pinned Java platform mappings for modified
horizontal arrows. The capture verifies 17 cases per model on each of the
Windows, Linux, and macOS encoders. All eight HP replay methods passed
against SithTermFX v1.2.7 with Microsoft OpenJDK 21.0.12 and `--release 17`.
The modified-arrow fixture SHA-256 is
`37aa78f16f47eb69490d46af9782b7973f170d5e3765ee84ff57445590639764`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,781 and 1,855 tests across 61 targets, with 11
and 69 ignored respectively. They cover mixed favorite transactions, map
and PDF scope, deferred vault unlock and cancellation, and the HP bindings.

The native Fedora X11 run used two XML files with the same connection ID.
The inspector star saved a favorite for one origin. After restarting, the
Favorites scope still showed one connection, and opening it selected the
expected host. The context menu action passed the Rust state test. The native
X11 attempts closed the popup without confirming a favorite write.

The X11 login form first rejected an explicit clipboard paste with clipboard
reads disabled. With clipboard reads enabled in the disposable profile, it
accepted a generated OpenSSH private key. The driver checked the form's
ready status and scanned the profile files and app logs for the key bytes.
The run stopped at the SSH form. The native binary SHA-256 was
`df2d2a697cb6d562056a42633c97f3bbebead7ae4ad60a68696dfe71f01928bb`
from source manifest
`e73818a170494ba691ac3497643a79429670c4e31a707aaf8b127e70814867d5`.

### Teamwork recency, native menu input, and legacy keypad arrows

The Connection Manager accepts positive `lastUsed` values from Teamwork XML
as Unix milliseconds. The cache preserves these values for offline use and
continues to read older snapshots. The Recent scope ranks the combined local
and shared catalog, takes up to 15 entries, then applies the search. The
list, range selection, map, and map PDF use that projection. Shared launch
attempts have no local saved ID and do not update the local recent store.

An instrumented Fedora X11 run traced premature context-menu dismissal to
`ConnectionWindow::set_open_counts`. It reconciled the selection even when
an open-session count refresh delivered the same values. The manager now
reconciles after a change in counts. The favorites regression exercises the
context-menu action across an identical refresh.

The explicit **Paste private key** action uses the configured clipboard. The
program permission for OSC 5522 remains separate. Each read has a single-use
request ID tied to its form, clipboard mode, and current authentication
controls. The workspace drops late, superseded, and replayed results. The
Teamwork policy check applies before the read and again on receipt.

The pinned Java comparison covers the four keypad arrows on eight HP,
TeleVideo, and Wyse profiles across Windows, Linux, and macOS. The fixture
checks all eight Shift, Control, and Alt combinations in each key family.
Alt prefixes the profile's keypad sequence with Escape. All eight Java
replays passed on Microsoft OpenJDK 21.0.12 with `--release 17`.
The fixture SHA-256 is
`e4748606958a96ec148d87e21fbc60aa21380e21288099c6fa67a388760bdb65`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,785 and 1,859 tests across 61 targets, with 11 and
69 ignored respectively.

The final native Fedora X11 run activated the shared favorite action by mouse
and by keyboard in separate profiles. Both stored the selected origin among
two sources with the same XML ID. The Recent list showed two source-dated
entries newest first and omitted the undated entry. The newest entry opened
the expected SSH form. An explicit paste accepted a generated key with the
default OSC 5522 permission off. The profile and app logs had no key bytes.
The native binary SHA-256 was
`971bf51c580dbec0199613f0e91bb9dfda4a4a12fb80d7ee0f5af3a7c8a49410`
from source manifest
`1fc621ecb0811fb1879e5b0b34aa6da7021465a6cc207ab7d09e6984e53d38f1`.

### Teamwork hide and restore and WY-160 scroll margins

The Java 2.26.0 manager asks separately about hiding shared connections and
deleting local profiles in a mixed selection. The native manager follows that
sequence. A hide records a typed source and XML ID in the local connection
store, with the name and host needed by the restore picker. The mark filters
the list, Favorites, Recent, tags, map, and map PDF. The SSH and SFTP launch
paths read the store before using a captured shared selection. An already open
login also checks the store at submission and host verification.

The manager owns the native **Restore Teamwork Connections** dialog. It can
show an entry while the source is unavailable. Restoring clears the local
mark and uses the retained source snapshot when available. A source refresh
follows. If a read is already running, the refresh waits for it and retains
its current result. A stale store result reloads the picker and requires a new
selection.
Portable backup projection excludes the profile-local marks. The Teamwork
toolbar's Refresh action republishes the current snapshot.

The WY-160 stream now retains vertical scroll margins when the host requests
80 or 132 columns. Three additional cases cover both directions and a
single-row region. All 27 Wyse cases matched the pinned Java reference. The
stream fixture SHA-256 is
`2cef7d91d34b8381abc2fe51a497c1a9f1856a5f8e8b57b236d7fa6ac391bcf2`,
and the expected capture SHA-256 is
`c77fee46384150026519c08a593a206775470a2d090676cfd149002007872a8d`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,791 and 1,865 tests across 61 targets, with 11 and
69 ignored respectively. The verified Fedora source manifest SHA-256 was
`fa46eed7b0f0f92efdaeed029370137dde02506b235745a828b5e35b8640bc9f`.

The native Fedora X11 run hid one of two rows with the same XML ID and
confirmed the other was still visible in All and Teamwork. The hide survived
an application restart. The Restore dialog was modal to its Connection
Manager and returned focus there after restoring the row. Both source files
matched their original checksums. The executable SHA-256 was
`68c7a14a2806b0361c25d0793f098d377e84e48b9fcf2f2c326463a9f001f7e1`.

### Teamwork in the connection palette and TeleVideo saved cursor reset

The `@` command palette orders its source rows by saved last use for local
connections, followed by visible shared connections ordered by name. Shared rows use
the pinned Java badge **Shared (Teamwork)** and show the source address and
group. Each key holds the source identity and the external XML ID, so two
sources with the same XML ID have distinct rows and session MRU entries. The
palette opens the manager's fixed-route SSH sign-in. It compares the captured
route with the current catalog and checks the local hide and policy state
before dispatch. An invalidated choice leaves the palette open with a reason.

TVI 920 and 925 `ESC z` clears the current display and homes the cursor.
It keeps the saved cursor record so `ESC "` can restore the cursor, saved
attributes, and graphics designation. All six TVI replay methods passed
against the pinned SithTermFX source. The new stream SHA-256 is
`8743d31aff9085487f0897573500f3d164dc26dc7f3f11cf1d762bc2eb19063d`,
and its expected capture SHA-256 is
`1e15fdef52dd670994ca431e2cb79616dfd118d3ceb72df0c0978fb62c4821ff`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,795 and 1,869 tests across 61 targets, with 11 and
69 ignored respectively. The verified Fedora source manifest SHA-256 was
`a74dddf3a8c07b8883fcf91096814c7e832c3a72ce5c937e9863891faddb9a05`.

The native Fedora X11 run displayed both origins and their route details
for one shared XML ID. After recording a local hide for the first origin,
the test reopened the palette and chose the second row by keyboard. The
SSH form showed that row's username, host, and port. The test kept both
XML files byte-identical and left the authentication form unsubmitted.
The executable SHA-256 was
`40fe2fd3f852db89f5d067af5e14538992a4c93b8ea75f7a91e23f4d68f5c7d3`.

### Control API tab creation and ANSI backward motion

The local Control API and `kortty-cli tab create` select saved SSH and serial
profiles or visible Teamwork rows. Explicit local numeric IDs take priority.
Ambiguous shared IDs return source-qualified candidates so the caller can
pick an origin without exposing the source location in the selector. Names
are compared case-insensitively. The request can name a window and apply
request-local privacy or isolation. A ready SSH start allocates a real tab
and keeps its pane ID when the authenticated stream replaces the provisional
stream. A deferred start responds with `pending: true` and `tab: null` and
uses the native sign-in or vault unlock. The desktop rechecks the saved
route, current policy, and Teamwork visibility before proceeding.

Saved local-shell profiles are a remaining gap for the Control API launch
workflow.

Backspace and `CSI Ps D` now count from the ANSI parser's virtual right
margin after a full row. The pinned SithTermFX replay passed for all ten
ANSI profiles. The new input SHA-256 is
`68fe0526e49e1a518e6eeaf065252bfe9cc6cc25b7fb9d39688fc93035221f59`,
and its expected capture SHA-256 is
`5c0d9860fe4789cb57b897ab56c23d80ee49170870ba960d67fab152d27db686`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,802 and 1,876 tests across 61 targets, with 11 and
69 ignored respectively. The verified Fedora source manifest SHA-256 was
`68dda5843f9c3560d0ab7975eaf5a63a0f882eece757cc80c2bcb056347e3b9f`.

The native Fedora X11 tests enabled the Control API in two private profiles.
The first used the actual Unix socket to open and cancel a Teamwork SSH
form, with a pending response and unchanged tab IDs. The second used a
generated key and pinned host key for a loopback OpenSSH server. The API
returned a ready tab before accepting the SSH connection. The original tab
and pane IDs then resolved to the authenticated shell and its output.
The executable SHA-256 was
`a21e86278f13556ebdcc76733c48f889632befe88b4efc3a56773eed91f317cc`.

### Managed server routes and ANSI resize margins

Global server rules parse the pinned Java name, glob, port, IP, CIDR and
inclusive range forms. A direct probe of the pinned Java matcher passed
49 standard assertions and confirmed decimal Unicode digits in numeric
parameters and zero-padded IPv4 tails within IPv6 addresses. Rust's
regressions cover those cases. A connection must satisfy every applicable rule.
Configured DNS names are compared without DNS resolution. Rust also blocks
legacy numeric IPv4 spellings when the native resolver could interpret them
as IP addresses under a server rule. This pass covered global rules. The
subsequent pass added user and group selection.

The Connection Manager and command palette mark blocked routes. A saved or
Teamwork SSH launch checks the active jump first and the final destination
second. SSH form submission checks the actual route before authentication
or worker startup. Host-key and dial continuations check again. The Control
API reports the first blocked endpoint. The same route gate covers SFTP,
IBM startup, scheduler commands and remote operations, and the scheduler
directory browser. Multi-target jobs report blocked targets independently.

ANSI resize retains the top margin and adjusts the bottom by the height
delta. A shrink that removes the region uses a valid full-grid default.
The width-only and height-change captures matched pinned SithTermFX on all
ten ANSI profiles. The input SHA-256 is
`4cff8b93efa1ec3061468354af964d721266381110051fda2207209950820d10`,
and its expected capture SHA-256 is
`f54832e1449d0dfb962279412ec2a8d23c865e51d4c618656afe403e3c7411be`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,811 and 1,885 tests across 61 targets, with 11 and
69 ignored respectively. The verified Fedora source manifest SHA-256 was
`d88d9ed3828be279c7abc515d0eae0f70cffd4318d1794222a5005e2f16ef3c7`.

The native Fedora X11 run enabled the Control API in a private installation
with a server deny rule and three loopback listeners. It received
`blocked_by_policy` and the exact blocked endpoint for both a denied final
host behind an allowed jump and a denied jump. Neither request reached a
TCP listener or created a tab. An allowed route returned pending and showed
the native password form. The executable SHA-256 was
`b40558f9652c59f7cdd8d1e4e63cec9167f9c75725e8f143c5087993d4ce924b`.

### Managed policy scopes and ANSI vertical motion

The installation policy recognizes `users`, `groups`, and policy-defined
group memberships. Each supported setting uses the highest matching tier
that defines it, in user, group, then general order. Server tables are
selected as a unit and intersect within the winning tier. Journal redactions
in a scoped policy merge in tier and file order and deduplicate equivalent
definitions. Teamwork sources remain global. All declared rules are checked
before looking up local identity. Duplicate normalized group definitions and
failed identity lookups needed for a decision lock the policy. The latter
also prevents an unavailable OS group lookup from dropping a restriction.

Windows resolves groups from the process token and includes both qualified
and bare names. Unix resolves primary and supplementary group IDs through
the native account database. Scoped policy caches identity lookup results,
with a two-second wait per lookup. General rules need no identity lookup.

The ANSI host stream retains the virtual right margin after LF, VT, FF,
IND, or RI. Automatic newline performs a carriage return before the feed.
The capture matched pinned SithTermFX on all ten ANSI profiles. The input
SHA-256 is `79f801c25bc3d0928e2a310cdc8826cfdcbb6bfd787fdc3c15f8ef909fd67a4b`
and the expected snapshot SHA-256 is
`1aac9c0d737217cb9bd51f3b545f3c7ae605e746878a685fb05162c411f6e663`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,820 and 1,893 tests across 61 targets, with 11 and
69 ignored respectively. The Windows simultaneous editor-save test had a
transient access-denied failure before the passing rerun. The verified Fedora
source manifest SHA-256 was
`dd8f9c95f5f9d1001ae843b83fb0ce227870f428c7fdbc791e08b87642c364a7`.

The private Fedora X11 instance enabled the Control API through an explicit
user rule over general and group denials. A policy-defined group allowed a
saved route over the general deny. A second rule matching the native primary
OS group denied another route in the same tier, and an unlisted route was
also denied. Both denials identified the blocked endpoint. The allowed route
returned pending for authentication. The executable SHA-256 was
`ed2f9565bb7c0ff63fddafe7dd46597e1330337eda97db23ca550dfe5669ad57`.

### Connection Map header, terminal AI memory, and Xterm soft reset

The Connection Manager reserves the full width of the List and Map switch
before laying out the scope summary. At the 940-pixel minimum width, a long
folder or tag name is clipped and the count can wrap. The active scope and
automatic network layout follow the existing Map action. The layout test
compares the control bounds for ordinary, long folder, and long tag scopes
in both views.

Terminal AI uses saved global memory as a source of user preferences. The
chat offers an inclusion switch when global memory is available to it. The
loader reads a bounded private file, masks complete entries, applies the
configured estimated token budget, and escapes markup and chat delimiters.
The reviewed JSON contains the exact system message sent to the selected
profile. The send worker verifies the included memory bytes again before
contacting the endpoint. Incognito chats omit the saved file. This pass
covers global memory. The pinned Java request path also combines memory
associated with the active saved connection.

The loopback regression rejects an obsolete load result and a changed file
before HTTP, then compares the authorized request with the rendered preview.
The injected memory is separate from the conversation turns. The existing
knowledge review test also exercises the asynchronous review path.

Xterm retains cleared and custom tab stops through DECSTR (`CSI ! p`). The
five Java replay cases passed against pinned SithTermFX for all ten ANSI
profiles. The soft-reset input SHA-256 is
`6bf61b841a0c72be9559f41d487df87fb67f7bf912ce7eaf421242cc8206b126` and the
expected capture SHA-256 is
`4d50a79b9c99e937898877e4ef88efbd4019be7d9eef0b1c30bf6e6b237f2d16`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,827 and 1,900 tests across 61 targets, with 11 and
69 ignored respectively. The Fedora source manifest SHA-256 was
`5b06e73eb063f8623766ddcba69c9003b58594cb426277080e2a81a1eacdc3cc`.

On Fedora X11, the private Connection Manager opened at 940 × 760. The test
selected a 96-character folder, activated Map, verified the in-scope host on
the canvas, and returned to List with the same scope. The out-of-scope host
stayed hidden after selection in both views. The executable SHA-256 was
`94ce1a4bb0ba32ba8bbefde64bf62df8c12dd1b49b22362c48ed1a58b76471cc`.

### Saved SSH memory, Windows editor publication, and TeleVideo insertion

Terminal AI captures the saved ID and full route from the source SSH pane
when it is running and belongs to a non-incognito tab. It resolves the
same ID and route in the current connection catalog before reading scoped
memory. Ad hoc and Teamwork routes without a local saved ID use global
memory. Reopened chats also use global memory. Connection entries have
priority in the shared token budget. The preview renders Global before
Connection, as the Java builder does. The send worker rechecks the catalog
and both read files before submitting the reviewed request. The loopback
test distinguishes profiles at the same endpoint, rejects a changed jump
route or edited memory before HTTP, and compares preview and request.

The Windows editor keeps the staging handle with delete access through
synchronization and handle-relative publication. New files use an atomic
no-replace operation. Before a replacement retry, the editor rereads the
original and checks the identity of the destination directory. The native
tests check a staging reader that declines delete sharing, an open reader
that permits delete sharing, and a blocked destination changed externally
before retry. The same-snapshot race test passed 30 consecutive direct
runs. The earlier intermittent access-denied error remains unattributed.

TeleVideo 910, 920, and 925 insert a double-width BMP glyph using its
one-unit UTF-16 length for the shift and two display cells for painting.
All seven TeleVideo captures replayed against the pinned SithTermFX core.
The insertion input SHA-256 is
`f4e95f8ff4e7941a73d39fa3ac1739ff85fae35ad9657ae95259c6f02323c407` and its
expected capture SHA-256 is
`619c2dc4fd5797c4ce579d2e716aa104ac3c5e949fc1473e0b31a4da9eb7b9e6`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,834 and 1,904 tests across 61 targets, with 11 and
69 ignored respectively. The Fedora source manifest SHA-256 was
`e082bb2e5f9863c6513d5e3c9fc7e00463bc7bc2a3ef83c190f3da99125c58cb`.
The Windows pass includes the conditional import for the native editor
branch. The Fedora snapshot predates this import-only change.

The native Fedora X11 follow-up built the revised source, opened a saved
SSH profile against an isolated loopback OpenSSH peer, and captured its
terminal output through the AI menu. The connection-memory badge appeared
in the review. Replacing the selected memory file stopped the reviewed
send before HTTP. A fresh review sent global memory, the updated selected
connection memory, and the real SSH output to the loopback model. The
request contained the mask for the synthetic token and omitted the other
profile's memory. The executable SHA-256 was
`7b838cbcc2fd2e8e0c9c72b68162e8fb2dc7e0147be557c0308a40222dc5b4e9` and the
revised source manifest SHA-256 was
`86b78efd35b9f7c1953e76f54ba48eb50d13c8b0ed3a812565581f255c3b4940`.

### Approved external AI memory and ANSI erase at the virtual margin

Terminal AI offers a separate opt-in for approved external references. A
new chat starts with this option off, including reopened conversations.
Active global references and references for the verified saved SSH scope
can contribute local `.md` or `.mdc` excerpts under the canonical home.
The approved path is opened relative to that root without following
links. The open file must prove local filesystem access and match its
approved digest before its bytes can enter the prompt. The OS may consult
mount metadata while opening a path. Unrecognized filesystem types and
project directories outside the home are excluded in this pass. Lexically
ineligible approvals are removed before the file-open limit is applied.

External excerpts are sanitized, screened for residual credentials, and
placed after saved memory as untrusted data. `.mdc` front matter stays
out of the request. Excerpts are bounded to 16 KiB per file and use at most
1,000 estimated tokens from the remaining memory budget. The send worker
reopens the included reference bindings and checks their registry rows,
file identity, and bytes before HTTP. `[rule.ai-memory] external-files`
uses normal rule precedence and restrictive same-tier combination. A
managed deny leaves reference metadata visible and locks file review and
changes in AI Manager.

ANSI `CSI 0 K` and `CSI 0 J` preserve the last visible cell and the pending
wrap at the virtual right margin. `CSI 0 J` still clears following rows.
Six replay methods passed on all ten ANSI profiles against pinned
SithTermFX. The input SHA-256 is
`932d2a9ea77e242269c70e817bf1404f7966bfe8d915c8c3da2ec3ff21bf56b8` and the
expected capture SHA-256 is
`ebd04134d45ef38e4db1b4e08bc4048f2f8351afee63c882ed7f8934fce985f0`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,844 and 1,914 tests across 61 targets, with 11 and
69 ignored respectively. The `aarch64-apple-darwin` workspace check covered
all targets. The Fedora source manifest SHA-256 was
`a94c05be91651346d090e94dc668cc9115956fd1d377dd3168433afab7beb385`.

The native Fedora X11 run used a private shell, home directory, reference
registry, and loopback model. The default reviewed request carried global
memory. After enabling references, the request included the approved `.mdc`
body without front matter or the unapproved file. Changing the included
file after review stopped the request before HTTP. The executable SHA-256
was `6992e8d6d2b025ac28304874f9b3a79524e52438e1a64e5de8d0bd7ff25f45ed`.

### Managed user memory and one-row ANSI scrolling

The managed AI memory feature switch and user-memory mode guard access to
user memory. Read-only mode exposes saved global and connection files,
history, archive, and approved reference previews. The manager displays a
locked learning preference and the effective token limit. A rule can cap
the prompt at 300–4,000 estimated tokens while retaining the saved user
preference. Each key uses its own winning tier. A canonical external file
path must match each nonempty allowlist in that tier before its content is
read. The reviewed request is invalidated when its effective memory
policy changes, and the send worker checks referenced files again.

The ANSI profiles accept equal top and bottom margins. Index and Reverse
Index at the selected row clear that row and preserve its neighbors.
Seven replay methods passed against pinned SithTermFX on all ten ANSI
profiles. The input SHA-256 is
`a74b0add3a97a035774630961837ff6f3d004bd1360e74f1f1d2f6f22991b92b` and the
expected capture SHA-256 is
`98d60f93a91aaba8d068890c8ab6cc394a5e0fd8a7900b3182c61531b5298445`.

Formatting and Clippy passed on Windows ARM64 and Fedora 44 ARM64. The
workspace suites passed 1,857 and 1,927 tests across 61 targets, with 11 and
69 ignored respectively. The `aarch64-apple-darwin` workspace cross-check
covered all targets. The Fedora source manifest SHA-256 was
`3090209758bb0e17cbe2070b7e2a494ba60da4814568d6db5d1db3d436dc3d63`.

The Fedora X11 smoke opened a local terminal summary, opted into approved
references, reviewed the request, and sent it to a private loopback model.
Two same-tier rules limited the budget to 350 tokens and admitted one of
three approved files through intersecting allowlists. The captured request
contained the short global entry and the eligible reference. It omitted
the long entry and the other two references. The stored preference was
still 1,400 tokens. The executable SHA-256 was
`c46ffc80b72eeedd81377255158960121564ec01fbb3f7680bed3ff6b13ba3cc`.

### Organization memory in Terminal AI

Top-level `[[organization-memory]]` blocks are installation-wide and keep
their file order. The parser validates the block shape, limits each field
to 8,000 Unicode characters, and accepts at most 20 blocks and 16,000
characters including titles. It drops oversized or credential-like
blocks with content-free warnings. The Rust parser rejects unknown block
fields and strips invisible controls from accepted text. The AI Manager
shows individually expandable, read-only rules with the applicable
personal-memory status and policy denial.

Terminal AI includes allowed organization rules in its exact request
preview. It masks titles and contents, encodes them as JSON, and budgets
them before optional personal memory. The full rules remain in the
request above that budget. Personal memory switches and incognito
storage rules apply only to the personal sources. The preview binds
the effective organization rules so a changed rule requires another
review. Java's additional memory consumers include Terminal Agent
planning and scheduled agent jobs. They remain part of the respective
workflow ports.

The organization-memory snapshot passed formatting, Clippy and the
workspace suites on Windows ARM64 and Fedora 44 ARM64. The runs passed
1,866 and 1,936 tests across 61 targets, with 11 and 69 ignored
respectively. The `aarch64-apple-darwin` cross-check covered all workspace
targets. The Fedora source manifest SHA-256 was
`527ac10231c153290da3cf3457d4934e52a1428e3d3cad0ad596fec2f8c7834b`.

The native Fedora X11 run opened a terminal summary and used a private
loopback model. It observed no HTTP before Send. With both
`user-memory = "disabled"` and `KORTTY_AI_MEMORY=off`, the approved request
carried two full organization rules in policy order. The first rule alone
exceeded the 300-token ceiling, and personal and external fixtures stayed
out of the request. The executable SHA-256 was
`4a059b57952b3e4e94a3176cd0f0cbe41e8d039bd39790c7bbb20203c8555fd8`.

### ANSI vertical position absolute

`CSI Ps d` addresses a physical row under DECOM and retains the column
and pending wrap. Rust bounds out-of-grid row requests to the physical
grid. The combined fixture matches the pinned SithTermFX core on all ten
ANSI profiles. Its input SHA-256 is
`38d4637a73b53058c6d6cde346ed483db4d4c7b2c773f4234ceaaf81f601d4d5` and the
expected capture SHA-256 is
`d99e0af5878a4e23ba3aa9829e87d2b9bbe3d334679a392fb21ad77bbc002c3d`.
The subsequent terminal-crate runs passed 229 tests on both Windows
ARM64 and Fedora 44 ARM64, with formatting and Clippy clean. The
terminal-crate `aarch64-apple-darwin` cross-check covered all targets.
The Fedora source manifest SHA-256 was
`da308705629ed6e637ed66116f3e27d7e804b9fde22d7fdfe2f67431f1ed2c8d`.

### Confirmed manual rules in Terminal AI

**Remember as a rule…** starts from a displayed user message in a live or
reopened Terminal AI conversation. The native editor uses the full
displayed question as its starting text. It suggests connection memory
when the text names the verified saved SSH connection or its host.
Reopened conversations offer global memory. The destination, normalized
rule, and exact before and after files are visible before the local
write. Escape closes the draft and returns to the conversation.

Saving requires editable personal memory and a fresh confirmation of the
current file version. The writer checks the 300-character rule limit,
credentials, duplicate rules, and the file and token budgets. Connection
writes bind to the current saved route. The rule uses the existing
managed entry format and history. Undo is available for 30 seconds. It
archives the intact entry and restores the previous bytes when the file
still matches the confirmed write. If another edit intervened, undo
preserves it and removes only the intact managed rule. Automatic
proposal detection and learned skill targets remain separate Java
workflows to port.

The manual-rule snapshot passed formatting, Clippy and the workspace
suites on Windows ARM64 and Fedora 44 ARM64. The runs passed 1,875 and
1,945 tests across 61 targets, with 11 and 69 ignored respectively.
The `aarch64-apple-darwin` cross-check covered all workspace targets.
The Fedora source manifest SHA-256 was
`855ec87805ffd8a770f3b0e3a8dd492a27531c557ab4405ecd1609cea77f615e`.

In the native Fedora X11 run, a private shell produced the selected
terminal text and a loopback model received one reviewed chat request.
The rule preview and Escape left both files byte-for-byte unchanged.
The next review and save appended the expected managed rule. Undo
archived that exact fence and restored the original memory bytes. The
rule steps left the total model request count at one. The executable
SHA-256 was
`c4f9e629804096adaa1fbc564cdf615795aeff2f5ab52ae7199d82e4d37a628d`.

### ANSI column addressing and line movement under DECOM

CHA and HPA reapply the vertical origin to the current physical row.
CNL and CPL apply it after moving to the requested line. With DECOM off,
the latter commands use the full physical grid even when a smaller
scrolling region is set. These commands clear pending wrap. Two replays
matched pinned SithTermFX on all ten ANSI profiles. The registered input
SHA-256 is
`054de83af895a5ab2235ad1145c2ecb0f834405b382759e30b401d7cbe8fbcdf` and the
expected capture SHA-256 is
`244624e860ba91e610a3ac5d0de9ae775090816d8cbc66a6109a671e568f6d8a`.
The subsequent terminal-crate runs passed 231 tests on Windows ARM64
and Fedora 44 ARM64, with formatting and Clippy clean. The terminal-crate
`aarch64-apple-darwin` cross-check covered all targets. The Fedora source
manifest SHA-256 was
`4f573a8e9ef16845c5df658ecd1038e6dc9b05294251b05eec51adf44d4f13fe`.

### ANSI insert and delete lines

Unqualified IL and DL move rows from the physical cursor row through the
bottom scrolling margin, including when the cursor is above the top
margin. They leave the cursor position and pending wrap in place. An
omitted count means one. An explicit zero leaves the rows intact, and the
first numeric group supplies the count when leading separators occur.
Four pinned replays cover these behaviors on all ten ANSI profiles. Rust
also checks the DEC C1 form separately. The native horizontal margin
restriction and bounded counts remain in force.

Windows ARM64 and Fedora ARM64 each passed rustfmt, Clippy, and all 236
tests across 21 terminal crate targets. The Fedora source manifest SHA-256
was `5cec02cb9c59d8444bdef37daa5936f3b87eec0cbf55634fe2a72097bae36ff4`.

### Confirmed learned rules in AI Skills

The manual rule editor offers eligible existing user skills and the
bundled skill for a recognized topic. An eligible user skill can override
the bundled destination. A known free tool or language topic can also
offer a new skill. A named originating connection remains the first
suggestion when its memory is writable. The editor labels new skills
separately and shows the current and proposed Markdown before applying
the rule locally.

The local learning setting follows the explicit managed
`[rule.ai-memory] skill-creation` value while preserving the stored user
preference. Read-only personal memory can still allow a skill destination.
The skills file retains legacy rule strings and gives newly saved rules
durable IDs and fingerprints. Undo removes the matching intact rule, or
the whole new skill while it matches the confirmed creation.

AI Manager settings and learned skill changes share a guarded atomic
write. A stale settings snapshot or changed reviewed destination requires
another review. A pending edit in AI Skills delays chat rule writes.
Completed writes refresh a clean skill editor and retain unrelated
settings drafts. Open Terminal AI and diagram reviews rebuild against
the new skill library. Automatic proposal detection remains a separate
workflow.

The Windows ARM64 and Fedora workspace checks passed with 1,888 and
1,958 tests respectively, across 61 targets on each platform. The runs
also passed rustfmt and Clippy. The macOS ARM64 workspace cross-check
passed from Windows.

The Fedora X11 desktop run reviewed and canceled an edit to the bundled
Rust skill, then saved and undid the exact rule. It also created and
undid a new Elixir skill. The loopback backend saw one request for the
original chat message, and the global memory file kept its exact bytes.
The source manifest SHA-256 was
`fb99dd572e228a4e7df4bfc11c29d1840942789f7ce2ae8d64aa99b3e820f37a`.
The desktop binary SHA-256 was
`6f81eaafc6ab185fd74c5fa340c564e4f5ef77d51ac77c6c9f0f8c1bf915c38e`,
and the smoke driver SHA-256 was
`06d471183291bf6d7f6f6f700e5c84f0d5b014a1a4e8c2f1ef27a9b1e7e9af81`.

### AI profile setup wizard cancellation

The embedded setup wizard waits for an active profile request to finish
before opening. It blocks AI Manager page navigation, search, and saving
until it closes. Cancel, Escape in a focused field, a close request for
the standalone AI Manager, and the native macOS close shortcuts dismiss
the wizard and preserve unrelated edits. An owner close uses the AI
Manager's draft confirmation. The manager discards late results from
canceled wizard profile requests. Rust embeds the wizard in both the
standalone AI Manager and its hosted tool tab.

The Windows ARM64 workspace run passed rustfmt, Clippy, and 1,896 tests
across 61 targets, including the focused input, owner close, and late
request regressions. The macOS ARM64 workspace cross-check included the
native menu dispatch and its regression test.

The Fedora ARM64 workspace run passed rustfmt, Clippy, and 1,966 tests
across 61 targets. The source manifest SHA-256 was
`49f91849355d5e457f2e66c34afbf92b40bc918864010ab83a959d710356ed07`.
The first native X11 probe reached the Profiles page through search and
timed out locating the setup button. The X11 binary SHA-256 was
`fd13d72ca05ebdc4ccafa84f916d4086b381b18137250c40ae58dbf912122b7d`.

### AI Manager profile actions and picker Escape

The profile page places Setup wizard, Add profile, and Refresh above
both columns, matching the pinned Java AI Manager. At compact heights
the profile header, section tabs, and fields share a scrollable area
above the editor actions. The wizard's open pick list consumes Escape
and closes before Escape can reach the enclosing wizard.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,898 tests across
61 targets with four test threads. Fedora ARM64 passed rustfmt,
application Clippy, and 1,534 application tests. The macOS ARM64
workspace cross-check passed. The Fedora source manifest SHA-256 was
`4a87ce2c39e38704f93544336246f1356aa57f31f5efd93f50927bbcf2eb441e`.

The native X11 run used a 1,600 × 1,000 virtual output and a
1,120 × 760 AI Manager client. It verified the action strip above
both columns, the two Escape presses, the reset wizard endpoint, and
the saved name of the unrelated profile. The Fedora binary SHA-256
was `70b3ae9093b4537972f524f8f28d01231f8304a3992d7e80ee793a721477d656`.
The private Mutter 50.6 run started the native Wayland main window and
captured the 1,600 × 1,000 virtual output through PipeWire. The driver
initially timed out opening AI Manager from the menu. A follow-up used
the command palette and settings search, then verified native keyboard
and relative pointer delivery to the 1,120 × 780 AI Manager client.
Escape closed the list first and then dismissed the focused wizard.
Reopening restored the default wizard endpoint and saving persisted only
the staged name of the original profile. The successful Wayland binary
SHA-256 was
`beded485e5f62a58386621c45fb5c4300771de67f407713d01d74f62d4238cf3`.

### Reviewed automatic rule suggestions in Terminal AI

Terminal AI offers a classifier for eligible new questions when
effective AI Memory learning is set to Propose and a destination is
available. The auxiliary request starts unchecked. Selecting it opens
its exact two-message preview, and the send button confirms both
requests. After a successful primary answer the same local profile
receives only the reviewed question and the classifier instruction.
Incognito chats and generated initial summary or diagnostic prompts
are excluded.

The bounded typed JSON response can produce a proposal card. Its rule
opens the existing local destination and file review, with `chat`
provenance in a confirmed memory or skill entry. Not now and permanent
rejection affect both the question and rule fingerprints. The private
learning ledger uses a locked atomic update for the ten attempts and
three cards per local day. Agent-run learning is a separate workflow.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,912 tests across
61 targets with four test threads. The loopback test compared the
auxiliary request with the displayed JSON and confirmed a separate
local save. The macOS ARM64 workspace cross-check passed.
Fedora ARM64 passed rustfmt, application Clippy, and 1,548 application
tests. The Fedora source manifest SHA-256 was
`f71fb0b785d99f589edd93da6d322a521c435418e0e42d842208101b39a06c07`.

### Managed model default, Dockerfile structure, and ANSI character edits

The Local Models picker now matches the pinned Java workflow of marking a
managed model with a star and selecting it when the manager opens. The native
manager commits the preference with **Save**. The saved registry ID restores
the selection after restart, while refreshing keeps a valid active selection.
The choice is independent of text, code, journal, and embedding assignments.
A missing registry entry leaves the preference available to clear.

Dockerfile snippets list `FROM` stages, `ARG` parameters, and `ENV` variables.
The native editor also routes `Dockerfile` and `.dockerfile` paths through the
scanner and navigates to a selected declaration after **Structure → Refresh**.
The regression uses `SnippetStructureScannerTest.dockerfileStagesWithArgsAndEnv`
from Java 2.26.0 and checks scopes, byte spans, and reference counts. Both
scanners are line-based. Rust resolves repeated declarations in source order.

The shared ANSI parser preserves the last visible cell and the pending wrap
when ICH, DCH, or ECH arrives at the virtual right margin. The pinned
SithTermFX v1.2.7 replay produced the same result across all ten ANSI profiles.
The [fixture record](crates/terminal/tests/fixtures/sithtermfx/README.md)
documents the stream and expected capture. A separate VT220 test verifies the
edits after direct cursor addressing to the final physical column. Default
VT100 and VT102 edit support at a physical column still differs from the pin.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,918 tests across
61 targets, with 11 ignored. The macOS ARM64 workspace cross-check passed.
Fedora ARM64 passed rustfmt, application and terminal Clippy, and 1,799
tests across 24 application and terminal targets, with 49 ignored. The
Fedora source manifest SHA-256 was
`1c826b24d6d0cd43068d69899cb4098927e7274ddf72079993107c4bf2ebe704`.

The Fedora X11 run opened both native windows from their menus. The model
driver confirmed the saved default ID and the existing role assignments in
the private profile. The editor opened the pinned Dockerfile and reported
line 6 after a sidebar click. The driver did not finish the model restart
and clear checks or prove the exact line 5 edit and save. These are partial
desktop results. The state tests cover selection from serialized settings,
clearing the saved preference, and the exact editor cursor jump.

### Immediate model default and exact Dockerfile and terminal positions

The Local Models default marker is now committed immediately, as in Java
2.26.0. A separate private preference records the registry ID and leaves AI
settings and in-progress request reviews independent. The write checks the
previous marker and publishes atomically under its own lock. An explicit
clear overrides the earlier default field in `ai-manager.json`. The previous
field remains a fallback for profiles that have no separate preference.

Dockerfile reference counts use the declaration for the whole current stage,
including the `FROM` line, with a global fallback. The snippet outline keeps
equal stage labels and their scoped variables separate for mouse selection,
caret tracking, and keyboard jumps. Source spans identify the alias and the
declaration token even if the same text occurs earlier on that line.

The directly addressed final column now supports ICH, DCH, and ECH across
all ten ANSI profiles, including VT100 and VT102. The pinned Java replay
passed both physical and virtual right-margin streams for all ten profiles.
The test suite also covers Rust's explicit DECSCL downgrade. That mode
continues to gate ICH and ECH on later DEC profiles after selecting level 1.

The pinned Java Dockerfile scanner was also compiled in isolation on Windows
with JDK 21. It confirmed the stage-wide reference counts and the distinct
source positions of aliases, repeated stage rows, and their declarations.
The Java scanner check did not launch the JavaFX editor.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,923 tests across 61
targets, with 11 ignored. The macOS ARM64 workspace cross-check passed.
Fedora ARM64 passed rustfmt, application and terminal Clippy, and 1,804 tests
across 24 targets, with 49 ignored. The Fedora source manifest SHA-256 was
`1fc576c0b011c23691f5c6948283d7ddf3f4d9bb9fb72a9f547a402b1387cf06`.

In a private Fedora ARM64 Xvfb and Openbox session, the command palette opened
the native Editor. Clicking the stage on line 5 moved the cursor and focused
the editor. Typing a marker and saving the disposable file changed exactly
the beginning of line 5. This window test used one stage. The pinned source
comparison and state tests cover multiple stages and repeated labels.

In the same Fedora environment, **Set as default** immediately wrote the
expected managed model ID while `ai-manager.json` remained byte-identical.
The driver did not complete the window close through Alt+F4, so the native
restart and clear checks remain open. The state tests cover restoring the
separate preference and persisting an explicit clear.

### Managed model CPU threads, SQL outline, and ANSI autowrap

**Configure model** stores a CPU thread count from 0 to 1024 with the managed
model. Zero and older manifests use the runtime's automatic selection. The
verified sidecar command adds `--threads` only for an explicit count. The
configuration still uses the registry lease and the stopped-model workflow.
The argument test inspects the command used to start the sidecar.

The SQL outline accepts the pinned Java scanner's bounded `CREATE` forms for
tables, views, functions, procedures, and triggers. It detects global `@`
variables introduced by `DECLARE` and `SET`. Both the snippet panel and the
standalone editor retain the source position for repeated names. Two pinned
Java scanner probes confirmed the object ranges, declaration positions, and
reference counts, including Unicode, CRLF, and masked comments and strings.

When ANSI autowrap is disabled and a full row has a pending wrap, the next
printable character starts in column one of the same row. Enabling autowrap
before that character moves it to the next row. The styled transcript matches
all ten ANSI profiles in the pinned SithTermFX v1.2.7 replay. The separate
horizontal margin behavior has a Rust regression test. Cursor reports at a
pending virtual margin remain a separate reference difference.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,930 tests across 61
targets, with 11 ignored. The first workspace run encountered a Windows
access-denied error while finalizing a journal fixture. The isolated journal
test and the full rerun passed. The macOS ARM64 workspace cross-check passed.
Fedora ARM64 passed rustfmt, application and terminal Clippy, and 1,811 tests
across 24 targets, with 49 ignored. The Fedora source manifest SHA-256 was
`e0a862feaecc8e277b65784a74fd61b822510aa2233f0f6de4d7ab807d96927c`.

The private Fedora X11 session confirmed the default model through three
application starts. It set the marker, reopened with the model selected,
refreshed after a different live selection, cleared the marker, and reopened
with an empty selection. The test compared `ai-manager.json` byte for byte
at each phase. Ctrl+W closed the native AI Manager at each step.

In a separate native X11 model configuration run, saving 6 threads updated
an older manifest. After another application start, the form displayed 6.
Changing the field to 9 and canceling preserved the manifest byte for byte
and restored 6 when the form reopened. The test used registrations without
weights. The runtime argument test covers the launch command.

The native X11 Editor opened a disposable `.sql` file through the command
palette. Selecting its table declaration in **Structure** focused the editor
at line 5. Typing a marker and saving changed exactly the beginning of that
line in the file.

### GPU layer configuration, ANSI cursor reports, and journal publication

**Configure model** stores an explicit GPU layer count from 0 to 10,000.
The −1 setting and older manifests keep the installed runtime's default.
The sidecar command passes `--n-gpu-layers` only for an explicit count.
The registry tests cover range checking, old manifests, resetting to automatic,
and preserving weights while another request holds a model lease.

Standard ANSI `CSI 6 n` reports the virtual column after filling the final
cell and the physical row when vertical origin mode is active. Direct cursor
addressing reports the addressed cell. Both transcripts passed the pinned
SithTermFX v1.2.7 replay across all ten ANSI profiles. Rust's private DEC
report and horizontal-margin extension have separate behavior.

Windows journal finalization now uses the existing atomic replacement for
delete-shared readers. The focused test reproduced Windows error 5 on the
old path while a reader held `journal.xml` open. The two regressions exercise
an open delete-shared reader and a reader that withholds delete sharing.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,935 tests across 61
targets, with 11 ignored. The macOS ARM64 workspace cross-check passed.
Fedora ARM64 passed rustfmt, application and terminal Clippy, and 1,814 tests
across 24 targets, with 49 ignored. Its source snapshot predates the
Windows-only journal replacement. The manifest SHA-256 was
`32d9999fa6c232c24631fff3869819107b97fdb725f71fdd8ffd39346705979f`.

The Fedora release-signed smoke passed with the ARM64 CPU package from
[`llama-b11727-kortty2`](https://github.com/chardonnay/kortty-llama-runtimes/releases/tag/llama-b11727-kortty2)
and the pinned [TinyStories Q4_0 GGUF](https://huggingface.co/ggml-org/tiny-llamas/blob/6e091d820cbe8f22eeb604d136403eca290b8c1e/stories15M-q4_0.gguf).
The archive and model SHA-256 values were
`3199d8458fb397a3a23681bd1b08c5f1e7b34c30e55236443005a2e497293e1c` and
`6151b1929d7f5aa3385d9ddef3393e55587c0a55de661562322bc51dfda93a04`.
The test checked `--threads 2` and `--n-gpu-layers 0` on the owned process
after real authenticated responses. It also verified shared leases, idle
unloading and reloading in the same process, and shutdown cleanup.

The native Fedora X11 run saved 12 GPU layers to an older model registration
and read that value after restart. Cancel preserved the registration after
editing it to 4. Saving −1 removed the explicit setting, and a third launch
restored automatic selection. The registered CPU thread count remained 6.
The native form used manifest fixtures. The signed smoke used the CPU
runtime, so verification of real GPU offload remains open.

### Signed model backends, raw snippet scripts, and ANSI status requests

Managed models persist Auto, CPU, Vulkan, or Metal as their backend. Auto
uses the signed package's behavior. CPU uses zero GPU layers. The manager
offers a GPU backend after checking the active signed package receipt and
the host platform. A saved unavailable backend remains visible and can be
repaired without changing the stored layer or thread count. Manual Start and
Test require a matching GPU backend. Managed profile requests can use CPU
placement with the installed verified package, including when a previous
request already holds a fallback runtime lease.

The single snippet report export offers **Script (plain text)**. Before
uses the stored analyzed source. After uses the final source for the
selected application run, including when its verification record is open.
The export preserves UTF-8 bytes and line endings, honors cancellation before
publication, and uses the existing atomic no-clobber file creation.

Ordinary ANSI status reports skip leading empty parameters and respect an
explicit zero. Private `CSI ? 6 n` is silent in the ten shared ANSI profiles
under their usual geometry, matching SithTermFX v1.2.7. Rust retains the
private report in its explicit horizontal-margin mode. The pinned replay
passed the four status and cursor-report streams across all ten profiles.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,941 tests across 61
targets, with 11 ignored. The macOS ARM64 workspace cross-check passed.
Fedora ARM64 passed rustfmt, application and terminal Clippy, and 1,820 tests
across 24 targets, with 49 ignored. Its verified source manifest SHA-256 was
`df6cddcb29ac1185f53b75000f1a2695cbe847036c03a03f8627eef322c2f471`.

The Fedora release-signed smoke used the same ARM64 CPU package and pinned
TinyStories GGUF recorded above. With a saved Vulkan preference, manual
Start and Test refused the mismatch and preserved the pending activation.
A managed profile obtained a real CPU response. Manual actions also refused
to reuse that fallback process. Selecting CPU produced real responses with
the expected owned-process arguments and unloaded and reloaded the model
on the same process after idle. The Fedora Vulkan loader enumerated only
llvmpipe as a CPU device.

Under private Fedora X11, an older model registration opened as Auto.
Saving CPU preserved the registered thread and layer counts. After a new
application start, canceling a draft change preserved the manifest byte for
byte and the form reopened with CPU selected.

In a private headless Mutter Wayland session with X11 disabled, the manager
displayed a saved Vulkan preference while the signed package was unavailable.
The form offered Auto and CPU. Choosing CPU published the repaired backend
while preserving the thread and layer counts. The client log confirmed the
native Wayland AI Manager `xdg_toplevel`.

### Guided runtime repair, chat policy, and report history

An unavailable saved Vulkan or Metal model can open a package form for its
required backend on supported platforms. The form carries the model selection
and keeps its paths separate from the general offline installer. The signed
index and copied archive identify the package before resident models are
stopped for activation. Cancellation before publication and a backend mismatch
preserve the existing activation pointer. The trust index still enforces signed
revocations. After an authenticated matching activation, the guide offers a
return to the model.
Starting or testing the model remains a separate action. Escape, the window
close request, and the close shortcut handle the guide before its parent.

The enterprise `ai-chat` feature follows the pinned Java `PolicyFeature`,
`EffectivePolicy`, and `MainWindow` behavior. Its highest matching user, group,
or global tier wins, with denial taking precedence within that tier. The
master `ai` rule also applies. Terminal selection and recent-output actions,
chat requests, and Saved Chats honor the rule. Other AI tools still use their
existing policy gates.

A successful single formatted snippet report adds an entry to the exact saved
analysis selected for the export, including the originating apply record for
an After report opened through its verification. The implementation follows
the pinned Java `SnippetAnalysisExportController`, `SnippetAnalysisController`,
and `SnippetAnalysisRecord`. A record retains up to 32 entries with the
filename, format, phase, run identifier when applicable, and timestamp. The
published report remains available if the analysis history has changed or its
write lock is busy.

Ordinary ANSI CUU and CUD preserve pending wrap and use directional vertical
bounds in origin mode. With origin mode off, both clamp to the screen edge.
Two [SithTermFX fixtures](crates/terminal/tests/fixtures/sithtermfx/README.md)
match all ten shared ANSI profiles at the pinned revision. Explicit horizontal
margins retain the existing Rust movement behavior.

Before the desktop layout adjustment below, Windows ARM64 passed rustfmt,
workspace Clippy, and 1,950 tests across 61 targets, with 11 ignored. Fedora
ARM64 passed rustfmt, application and terminal Clippy, and 1,829 tests across
24 targets, with 49 ignored. Focused tests cover a real history lock
held by a competing handle, a wrong signed archive while usage is shared,
successful matching activation after release, and busy repair cancellation
through window and tab close requests.

The Fedora base source manifest SHA-256 was
`e670ec6a3182ccaa2ceb4b853e205e642cadbadf1764da806bcbb343d0e57e26`.
The verified source update manifest SHA-256 was
`b08a9bc1d7e0ad478b02009bc51df84f8be05172ffd2f024df98c1413c383f3d`.

Desktop probes led to placing the repair entry next to the unavailable backend
notice and moving the guide's actions above its scrollable fields. Windows and
macOS ARM64 passed workspace checks for all targets after that adjustment.
The rebuilt Fedora binary used the source manifest SHA-256
`45cf0161806e99a4ec9fa1526ed4c580c68c5eacc47e621a3b18f10d49c8a3af`.
In private native X11, the guide displayed the saved Vulkan backend and
rejected the release-signed ARM64 CPU archive before installing its executable
or publishing an activation pointer. Ctrl+W returned to the same model and
left its manifest unchanged.

In private native Mutter Wayland, the guide showed the required Vulkan backend
and its actions. Escape from a focused package field returned to the same
model with the manifest unchanged. The client trace confirmed the AI Manager
as an `xdg_toplevel`. Hardware Vulkan and Metal offload remain open.

### Chat code insertion, runtime removal, and report destinations

Chat answers expose Copy code and Insert into terminal for closed fenced
blocks. Target selection follows the original pane while open, then the
current window's selected, recent, or sole terminal. The insert path rechecks
managed AI chat permission, mirroring, connection generation, and input state.
The terminal actor checks its live bracketed-paste mode and refuses lossy
encoding before queueing code. The stream writer acknowledges a successful
write and flush. Multiline and tabbed code need bracketed paste. The action
adds no Enter. The Java code-block Run action and action preference remain
open.

Local Models reviews and removes installed signed runtime packages while
retaining the GGUF registrations and accepted trust floor. The confirmation
binds the activation and private package directories. Active usage, changes
to the reviewed tree, and mount boundaries prevent removal. Cancellation of
the read-only review invalidates its queued result. Cleanup can be retried
after deactivation if a package could not be removed.

Single and combined formatted snippet reports and ZIP exports add their
selected extension when needed. The successful outcome offers Show in folder
for its original output directory. The native opener rechecks the directory
identity. Raw script filenames retain the entered path.

Ordinary ANSI CHT and CBT leave the cursor and pending wrap unchanged as in
the pinned SithTermFX dispatch. Explicit horizontal-margin mode retains the
Rust extension. The new tab-movement and existing horizontal-tab fixtures
matched all ten shared ANSI profiles with the pinned Java source.

Windows ARM64 passed rustfmt, workspace Clippy, and 1,960 tests across 61
targets, with 11 ignored. The macOS ARM64 workspace cross-check passed.
Fedora ARM64 passed rustfmt, application and terminal Clippy, and 1,841 tests
across 24 targets, with 49 ignored. Its PTY test held a read pending after
insertion and received the literal code after a separate Enter. The Fedora
source manifest SHA-256 was
`545e13d6987fdb2c56873ffe94be653848dca2455b00d6ad2936d73216e363aa`.
The verified source update manifest SHA-256 was
`ac142dbb74f4606abbcbc7f5a135d54405d0b58288e52cfddfbd0f19fc3c070c`.

Private native X11 installed and activated the release-signed ARM64 CPU
runtime. Cancel preserved the active pointer. Confirm removed the activation
and package tree. The trust record and model registration matched the bytes
captured before removal. The rebuilt native binary SHA-256 was
`b97328e194fe5d9a75043e990703314682d294bcdf0a4595241d396b093b9272`.

### AI chat terminal actions and batch report progress

The AI Manager stores the Java terminal-action preference IDs and exposes the
setting through General and search. Run previews a single shell line and its
target pane and checks the managed `ai-agent-execution` policy. The rendered
pane and connection generation bind the click. The stream writer asks the
terminal actor to recheck the screen mode and prompt revision when the request
reaches the input queue. The final authorization combines known titles and
visible prompt evidence with local process evidence. Linux checks the PTY's
foreground group and scoped interpreter arguments. macOS uses the foreground
group and executable paths. Windows uses executable paths from the pane's
private ConPTY job. The 20 bundled screen-rule sets from Java `1ebbe724ee84`'s
`src/main/resources/coding-agents` supplement the generic approval detector.
The Rust fixture replay verifies all 73 rule IDs across 174 pinned screens.
The actor checks for pending terminal output before and after local process
inspection. On Unix, read-only process and directory queries leave the shell
PID reserved until the session owner explicitly reaps the child.

Version-1 user rule files in the active profile's `coding-agents` directory
replace the bundled rules for their matching agent at startup. Missing,
invalid, linked, oversized, or changed files leave the bundled rules in
effect. Invalid files produce an application log warning. The Rust regex
engine rejects Java-only constructs such as lookaround and backreferences.

Local shell panes detect a coding agent when a running process and screen
rule agree. Subsequent classifications recheck the same process identity.
The Terminal settings expose the default-enabled detection preference and
apply it to open panes on save. A split tab shows one badge in Java's
priority order of blocked, completed, then working. Completion stays visible
until the affected pane is focused. Unix detection currently inspects the
foreground process group. The Java search for the newest live descendant
and remote process detection remain open.

The per-window **View → Coding Agents** dock presents the shared local agent
list. Waiting agents sort first, followed by the oldest state change and the
window, tab, and pane location. Focus resolves the live pane again after a
tab move and rejects an obsolete connection or agent instance. The menu
and command palette share the dock and navigation actions, including the
reference Ctrl+Alt and Cmd+Option shortcuts. Each window follows presence
transitions, respects a manual hide while presence stays the same, and
retains its preferred width when the journal or remote files are visible.
The profile saves automatic visibility, last placement, side, and width.
The status row counts waiting, working, and completed agents across all
windows. The primary action jumps to the next waiting pane. Its context
menu exposes the panel toggle and the same navigation action. Dashboard
rows show per-pane chips and group totals. Show in Coding Agents Panel
opens the remembered dock side and reveals the selected live agent while
preserving the existing prompt draft binding.
The dashboard's waiting-agent context menu offers Enter, Escape, and
Interrupt. It captures the exact agent and evidence when opened and drops
obsolete actions before input can reach a different pane or prompt. Keys
share the dock's guarded writer and remain available with the dock hidden.
The independent app badge counts waiting panes across all windows. It uses
the AppKit Dock tile in the macOS bundle, both native icon slots on Windows,
and the title fallback on Linux. Windows uses the pinned Java app artwork
and caps the native overlay at 9+. Obsolete icon tasks cannot apply an older
count. The title fallback uses the full count. Linux launcher badges and
dashboard per-row pulse and accents remain open.

The Messaging preference for coding agent desktop notifications defaults
to on and is independent of detection and the badge. An unseen blocked
state settles for at least 800 ms, and only a synthetic completion from
Working to Idle or Unknown can notify for Done. A one-second clock delivers
eligible transitions at least ten seconds apart per pane. The dispatcher
checks the live stream, agent instance, state, focus, and desktop policy.
Private tabs are excluded. Notification text uses the alias or agent name,
location, working directory, and available rule evidence with the existing
credential and managed text masking. Clicks re-resolve the live pane after
tab moves and respect the modal owner.

Windows uses the shell tray with one current balloon and a one-shot click.
The packaged macOS app uses Notification Center and requests local alert
permission on the first eligible notification. Linux uses the native
FreeDesktop notification service on the session bus. The Linux dispatcher
correlates actions with the reply sender and notification ID and can pass an
activation token to a Wayland compositor. Each operating system controls
whether a notification is presented.

The native Rename dialog applies a memory-only alias to the pane. It follows
a replacement agent or connection in the same pane and clears with the
pane. Quick keys use the pinned byte sequences. The prompt editor binds its
draft to the selected target and clears it after successful delivery.
The writer authorizes the exact agent process and live screen before input
and checks the screen revision again for a waiting decision. ConPTY also
requires a current screen rule because a job can contain background processes.
The actor allows one confirmation per blocked screen revision. The writer
checks for newly arrived output before writing the authorized bytes.
Free prompts are refused while the agent is waiting for a decision.
Multiline prompts use the pinned CR and bracketed-paste encoding and
require active bracketed paste. This prevents the first line from opening
a new decision that a later line could answer. Application logging records
the successful action type and number of bytes.

The detector carries the matched rule ID and bounded visible evidence in its
live pane frame. The line, joined-regex match, matching title, and rule ID
follow the Java evidence precedence. The card shows the first printable
line, limited to 80 UTF-16 units. Explain opens a read-only snapshot with the
rule, state and duration, process ID and executable name, start time when
available, alias, and pane. The snapshot closes after 60 seconds, on a second
click, on hide, and when the pane changes agent. Password prompts suppress
the evidence and drawer. Copy respects the application clipboard policy.
Application logging records that the drawer opened. The configurable host
AI shortcut and its prompt conflict check remain open.

Batch snippet reports publish rendering progress and support cancellation before
commit. The shared terminal parser treats CSI `a` and CSI `e` like pinned
SithTermFX. The Java replay input and output SHA-256 hashes are
`e7f5c3dd1c61a75c135024cb43cb13c770dcfbc74db8dbcfaea68ff80213daf8` and
`24a77c79735b2997bdc5bfb68ded4a9e7bcffa31be646f4fbe222a7427d52c88`.
CUF and CUB now use the first numeric count in the shared ANSI path, with
horizontal margins retaining their existing behavior. The input and output
hashes for the 16-column, five-row replay on ten pinned Java ANSI profiles are
`2ccdf20efb48d208bb0abfa1ea1ebf102b1b139773fbeb97f0b5f761745f97b5` and
`f86e113dd5fdc04dcf34273ca10bc76da7555faf9e528846b486d5c1a4a2c26b`.
In the shared ANSI path, DECOM now preserves the live cursor and pending wrap
when enabling and disabling origin mode. The input and expected capture for
the ten-profile Java replay have SHA-256 hashes
`7367936f0a8394ba30aedf2ebac8dd3479bbb806f860a97537d5948a36280b2f` and
`26482b0e103261cc0ece485729fd95b4787c2e3299f03b17a4130233da03beb6`.
Plain alternate-page selectors 47 and 1047 now carry the live cursor and
pending wrap across the switch. The ten-profile Java replay input and capture
have hashes `fddcc31777435545315e1baf761143d07ffa119ff1a532238e345a09dee78179`
and `ed2cd6a884dc436e248d5b110529856aca6b8fce27567e1bd0643c14f74184ba`.
Mode 1049 shares the saved cursor with mode 1048 and DECSC/DECRC. The pinned
Java replay includes redundant switches and both saves and restores across
pages. Its input and capture match all ten ANSI profiles, with hashes
`4f8ddada21808396f158186c07d0cab29425be135b10c351ae483cba9aea0504` and
`c8fb9a1183afd3209faad8c75ea56de911698ee9c5e9c264511bec9eae05449b`.
Separate Rust tests cover resizing while the primary page is hidden and
preserving a later cursor save in the alternate page.
The shared ANSI RIS and DECSTR replays keep the saved cursor and tab stops
while returning to a cleared primary page. DECSTR retains scrollback.
The ten-profile Java replay hashes for DECSTR input and capture are
`79584c79675faa71ad206d93142b983603f59de2e0716cf6a83660d13d596254` and
`eefd568a37023c922350453815d2ba20654550b01e8394caee698dfaf985862d`.
The corresponding RIS hashes are
`dbb6f4939f8c956daf840e3dbc5a1f2b1912dcc56b15fe5eec1ff5d1b9d9c124` and
`192be6c078a1cc99224a43720c904767bcd7272400476ec31ec28ce8941a2ab5`.
SU and SD use the first numeric count, default to one when omitted, and
leave rows and history unchanged for explicit zero. Active DECLRMM retains
its rectangular count rules. The new replay matched all ten pinned Java
ANSI profiles. Its input and capture hashes are
`53073b2de58019b8657c64f4c8975d1c1a8cab7d63fa509bcf5146338de71542` and
`1385f6505f44ca4f17832716fdc24630ba2b40c1c19a2d7a3c9d767662bce66c`.
ICH, DCH, and ECH apply the same count rules outside active horizontal
margins and retain the DEC conformance gates. The replay checks separate
zero, omitted, and leading-empty counts, the cursor reports, and cell
styles across the ten pinned ANSI profiles. Its input and capture hashes
are `b93eef3778259bda5bd285bff5724d3e4fa31d7af9e3e96fa8e935c5d51b0b13`
and `d212ae45e12a847f202e4f66e5dd2c23b037427d30f9ff7208aaf63e49e62c96`.
ED 3 clears the active page's history, visible rows, and image placements
while preserving the cursor and pending wrap. The hidden primary page stays
intact when the alternate page is active. A one-row grid retains its visible
line, matching the pinned reference. The ten-profile replay input and
capture hashes are `849d1e73c8d1496cc4b43cf8ffbba09531f5a4148c9f45becd9eb2defa0f319e`
and `a91f783bc199e5ac15b018fe9f4d5df13112b26111394c75d82bf8cb05090b69`.
Ordinary ED 0 preserves the single remaining row below the cursor. ED 1
clears the full cursor row when a preceding row exists. Selective erase
and active horizontal margins retain their own rules. The ten-profile
Java replay input and capture hashes are
`aea02c23a481edbd437ac4c55bcd4950204732ca96da35309e48e0e062ed0c91` and
`35033d3055a570b55acac63c6b23ae859f2da3d566d34fd38e9bd99e95cb2d92`.

On Windows ARM64, the offline workspace suite passed 1,983 tests with 11
ignored across 61 targets, with formatting and Clippy also passing. Screenshot
attachments now publish through a handle-relative directory rename with a
bounded retry. The automatic rule ledger uses a bounded retry under the profile
lock and checks the previous bytes and file stamp before each replacement. Its
concurrent-worker test passed five isolated runs and the workspace suite. The
macOS ARM64 all-targets cross-check passed. Native macOS acceptance remains open.

On Fedora ARM64, Clippy and 2,057 offline workspace tests passed with 69 ignored
across 61 targets. The real Unix PTY regression confirms that checking the
shell's process and working directory preserves its exit status until reaping.
The Windows and Fedora Control API socket tests verify that
revoking a pending request and dropping its reply still returns `Disabled`.
The Windows socket test also passed five isolated repetitions.
The earlier native X11 test opened a saved chat,
canceled the command review with Escape without creating its result file, and
verified that the confirmed command wrote the exact expected contents. The X11
binary SHA-256 was `0fe7c810c3d668a82aa89264b0a1f0f0850e07c4c8fa6b1792b0975e46e8c169`.
The native Wayland test repeated the saved-chat flow on Fedora ARM64 with
headless Sway and XWayland disabled. Both Escape cancellation and the exact
contents written by the confirmed command were verified. The test keeps an
idle `wtype` virtual keyboard and a `zwlr_virtual_pointer_v1` client attached
throughout the interaction to retain seat focus and pointer capability.
A local executable named `gemini` was used as a PTY test double. A valid profile
rule blocked insertion on its custom screen. With an invalid rule file, startup
reported the rejection, the bundled Gemini sign-in screen still blocked insertion,
and the custom screen received the exact expected bytes.
The final Wayland binary SHA-256 was `8ebc08e536ce79731c4145563b810cbff367e4702e98021e288e9730b84f38ad`.

With passive local detection and the 1049 changes, Windows ARM64 passed
1,988 offline workspace tests with 11 ignored across 61 targets. Clippy,
formatting, and the macOS ARM64 all-targets cross-check passed.
Fedora ARM64 passed Clippy, formatting, and 2,063 offline workspace tests
with 69 ignored across 61 targets. A live PTY regression covers the process
and screen evidence, disabling and re-enabling detection, rapid preference
changes, and clearing the agent after its process exits.
The native Wayland run used headless Sway with XWayland disabled. A foreground
PTY test double displayed the pinned Gemini sign-in, working, and idle screens.
Its tab showed the blocked state, retained completion while in the background,
and acknowledged completion when selected. Saving the detection preference
disabled and re-enabled the status on the open pane. Exiting the agent cleared
the status. The first UI driver missed the Save label in full-screen OCR.
The retry located it within the settings footer and verified the saved value.
The verified binary SHA-256 was
`1469aebf126deadfa59b94dfe844b7e2da6a015abe25d700671bc666982817a4`.

With the dock, menu, and ANSI reset changes, Windows ARM64 passed 1,996
offline workspace tests with 11 ignored across 61 targets. An intermediate
run hit NTFS access denied while renaming a disposable journal root.
The same test passed five isolated runs and the unchanged full rerun.
Fedora ARM64 passed 2,071 tests with 69 ignored across 61 targets.
Clippy and formatting passed on both platforms. The macOS ARM64
all-targets cross-check passed.

Native X11 on Fedora ARM64 used private Xvfb and Openbox with two 1,520 × 915
kortty windows and two foreground PTY test doubles. The rendered roster
showed both waiting agents with the older one first. Ctrl+Alt+N cycled the
active native window in both directions. Focus on the first card activated
that window and its terminal pane. The card button was located from the
rendered image after OCR missed the small text.
The verified binary SHA-256 was
`91d4c7939da50e16ba61998dc8ffcc4f602b733d8f499a2c33287f428b53d88c`.

Native Wayland on Fedora ARM64 used headless Sway at 1,600 × 1,000 with
XWayland disabled. A two-tab run confirmed Next Blocked and Focus against
the visible terminal markers. The geometry run used the panel menu to
switch to the left, dragged the divider to 850 logical pixels, and saw
automatic hiding after the agent exited. With automatic visibility turned
off in Settings, the manually opened panel restored the saved left side
and width after restart. View menu registration is covered by the unit
test. The geometry build SHA-256 was
`29b84b3934d69ffa4048ce526db6cc8fd463f35b4c5b9c8812fb2d0222a991f5`.

With the agent input, alias, and SU/SD changes, Windows ARM64 passed 2,003
offline workspace tests with 11 ignored across 61 targets. Fedora ARM64
passed 2,078 tests with 69 ignored across 61 targets. Clippy, formatting,
and the macOS ARM64 all-targets cross-check passed. A real Fedora PTY
rejected a blocked prompt, an obsolete confirmation, and a second answer
without new screen output. It accepted the fresh answer and the subsequent
free prompt. The existing exact AI chat command test also passed.

The Fedora 44 ARM64 desktop executable had SHA-256
`0a0e21996c86ac5ae1e5891a9a498df72e6a0bb8ddf37f79783e43292a95d3c6`.
Under native Wayland, Enter saved the alias in its owner dialog and a
free prompt stayed in the editor while the agent waited for a decision.
The raw PTY captured `59 45 53 0d` from YES, then the exact multiline
prompt with CR line separators, bracketed-paste framing, and a final CR.
The dock hid after process exit and the next agent in the same pane
retained the alias.
Under X11 with Openbox, Cancel discarded the draft and restored owner
focus. A synthetic click that warped directly back to the original Rename
coordinates missed the control. Moving the pointer through an intermediate
point inside the owner before clicking reopened the dialog on the first
attempt. Enter then saved the alias. The native YES button reported the
alias as its target and delivered exactly `59 45 53 0d` once to the PTY.

With the Explain drawer, rule evidence, and ANSI character-edit changes,
Windows ARM64 passed 2,008 offline workspace tests with 11 ignored. Fedora
44 ARM64 passed 2,083 with 69 ignored. Both runs covered 61 targets, Clippy,
and formatting. The macOS ARM64 all-targets cross-check passed. The real
Fedora PTY verified the matched rule, evidence, live process metadata, and
privacy generation after a password prompt. The final Fedora source
manifest SHA-256 was
`190b08f9ff58255fe2ec78c22a73d21f4c9b3cf8974b72a6aca6287af1d9b3ea`.

The final Fedora desktop executable SHA-256 was
`20631cb01c0baed07e855cff85dd9b9f09edcd38cf65f93e22535867b6086a10`.
A native Wayland run in a private Sway session displayed the Gemini
authentication evidence, matched rule, state duration, and actual fixture
PID and process start. It copied the selected explanation through the
native clipboard with OSC writes disabled. Clicking Explain again and
hiding the dock both closed the drawer. A separate opening expired after
61.4 seconds while the agent and its evidence remained visible.
Native X11 in Xvfb with Openbox showed the same detection and timing. The
test set an alias and verified that the copied explanation contained the
alias, matched evidence, rule ID, and owned fixture PID while OSC writes
were disabled. Repeating Explain closed the drawer. Hiding and showing
the dock cleared a reopened drawer.

With the dashboard, status strip, knowledge source controls, and ED 3
changes, Windows ARM64 passed 2,014 offline workspace tests with 11 ignored.
Fedora 44 ARM64 passed 2,089 with 69 ignored. Both checks covered 61 targets,
formatting, and Clippy. The macOS ARM64 all-targets cross-check passed.
The knowledge tests verified reuse of a disabled folder's vectors, indexing
without opening a disabled file, compatible rebuilds, and refusal to send a
preview made obsolete by a source change on disk. A stale settings save
left the winning index intact. The Fedora source manifest SHA-256 was
`6cc3989eee1f0883a2220bb03c6f79bc3ab2a19df44b5639f91ce74fd1ce0d3b`.

The Fedora desktop executable SHA-256 was
`a3d79ee511f19777cc1d9fd2917e46a106d7c40d35f912b812d37968e98b2628`.
In a private native Sway session, the status strip reported two waiting
agents from an unrelated tab. Its primary action selected the oldest
waiting pane. The context menu advanced to the second pane and hid the
dock while retaining the strip. Renaming the second agent updated its
tab tooltip. The strip disappeared when both agents became idle and
their fixture processes were still alive.
Native X11 in Xvfb with Openbox showed the alias on the agent's dashboard
chip. The ordinary split pane omitted the Show action. The agent pane's
Show in Coding Agents Panel action reopened the hidden dock and revealed
that pane's card. Right-clicking the blocked count opened the status menu.
In a separate private X11 session, the native AI Manager saved Disable
and Enable for a file source. Reopening after each save showed the persisted
mode. Both changes stayed in the draft until Save. The original file kept
its contents and inode, and the operation created no index.

With the dashboard key actions, app badge, and partial display erase changes,
Windows ARM64 passed 2,020 offline workspace tests with 11 ignored. Fedora 44
ARM64 passed 2,093 with 69 ignored. Both suites covered 61 targets, formatting,
and Clippy. The macOS ARM64 all-targets cross-check passed. A hidden Win32
window accepted both native icon sizes and released their handles on destroy.
The final Fedora source manifest SHA-256 was
`ab9005e7c691ddccbb10ff6895726034679d13b86b10967d004fdd52bc0cecdb`.
The desktop executable SHA-256 was
`68d56d97557684d54b79b2cd8faae3811837bdfdd310859ddb9d139a5ac9ce45`.

In a private native Sway session, a legacy profile showed a plain window
title until the Gemini fixture began waiting. The title gained a `(1)`
prefix. Saving the disabled and re-enabled preference in the searchable
Terminal settings removed and restored the prefix. Reopening Settings
showed the saved preference. The prefix cleared when the fixture became
idle while its process stayed alive.

Native X11 in private Xvfb with Openbox omitted direct key actions on the
ordinary split pane. The blocked pane's dashboard menu delivered exactly
`0d 1b 03` for Enter, Esc, and Ctrl+C while the roster was hidden. A new
blocked frame removed the stale actions, and Return sent no input from
that menu. The raw PTY received no extra bytes.

With desktop notifications enabled, Windows ARM64 passed 2,025 offline
workspace tests with 11 ignored. Fedora 44 ARM64 passed 2,097 with 69
ignored. Both suites covered 61 targets, formatting, and Clippy. The macOS
ARM64 all-targets cross-check and cross-target Clippy passed. The hidden
Win32 window test verified one-shot click routing for the current
notification ID. Native macOS delivery still requires desktop acceptance.

The Fedora source manifest SHA-256 was
`7709b76225a9e2ee67e48842bda33f92e6b8aaeaf0147386df395480a49cd00b`.
The desktop executable SHA-256 was
`684a736f248efa07922cf80fa18c6122fb82be88e16327a1f5d784b84219e567`.
The exact Linux windowing library passed a private-bus probe for escaped
markup, token and tokenless activation, rejection of foreign and duplicate
signals, revocation during capability discovery and before clicking, and
recovery for a later permitted delivery.

In private native Wayland, a hidden Gemini pane notified while a different
tab in the same kortty window had compositor focus. An explicitly
isolated notification test double provided the default action and a
genuine Sway token from a keyboard event in the focused witness window.
The exact app sent the token on its native surface. With the private
compositor's smart activation policy, Sway focused kortty, which selected
the Gemini tab and delivered subsequent typed input only to its PTY. With
Sway's default urgent policy, the same activation request left focus to
the compositor's urgency handling.

In private X11 with Xvfb and Openbox, a visible blocked pane made no
notification service calls. An unseen blocked pane sent one notification
with the expected title, escaped tab title, working directory, and
authentication evidence. A signal from an unrelated sender left the
control window focused. The test double's default action raised the
owning window and selected the previously hidden tab. A replay did not
activate the pane again.

On Windows 11 25H2 ARM64 build 26200.9711, a private-profile acceptance
build started an owned Gemini test double through ConPTY in the second
pane of an inactive split tab. The Windows shell accepted the desktop
notification. The first reported click produced no application activation.
On a fresh run, a user click on the Windows notification switched from
tab 8, session 7 to tab 3, session 6. Session 6 was the blocked agent,
and session 2 was the split tab's initially focused pane. The trace
verified the selected tab and pane. Foreground ownership at the time of
the click was not measured. The acceptance build used a temporary fixture
hook for setup and observation while keeping the notification transport
and click handler on the production path. Its SHA-256 was
`edd7ca40ae6298ce364403a8e1be9e2c260103bb0f473229fbfeeec64280e7a6`.

### Saved local profiles and connection overrides

The D1 and D2 implementation on 2026-11-09 adds local shell profiles to
the Connection Manager, Quick Connect, Command Palette and Open Recent.
The native PTY accepts a program, exact argv and a working directory.
Private layouts resolve saved profile references in the local store.
Portable projects reject those references. The XML connection import
previews the command and directory before saving. Java-compatible launch
settings use the Java fields and Rust-specific rows preserve exact argv
that the Java tokenizer cannot represent.

Saved SSH and local shell profiles can override terminal font, theme,
colors, encoding, close confirmation and the default terminal AI chat
profile. Appearance follows saved edits in open panes. A new session or
reconnect uses the selected encoding. The editor can restore inheritance.
The independent highlight and skill assignment work remains in D3 and B7.

Native Fedora 44 ARM64 PTY tests passed (11 passed, 2 ignored) in an
isolated PTY crate copy with the workspace's locked dependency versions.
The macOS ARM64 workspace compiled with `cargo check --all-targets`.
On Windows ARM64, formatting and workspace Clippy passed. The complete
workspace suite passed 2,032 tests across 61 targets with 12 ignored.
It covered the manager's save, duplicate and cancel workflow, a native
ConPTY launch through the manager, exact argv and directory delivery,
private layout references, live inheritance, close cancellation and
XML round trips. The real ConPTY regression also verified `cmd.exe`
starts with mixed separators in the configured path after normalizing
the Windows image path. Desktop GUI acceptance and a live Java XML
interchange run remain open.

### 2026-11-09 remaining-plan integration checkpoint

The next implementation batch added general terminal notifications, bounded
rule-based highlighting, additional structure languages and snippet AI actions,
coding-agent API/events/CLI routes, serial library interchange, native LM Studio
and reviewed remote HTTPS chat, streaming, local text attachments and conversation
export. `PORTING-PLAN.md` records the remaining work and route restrictions.

Windows ARM64 passed all 272 terminal-crate tests and all 20 control-crate tests.
The first full application run passed 1,624 tests, failed two and ignored eleven.
Its failures were the provider picker's changed option order and Perl array
references after concatenation. Both were corrected afterward. Workspace Clippy
passed for that snapshot.

The same batch was copied to a private Fedora 44 ARM64 run with source-manifest
SHA-256 `8f74dfd5a8554aa81d6db4a924ccee230c2439aa712a4989b0dc055e131f7097`.
Fedora passed Clippy and 1,692 application tests, failed four and ignored 49.
Two failures matched Windows. Another exposed the new desktop notification
overwriting the existing activity/silence tab label, which was corrected.
The last was the user's Bash prompt replacing a title fixture's OSC title.
The next run uses `SHELL=/bin/sh`, matching the existing Linux test convention.
These failed runs do not establish a green workspace result for the corrected
source. Subsequent skill-ID, live-assignment and notification-deduplication
fixes require another complete check.

The user approved platform TLS for remote HTTPS AI on 2026-11-09. The locked
Windows graph uses native-tls and Schannel. Fedora uses the installed system
OpenSSL headers and runtime. Physical IBM/HP/serial acceptance was deferred by
the user. The Fedora source/build copy and its private test directories were
removed after the check. Its post-clean source directory was 23 MiB before
removal. Windows development artifacts remain in use by the next check series.

#### Corrected integration snapshot

The subsequent frozen snapshot includes J1 automatic/confirmed-input journal
capture, B7 persistent quotas and private-chat accounting isolation, stable skill
IDs, saved snippet diagrams, notification deduplication and the corrected earlier
regressions. Windows ARM64 passed formatting, workspace Clippy and all workspace
targets with 2,133 passed tests and 12 ignored tests across 62 targets. Fedora 44
ARM64 passed the same checks with 2,203 passed and 70 ignored across 62 targets,
using `SHELL=/bin/sh`. The Fedora source-manifest SHA-256 was
`ac6fd7a6f72213a39d753ef736db10ac2995686110af31928d52d062c11350c5`.
The guest development target was cleaned and the 24 MiB source copy and private
run directory were removed. The macOS ARM64 workspace/all-targets cross-check
passed with the locked Security.framework dependencies.

The native Windows GUI binary for the next manual checks has SHA-256
`bc3177a17c71565c8ed70329c245515941c7800dee5f4d34eb87309d07f93f5b`.
Those GUI checks use a separate temporary profile. Further source changes for
agent execution, structured snippet hardening and online runtime updates began
after this binary was built and require their own verification.

The Windows inspection opened the highlight manager, duplicated a built-in set,
checked its live styled preview and discarded the unsaved draft through its
confirmation. The connection manager exposed the saved journal controls, and
the AI Manager exposed the profile quota/cost form and unsaved-close prompt.
The test application exited successfully and its temporary profile was removed.
This inspection found two desktop issues. Popup shadows painted outside the
current damage rectangle, accumulating black regions when a long menu scrolled.
The renderer now clips shadows and handles shadow-only damage, with a passing
pixel regression and all four renderer tests. SSH and serial profile editor
buttons also moved to the fixed footer already used by local profiles. Native
rechecks of these corrections follow the next application build.

The opt-in `crates/ssh/tests/exec_live.rs` test passed on Windows ARM64 against
the authorized Fedora SSH server. It used the existing key and pinned host key,
kept the original authenticated terminal alive while opening an independent exec
channel, verified separate stdout/stderr and exit status 23, then verified channel
abort and reopening. Its short remote cancellation fixture finished before the
test exited, and the temporary trust store was removed. Isolated-worker exec and
the complete built-in agent workflow have separate acceptance checks.

The corrected native Windows binary has SHA-256
`7a20acb65ebe3548addd4029dda8ae659f9eab97ac7ea8bf169152aea61ac881`.
The long View menu now renders correctly while scrolling forward and backward.
The SSH connection editor keeps Cancel/Save visible in its fixed footer both at
the top and after scrolling to the final fields in an 862-by-722 window. Serial
profiles use the same footer but were not independently exercised in this pass.

The A1, structured-hardening and signed-runtime snapshot passed Windows ARM64
formatting, Clippy and 2,153 workspace tests with 13 ignored across 63 targets.
The macOS ARM64 workspace/all-targets cross-check also passed. The Windows run
used a normal user token. An earlier restricted-token run failed private-profile
ACL checks and is excluded from acceptance evidence. Its five focused agent
tests passed immediately with the normal token before the full rerun.

Fedora snapshot `902d4cd4e2ece62dfbdbe82b339712166b41dff5a31c6e7996c7d192b7a40ac7`
passed formatting and Clippy, then 1,742 application tests with two failures and
49 ignored. Both failures were dependency-only apply/recovery fixtures whose
expected two stages now also selected the new default hardening options. The
fixtures now explicitly clear these options and still verify byte-identical
stored recovery independently of fresh defaults. The Windows full rerun includes
those corrections. Fedora requires a subsequent rerun. Its development target
and 24 MiB source copy were cleaned and removed after the run.

The later native app/test pair used app SHA-256
`bd1897db6db99a5b976d3e8cc29b9320fb2e6bc2241497201edde1c026117dd7`
and test SHA-256
`f7b3c560af6d0eb312d58c7b16e231e599d0e43e619c776c16570f62cdfc94c1`.
The isolated worker exec smoke passed against Fedora, preserving the original
shell and verifying separate output/error, status, cancellation and reopening.
The native Windows runner smoke found PowerShell CLIXML progress mixed into
stderr. Explicit text output and UTF-8 setup have been added and require a rerun.
The native agent dialog showed the reviewed Windows directory and exact probe
before approval. Cancelling at this point left the transcript empty. Its final
cancellation message is being corrected. The app exited and the private profile,
test executable copies and scratch files were removed.

Forwarding now admits thirteen simultaneous streams and reserves one channel
each for the terminal, attached SFTP and agent exec. Earlier fourteen-stream
verification records above describe the earlier two-reservation implementation.

The corrected native app/test pair used app SHA-256
`d61e5c39b18f91ca027ec028bd323773160bd5b669f2e7e4f147a56e7a95feec`
and test SHA-256
`effd0e7db701fca4cf11a726739c8d436761f73ecc070a8f4e2df1d6ac29a388`.
The Windows agent runner smoke passed with separate Unicode stdout/stderr,
explicit exit status, CMD OEM output and owned-background-process termination.
The isolated SSH worker smoke also passed against Fedora, including cancellation
and reuse while retaining the original terminal. Both tests ran from private
executable copies that were removed after the test application exited.

The native MCP CLI connected to the same private Windows desktop profile and
passed initialization, the five-tool read-only catalog, live pane listing and
reading, disabled-write rejection, argument validation, cancellation and clean
EOF shutdown. No terminal write was approved in that native run. The desktop
application exited with status zero, and its complete private profile was removed.

The first full Fedora integration attempts found an unimported Unix `Path` and
large enum variants in MCP input and modularization state. The Windows run also
reported a redundant formatting borrow. These were corrected with a Unix import,
shared MCP permit storage, boxed review state and the narrow formatting fix.
The corrected snapshot passed Windows ARM64 formatting, workspace Clippy and
2,169 tests with 15 ignored across 63 targets. Fedora 44 ARM64 passed the same
checks with 2,239 tests and 72 ignored across 63 targets, using `SHELL=/bin/sh`.
The Fedora source-manifest SHA-256 was
`79ee35f0f58585b29551ffc6cef22c04548145bcba148e5ea5b4046af03972ed`.
The macOS ARM64 workspace/all-targets cross-check also passed. The guest
development artifacts, 24 MiB source tree and private run were removed.
After all three verification runs completed, `cargo clean --profile dev`
removed 5.1 GiB of development artifacts. Explicit cross-target clean commands
found no remaining development files. The checkout occupied 348 MiB and
`target` occupied 293 MiB. The verified Windows ARM64/x64 package archives and
checksums were retained, together with the existing SSH fixture key directory.

The next integration added structured planning with saved destination-bound
reports, reviewed Hugging Face acquisition with resumable downloads and a
dedicated vault credential type, and reviewed folder scanning options for
knowledge stores. Windows ARM64 passed formatting, Clippy and 2,181 tests with
15 ignored across 63 targets. Fedora 44 ARM64 passed the same checks with 2,251
tests and 72 ignored across 63 targets. The Fedora source-manifest SHA-256 was
`ec37ea36a81d3e0562ca27511df703c8939d66785b12cadee963714f8aeb9339`.
The macOS ARM64 workspace/all-targets cross-check passed. The guest development
target and 24 MiB source tree were cleaned and removed. No public Hub model was
downloaded for these tests. The transfer regression used a local HTTP fixture.

The opt-in OpenSSH forwarding-capacity test passed on Fedora with thirteen
simultaneous forwards, a rejected overflow request, attached SFTP, independent
exec and the original terminal still usable. The first isolated fixture used
`/var/tmp`, which OpenSSH StrictModes rejected as an ancestor of its key file.
The successful fixture used a private directory under the test account's home.
It also exposed an exhausted-channel error reported as a protocol failure,
which now correctly reports `WouldBlock`. The final shell marker starts on its
own line and disables shell-history saving for the fixture. The successful
source-manifest SHA-256 was
`fd8e7b451d7a4a4259fd793e69c18046569a70331b8c409b3b3c32bf926e55cb`.
Its source/build copy and private test directories were cleaned and removed.

The following integration adds periodic and closing journal summaries,
re-evaluation and durable catch-up, initial live-pane AI and POSIX-script swarm
runs, and copied split-GGUF imports. Windows ARM64 passed formatting, Clippy
and 2,197 workspace tests with 15 ignored across 63 targets. Fedora 44 ARM64
passed the same checks with 2,267 tests and 72 ignored across 63 targets, plus
the focused no-AI swarm regression. The initial Linux fixture assumed the AI
configuration was absent. It now accepts an empty configuration while checking
that no AI profile or model work is available. The corrected source-manifest
SHA-256 was
`5c42a72d1bb506cfcdc88e30dd247430c705f954ce15ff12281f2dd3c12de24b`.
The macOS ARM64 workspace/all-targets cross-check passed after that correction.
The guest run removed its development target and 25 MiB source tree. Native
GUI acceptance remains separate. Automatic knowledge sync, journal Q&A,
Windows swarm scripts/reports and persistent SFTP receipts were integrated
after this snapshot and require their own verification.

The next snapshot adds approved automatic knowledge reconciliation, grounded
journal questions and citations, Windows swarm scripts and Markdown reports,
and persistent single-file SFTP recovery. Windows ARM64 passed formatting,
Clippy and 2,221 tests with 16 ignored across 63 targets. Fedora 44 ARM64 passed
the same checks with 2,291 tests and 72 ignored across 63 targets. The corrected
Fedora source-manifest SHA-256 was
`b658066d6b6d6c99cdd278facfb43f54c6d7d2d38e34cbfaa3aaebfae60d9997`.
The macOS ARM64 workspace/all-targets cross-check passed. The first test pass
exposed a chat-only fixture used for an embedding request and missing delete
access on reopened Windows staging directories. The fixture now checks the
actual embedding wire request, and recovered directories use the verified
handle with deletion access. The successful rerun includes cleanup and
completed-publication recovery regressions. The Fedora run removed its build
target and 25 MiB source copy. Additional transaction-edge work and native
PowerShell acceptance are recorded separately.

The opt-in native Windows PowerShell swarm test passed against the newly built
desktop executable. It used a saved script with a parameter block, a leading
dash argument, Unicode and literal command-substitution syntax, and checked
separate stdout/stderr plus exit status 29. The application SHA-256 was
`b2eeba4decebb10404d8589068e78b6784bce8a1ca50821dd9e238adec2784af`.
The test executable SHA-256 was
`fac20e82b44fe34d08ad1ca7ef211d896f24fae346d3700ac868434acab4e7c1`.
The copied executables and private profile were removed after the test, and
the run checked and stopped any remaining processes launched from its own
private executable directory.

After this verification series, the development clean removed 4.3 GiB. The
explicit cross-target clean commands found no remaining development files.
The checkout occupied 349 MiB. The existing verified Windows ARM64 and x64
package archives and checksums were retained. Private native test copies and
the Fedora source/build trees were removed.

The subsequent integration includes reviewed swarm workflow generation,
externally registered GGUF models, copied vision projectors, and per-target
automation journals for scheduler and swarm runs. It also makes interrupted
SFTP download discard retryable after a receipt write failure. Windows ARM64
passed formatting, Clippy and 2,236 tests with 16 ignored across 63 targets.
The macOS ARM64 workspace/all-targets cross-check passed on the same code.
Fedora 44 ARM64 passed formatting, Clippy and 2,304 tests with 72 ignored across
63 targets at source-manifest SHA-256
`6f601ff3d004e7436cb02e1a5c635d65c9fba66876ce0f9b892fa46e51e3769c`.
That Fedora snapshot includes the Unix scheduler fixture fixes and precedes
two subsequent Windows-tested fixes for prefix-reader cancellation and a
revoked automation capture warning. Its private source and build tree was
removed. Native dialog acceptance and signed-runtime multimodal inference
remain separate from these checks.

After the Windows build and cross-check ended, the development clean removed
4.6 GiB. The cross-target clean commands found no additional development
files. The verified Windows archives and checksums were retained. A private
copy of the Windows executable was staged for the separate dialog inspection
with SHA-256
`a141fa2f0581a450b1bb93e8e7631e27b4d6ba14dcaad4654b291015fa03869c`.

### Scheduler, transfers, agent history and manual checkpoint

Windows ARM64 passed formatting, Clippy and 2,258 workspace tests with 18
ignored across 63 targets. This run covered desktop notices after scheduler
journal commit, transfer queue transitions and the incognito boundary,
resumable new-target uploads, dense automation transcript capture, multipart
GGUF recovery, agent prompt history, and reviewed manual questions with exact
loopback request evidence. The macOS ARM64 workspace and all-targets
cross-check passed on the same code.

An opt-in Windows test connected to the authorized Fedora OpenSSH fixture
with its pinned host key. It cancelled a saved Swarm target during an
exec-only command and confirmed that the owned transport closed. The
fixture's command had a bounded lifetime and created no remote files.

The native upload probe also passed from Windows to the pinned Fedora
OpenSSH SFTP subsystem. It disconnected the transport after the server
acknowledged the fifth write and before the client received that reply.
The test checked the server's accepted offset, rejected a changed byte
outside the final overlap, resumed from the exact offset, compared the final
SHA-256, and verified removal of its private remote scratch directory.

The follow-up Unix work releases prompt history and SFTP locks explicitly
before dropping their file descriptors. It also runs the coding-agent
monitor's fixture script from a private file. This keeps simulated screen-rule
text out of the launch line echoed by the shell.

Fedora 44 ARM64 passed formatting, Clippy and 2,330 tests with 74 ignored
across 63 targets at source-manifest SHA-256
`04b67324def16e67f069636f7ac747bb9a680ed4fbe87208aaa953d6ef380dc3`.
The private source and development build were removed after the run.

The Windows cleanup removed 3.4 GiB of development artifacts and the
unused private dialog copy. The checkout occupied 351 MiB, of which 293 MiB
was under `target`. The existing Windows ARM64 and x64 package checksums
matched. The native dialog inspection remains open.

### Windows native Local Models and JobScheduler dialog inspection

On Windows ARM64, two freshly built private copies were started with isolated
profiles. The AI menu opened AI Manager, and its Local Models tab displayed
the local endpoint, signed runtime controls, model and projector paths, and
Hugging Face controls. The Tools menu opened JobScheduler. Its Job and Action
pages displayed the target fields, execution identity, and outcome notices.
The initial capture showed the vertical scrollbar overlapping the right edge
of both long forms.

The second build embedded the scrollbars with a content gap. The Local Models
page at 1,122 × 812 pixels kept the endpoint, model selectors, package fields
and buttons clear of the scrollbar before and after scrolling. The Scheduler
at 1,362 × 832 pixels showed the full Browse and schedule controls beside
the scrollbar. Its Action page showed the SSH-user default and failure and
blocked notices selected. The Local Preferences page showed runtime updates
set to Off. Both apps closed through their window controls and the two private
profiles were removed. The corrected executable's SHA-256 was
`880a6689b7dfbafd8e444b6acf521328ef6554a55d7147cdf753663c2b73a597`.
The inspection used no saved credentials, model downloads or job execution.

### Scheduler elevation, transfer defaults, agent design and runtime policy

Windows ARM64 passed formatting, Clippy and 2,275 workspace tests with 19
ignored across 63 targets. The native desktop build and the macOS ARM64
workspace and all-targets cross-check passed on the same code.

Fedora 44 ARM64 passed formatting, Clippy and 2,347 tests with 75 ignored
across 63 targets at source-manifest SHA-256
`5ef5287dfe6a9b6651697b6cb124d574de874e1858e2c98cea8f6aa231a15b47`.
The snapshot includes dedicated scoped sudo credentials, managed SFTP
conflict rules, native agent presets and the opt-in signed runtime policy.
The private source and development build were removed.

The optional native Windows sudo transport test passed against the pinned
Fedora OpenSSH fixture. A synthetic remote command emitted the challenge
over SSH stderr, received one synthetic password line, observed end of
input, and returned exit status 29. Actual privileged command execution
and staged file operations have separate acceptance requirements.

Cleanup removed 4.4 GiB of Windows development artifacts. The checkout
occupied 353 MiB, with 293 MiB under `target`. The existing Windows ARM64
and x64 package checksums matched.

### Scheduler webhooks, agent aliases, SFTP comparison and runtime resume

Windows ARM64 passed formatting, Clippy and 2,292 workspace tests with 19
ignored across 63 targets. The macOS ARM64 workspace and all-targets
cross-check passed. Fedora 44 ARM64 passed formatting, Clippy and 2,366
workspace tests with 75 ignored across 63 targets. The Fedora source manifest
SHA-256 was
`dac650fbe96c5c63b22558b098855063e519f3a8e8a922cee10e65770cdebf5c`.
The private Fedora source copy and build were removed.

- Scheduler webhooks use committed outcomes, dedicated encrypted destination
  URLs and per-job assignments. Fixtures exercise journal-write retry, desktop
  independence, domain allowlist intersection, vault locking and immutable
  destination identity. A loopback receiver exercised a 429 retry and checked
  that redirects stop at the first receiver. The delivery journal stores fixed
  outcomes. Live HTTPS provider acceptance and Java's optional masked summaries
  and central target enablement remain open.
- Terminal agent aliases require a live identified shell prompt and open the
  existing setup for that pane. Tests cover bounded capture, ordinary input
  replay, changed prompts and benign primary output. Fedora also exercises
  the shortcut through a Unix PTY. Tab completion and typed elevation remain
  separate work.
- The SFTP file comparison uses checked read-only snapshots and reports
  byte equality independently of text alignment. Tests exercise policy denial,
  stale remote metadata, binary and oversized inputs, and cancellation. Folder
  comparison, excludes and directional synchronization remain open.
- The managed runtime tests exercise compatible and withdrawn pins, exact
  ranged resume, prefix and validator mismatches, and policy revocation before
  a redirected request. Resume binds the partial to the accepted signed
  catalog. A changed catalog can require a new download. Native signed
  release-channel acceptance remains open.

Cleanup removed 3.2 GiB of Windows development artifacts. The checkout
occupied 354 MiB, with 293 MiB under `target`. Both retained Windows package
checksums matched.

### Reviewed folder copy, scheduler credentials and terminal AI

Windows ARM64 passed formatting, Clippy and 2,315 workspace tests with 19
ignored across 63 targets. The macOS ARM64 workspace and all-targets
cross-check passed. Fedora 44 ARM64 passed formatting, Clippy and 2,393
workspace tests with 76 ignored across 63 targets. The corrected Fedora source
manifest SHA-256 was
`1152bae5fd6d0ff95d52f4b94e27c8735c9e5c286035359335f197d17537a80c`.
The initial runs caught an export test that assumed the model's Mermaid
node IDs survived serialization. The corrected test reparses the exported
chart and checks topology, visible labels and masking.

- SFTP folder comparisons review the roots and exclusions before reading
  checked snapshots. Directional copy revalidates its manifest and copies
  source-only regular files under existing parents. Server publication
  requires the hardlink extension and a checked private stage. Packet
  fixtures cover a changed destination, cancellation, partial publication,
  and cleanup failure. An additional Fedora test with the installed
  OpenSSH `sftp-server` passed. It verified exact copied bytes, preservation
  of an existing server file, and removal of the staging directory.
- The scheduler tests exercise authenticated webhook preferences, default-off
  summaries, masking across whitespace normalization, and deferral while the
  vault is busy. Dedicated group sudo credentials require the exact saved
  connection group and username. Endpoint-specific credentials take priority
  and ambiguity blocks the target. Actual elevation and live HTTPS receivers
  remain separate acceptance cases.
- Agent alias completion is local to a confirmed shell prompt and can insert
  masked history in regular tabs. Tests cover stale selections and ordinary
  shell Tab. The attached SSH agent's typed passwordless sudo uses an exact
  one-use command review. Tests cover quoting and refusal, with no privileged
  command in the fixture.
- Skill tests cover weighted ranking, a cap of two automatic results and
  explicit snippet selections. Terminal Chat tests exercise masked local
  attachment flowchart generation, bounded SFTP text reads, source changes,
  and revocation after pane or provider changes. SSH attachment context stays
  in the live chat unless explicitly exported. Saved structured SSH attachment
  context and native end-to-end acceptance remain open.

The private Fedora source and build were removed. Windows cleanup removed
3.2 GiB of development artifacts. The checkout occupied 355 MiB, with 293 MiB
under `target`. Both retained Windows package checksums matched.

### Windows native general notification activation

On Windows 11 25H2 ARM64 build 26200.9711, a private kortty profile restored
an inactive two-pane tab with its first pane initially selected. The second
pane ran an owned ConPTY fixture. After the user switched to another window,
the fixture emitted an OSC 777 notification. The user clicked the Windows
notification. The read-only Control API identified the original tab and its
second pane as selected, and `GetForegroundWindow` identified the private
kortty process in the foreground. The application, shells and private
profile were removed.

The preflight executable SHA-256 was
`f34fd8e12027c74cde2f917834da2cd0ed6933408fcd2bae52e707f1887347b2`.
This exercise covers general native Windows delivery and click routing.
Agent and Swarm events have separate lifecycle fixtures.

### Agent notices, reviewed SFTP browser work, and application updates

On 2026-11-09 the workspace passed formatting, Clippy with `-D warnings`,
and all-target tests on Windows ARM64 and Fedora 44 ARM64. Windows reported
2,338 passed tests and 19 ignored tests across 63 targets. Fedora reported
2,418 passed tests and 76 ignored tests across 63 targets with `SHELL=/bin/sh`.
The macOS ARM64 workspace and all-targets cross-check passed.
The Fedora test copy source manifest SHA-256 was
`ad18d01b2719add5736182c3959ea230c88242e01264a799880bbd3ac5ee4d7a`.

- Agent and Swarm fixtures covered phase transitions, fixed notice text, focus
  suppression, private mode, and the reviewed stream and round. The native
  click run above exercised the shared activation path.
- `openssh_reviewed_folder_copy_publishes_without_replacement` passed against
  Fedora's `/usr/libexec/openssh/sftp-server`. The reviewed copy created three
  folders, including an empty one, and published two files with checked
  whole-second modification times. The excluded subtree was absent and the
  existing file was preserved. Separate SFTP packet fixtures covered sorting,
  recursive search, exact result selection and stale or cancelled results.
- `native_sudo_handshake_sends_one_line_over_exec_without_a_pty` passed from
  Windows over pinned OpenSSH to Fedora with a synthetic stderr challenge and
  one line of input followed by EOF. The fixture exercised the real SSH
  transport without invoking sudo. Agent password approval remains separate.
- Update fixtures covered stable archive selection, SHA-256, architecture,
  destination collisions, cancellation, managed-policy revocation and the
  saved background cadence. The archive checks exercised ZIP and TAR
  fixtures. A live published Rust release and platform installation remain
  separate acceptance cases.

### Agent password approval, scheduled SFTP, Java recording import, and SSH attachments

On 2026-11-09 Windows ARM64 passed workspace formatting, Clippy with
`-D warnings`, and 2,352 tests with 21 ignored across 63 targets. Fedora 44
ARM64 passed the same checks with 2,432 tests and 80 ignored across 63 targets
using `SHELL=/bin/sh`. Its source-manifest SHA-256 was
`c3a3f00ce4d2df7fd1b598153f22e28e3d629bfe671bb6a09a2b6b112e4eea6e`.
The macOS ARM64 workspace and all-targets cross-check passed.

- The Windows direct SSH tests passed over the pinned Fedora connection with
  synthetic password challenges. The Agent test confirmed one line followed
  by EOF and a live original shell connection. The password-notice fixture
  accepted only the current owned challenge. These cases used no privilege
  elevation. Native manual input and isolated SSH password input remain open.
- The scheduler editor fixture saved and reopened a reviewed SFTP job. The
  first claim consumed the durable approval and a second claim was rejected.
  The backup projection fixture checked removal of the review reference.
  The separate `openssh_scheduled_reviewed_copy_runs_once_in_both_directions`
  test passed through real OpenSSH for upload and download. Its private Fedora
  test root was inside the home directory to satisfy `StrictModes`.
- The recording manager fixture imported a Java recording through the normal
  action, refreshed its catalog and opened the resulting native replay. A
  separate Windows run used `TerminalRecordingSession` from Java revision
  `1ebbe724ee84d55f31295ec3060eecad3653d2f2` with a fixed test clock, Microsoft
  OpenJDK 21.0.12 and the cached Gson 2.14.0. The two helper methods were copied
  from the same pinned Java service into the isolated writer harness. The
  GZIP fixture SHA-256 was
  `acd94e47a2322004214ae228cab1694200181881a8a1e0e2106e4570bbbd69ff`.
  The Rust import verified two panes, geometry, wide and combining cells,
  style and concealed text, and the one-second replay duration. Reciprocal
  Java recording export and video encoding remain open.
- SSH attachment fixtures checked exact UTF-8 names, changed paths and
  metadata, and refusal to review a flowchart without the original source.
  `openssh_chat_browser_lists_and_loads_a_reviewed_file` passed with Fedora's
  `sftp-server`, including rejection after the listed file changed.
  Local and SSH flowchart requests use the reviewed masked snapshot. A live
  model response from an SSH attachment remains a separate acceptance case.

### Archived chat attachments, isolated SSH passwords, and optional tools

On 2026-11-09 Windows ARM64 passed workspace formatting, Clippy with
`-D warnings`, and 2,364 tests with 25 ignored across 63 targets. The final
Windows suite included encoder replacement checks and archive reopening after
removal of the original attachment. Saved attachment context uses the same
masked initial turn on follow-up review and remains subject to managed policy.
The macOS ARM64 workspace and all-targets cross-check passed.

The pinned `AiChatManager` at `1ebbe724ee84d55f31295ec3060eecad3653d2f2`
generated the attached-chat XML using Microsoft OpenJDK 21.0.12 and JAXB
4.0.9. The private fixture used a no-op telemetry stub and cached SLF4J
2.0.18. The Java input SHA-256 was
`4b69896ed91220b4bc06d6552adbcdd48ce38bc5b312299dc5c4daf023d145f1`.
Rust imported the file with masking and exported the selected terminal text
and attachment fields separately. The Rust XML SHA-256 was
`3976d56162bf15327b116bf56d877908bf1ddd08b5156f129cc7008b93e8089c`.
The same Java manager loaded, saved and reloaded the Rust output with its
Unicode filename, masked source and body, and original message order.

The Windows `isolated_worker_password_exec_commits_one_line_and_never_relays_output`
fixture passed against pinned OpenSSH on Fedora with a process-isolated worker.
It exercised a stdout decoy, a fragmented stderr challenge, one committed
synthetic line followed by EOF, suppressed echo on both output streams, and
continued shell use after cancellation. This fixture exercises the transport
with a synthetic challenge. Native manual entry and actual elevation are
separate acceptance cases.

Fedora 44 ARM64 passed workspace formatting, Clippy with `-D warnings`,
and 2,448 tests with 86 ignored across 63 targets using `SHELL=/bin/sh`.
The suite's source-manifest SHA-256 was
`929439da0a4941a0700d952fa6b190a2aaa00cc07b08d47ee637a3284feff18d`.
The local OpenSSH process-isolation password fixture also passed. It used a
generated strict host pin and one authenticated connection.

The Fedora FFmpeg and ffprobe 8.1.3 fixture exported a 275 ms range in both
WebM/VP9 and MKV/FFV1. It checked the 656 × 64 pane composite, exact packet
times at 0, 25, 125 and 274 ms, complete decode, and staging cleanup.

The final rsync change passed app formatting, Clippy, and eight focused
tests in four consecutive runs on Fedora ARM64. With rsync
`3.5.1-g315819c5` (protocol 33), the
`openssh_scheduled_rsync_download_publishes_verified_new_file` fixture used
one pinned OpenSSH connection for the sender and SFTP verification. It
checked the 196,610-byte result, refusal under denied policy, and preservation
of the original file and an existing destination. The bridge uses a compact
Unix socket path, flushes each binary handshake chunk, and handles SSH
input closure at the end of a successful transfer. The focused run's
source-manifest SHA-256 was
`8323c3795426d3f2f914fef38652e9ae727b22a9076ce3acc2df1db15cbc9118`.
The final rsync code also passed the macOS ARM64 all-targets cross-check.
Native macOS rsync execution remains a separate acceptance case.

### Windows password entry, Java replay export, and reviewed research

On 2026-11-09, Windows ARM64 passed workspace formatting, Clippy with
`-D warnings`, and 2,377 tests with 26 ignored across 63 targets. The Win32
tests checked password buffer bounds and wiping, the native edit control's
mask, rejection of injected text, and restoration of only the disabled
owner. Separate direct and process-isolated SSH synthetic password
fixtures passed against Fedora OpenSSH. Foreground prompt interaction and
actual sudo elevation remain native acceptance cases.

The pinned Java `TerminalRecordingService.loadReplayFrames` at
`1ebbe724ee84d55f31295ec3060eecad3653d2f2` opened the Rust GZIP export on
Microsoft OpenJDK 21.0.12. The harness used cached Gson 2.14.0, JAXB API
4.0.5, SLF4J API 2.0.18, and a stub for the recording settings getters
unused by the reader. It verified two one-second 19 × 2 composite frames,
wide and combining Unicode, foreground and background colors, style flags,
blank hidden cells, and exclusion of content after the range. The fixture
SHA-256 was
`56dc1280d573d3c3cd027e35253e8a8c12f1e185c9161317bb64be209ce2a7d7`.

The workspace suites exercised the Tavily request on loopback, exact
masked query review, no dial from canceled or invalid inputs, the disabled
and private chat gates, policy revocation, and the separate model preview.
Saved Chats XML retained masked display provenance with URL queries,
fragments and user information removed. Public-provider setup and
automatic provider tool calls are separate acceptance and implementation
cases.

Fedora 44 ARM64 passed workspace formatting, Clippy with `-D warnings`,
and 2,462 tests with 88 ignored across 63 targets using `SHELL=/bin/sh`.
The checked source-manifest SHA-256 was
`d8f0201bee46cc15bfa7a8f80db8c85e43e6074596387183b30bce7f0eae7d62`.
The opt-in scheduler Swarm test saved, reopened and ran an approved fixed
diagnostic with a pinned OpenSSH target and a fixture-blocked target. It
verified the physical directory and masking in the loopback model request
and the retained individual results and masked advice in the job journal.
The separate FFmpeg/ffprobe fixture passed again for both video formats.
The macOS ARM64 workspace and all-targets cross-check passed.

### Local project file tabs, live snippet analysis, and Agent export

On 2026-11-09 Windows ARM64 passed workspace formatting, Clippy with
`-D warnings`, and 2,387 tests with 26 ignored across 63 targets.

On 2026-11-09 Fedora 44 ARM64 passed workspace formatting, Clippy with
`-D warnings`, and 2,473 tests with 88 ignored across 63 targets. The
final source manifest was verified before the run. Its SHA-256 was
`8aa9f1cda4720ef92c221f30d268045d80b37dd8dd5f3e85962b18f45f168e0c`.
The macOS ARM64 workspace and all-targets cross-check passed.

The project fixture restored a text editor and an image in their saved
order, opened the selected image at its saved zoom, retained an unsaved
editor change after canceling close, and captured and exported both tabs.
The file-open regression rejected file and parent-directory symlinks on
Fedora. Remote editor, remote image and SFTP tool restoration remain
separate work.

The pinned Java `ProjectManager` at
`1ebbe724ee84d55f31295ec3060eecad3653d2f2` loaded, saved and reloaded the
Rust project on Microsoft OpenJDK 21.0.12 with cached JAXB 4.0.9 and its
supporting libraries. The harness used an unused history-storage stub
that fails if invoked. It checked both local paths, their tool types,
the editor title, saved tab order and active editor, and image zoom.
The Rust fixture SHA-256 was
`a53c8a9699957f781f8537ab0bef4476e8de5a788194e0d32372c7936c1ff675`.
The Java re-export SHA-256 was
`482128bdea79a1b7e174165ffca74d2290a3de284a18313d963baf5104d65294`.

The snippet analysis loopback SSE fixture held the response while the UI
displayed masked provisional content, then checked final history publication.
A separate cancellation closed its connection and discarded late progress.
The snippet editor loopback fixture verified the reviewed masked payload,
explicit Apply and Undo, and rejection of a changed original model on rerun.
Agent activity tests covered all seven render formats, the exact terminal
stream and private-mode gates, and canceled or changed-destination
publication.

### External SFTP editing, journal question previews, and scheduled proposals

On 2026-11-09, Windows ARM64 passed formatting, workspace Clippy with
`-D warnings`, and 2,400 tests with 26 ignored across 63 targets. Fedora 44
ARM64 passed the same checks and 2,486 tests with 89 ignored across 63 targets.
The Fedora source manifest SHA-256 was
`4b548559914a6205954195b997090c48d1e2bd1f97779e5ea8ce1e2b550dca41`.
The macOS ARM64 workspace and all-targets cross-check passed.

SFTP external editing opens a bounded private copy through a reviewed native
executable and literal arguments. The original SFTP channel can publish a
stable save after checking the opened file and its protected ancestry. The
Fedora OpenSSH fixture checked a successful guarded replacement, rejected a
group-writable parent, and detected changed remote bytes with the original
size and timestamp. Native external-editor launch and writeback together
remain an acceptance case.

Journal question previews show only complete quotations from the approved,
masked excerpts. Before sending, the worker verifies that the original
capture rows still match the review. The complete parsed answer enables exact
source navigation. The Agent design catalog supports independent activity,
status, approval, and window choices. Connection overrides resolve against
the saved catalog when a run starts.

The Fedora OpenSSH scheduler fixture reviewed a loopback model request and
one command per saved SSH target. It verified ordered execution in the
reviewed working directory, blocked changed masking before SSH, and rejected
replay of the same approval. The fixed diagnostics fixture also passed.

### Windows coding agent notification activation

The Windows ARM64 production desktop executable restored a private two-pane
tab and detected an owned Gemini screen replay in the inactive second pane.
After a foreground switch, the fixture reached the blocked authentication
state at `20:28:59.834Z` on 2026-11-09. The read-only Control API still showed
the first pane selected. At `20:29:04.664Z`, Win32 foreground observation and
the Control API showed the original kortty window focused with the second
pane selected. The terminal stream, shell process, and Gemini instance were
the same throughout. The test used the production notification path and a
manual click. The test programs and private profile were removed.

The executable SHA-256 was
`ed75cfa1cec8936bc3ec7a9b9a132cd468c38c5cb429eb7b9e9d647cef911202`.

### Privileged SFTP staging, remote project tabs, and live snippet application

On 2026-11-09, Windows ARM64 passed formatting, workspace Clippy with
`-D warnings`, and 2,412 tests with 27 ignored across 63 targets. Fedora 44
ARM64 passed the same checks and 2,498 tests with 91 ignored across 63 targets.
The final Fedora source manifest SHA-256 was
`943cf6f718c794d079bfdf25ffa7571458e5960f6945615e5b510df64260eec8`.
The macOS ARM64 workspace and all-targets cross-check passed.

The privileged edit uses a dedicated exec channel on the SFTP worker's
original authenticated SSH connection. The remote helper verifies Linux root,
the reviewed path and its ancestry, and the exact file revision. A save uses
a private stage and requires an explicit commit. The Windows SSH crate tests
cover continuation of the guarded sensitive line into framed data. A Fedora
test ran the fixed helper as namespace root in a private chroot. It verified
successful replacement, permissions and user xattr preservation, rejection of
symbolic and hard links and writable ancestors, stale content with the same
size and timestamp, and cleanup after EOF before commit. Host sudo on the
test account requires a password. Live sudo authentication and an external
editor click-through remain separate acceptance cases.

Rust restored the SFTP manager, remote editor and remote image in their saved
order and selection with an explicit route review before authentication. The
pinned Java `ProjectManager` at `1ebbe724ee84d55f31295ec3060eecad3653d2f2`
loaded the Rust export on Microsoft OpenJDK 21.0.12 with cached JAXB 4.0.9
and JavaFX 21.0.12. The verifier checked SFTP folders and idle timeout,
independent connection IDs, remote paths, tab selection, and image zoom.
Its isolated build used a failing stub for unused terminal history operations.

The single-snippet and folder apply fixtures stream progress and display
complete masked reasons before final validation. The review remains bound to
the original AI configuration and keeps validated recovery checkpoints on
cancellation. Highlight regressions exercise stream and rule revocation
between approval and delivery and preserve styling after an action-only hit.

### Isolated privileged file editing, journal summaries and network highlights

The pinned Java `HighlightBuiltinSets` at
`1ebbe724ee84d55f31295ec3060eecad3653d2f2` ran on Windows with Microsoft
OpenJDK 21.0.12 and the cached JAXB API. All four network expressions matched
the expressions stored for Rust interchange. The Java probe passed 17 match
cases, including a scoped IPv6 address with a zone longer than 32 characters.
The native Rust editor also checks each rule and paints an ordered multiline
sample with the active pane palette.

The opt-in `isolated_worker_privileged_file_round_trip_on_attached_and_standalone_routes`
test passed on Fedora ARM64. It used one pinned loopback OpenSSH authentication
per route, two in total. A private mount and user namespace provided a
synthetic sudo prompt and the real fixed Python file helper. The test verified
passwordless and password-backed reads, a UTF-8 write, rejection of a wrong
password and stale file revision, restoration of the fixture, and continued
use of the original shell or SFTP channel. Real sudo authorization and native
external editor writeback remain separate acceptance cases.

Local streaming HTTP fixtures exercised the journal manager and its automatic
capture scheduling. A manual pass displayed only verified keywords from
masked capture evidence. Automatic passes displayed stage and byte count.
Cancellation closed the live response without publishing its preview, even
when saving the cancellation failed. A configuration change paused the other
receipts in the manual batch. An automatic pass kept its durable receipt on
policy revocation.

The Windows ARM64 workspace passed 2,423 tests and Fedora ARM64 passed 2,509.
Windows and Fedora formatting and workspace Clippy passed, as did the
`aarch64-apple-darwin` cross-check. The Fedora suite used `PROMPT_COMMAND=:`
in its private process environment so the Fedora startup script did not
overwrite the live PTY test title.

### Scheduled privileged transfers, SFTP browsing and reviewed HTTPS analyses

The pinned Java `JobSchedulerJobRunner` uses separate privileged staging for
SFTP uploads and downloads. The Rust scheduler binds a recurring approval to
the operation, paths, sudo mode, saved route and all verified host keys. The
fixed Linux helper streams binary files through that connection. It verifies
the digest and publishes uploads from a private root-owned stage with atomic
no-replace semantics. Local downloads also publish without replacement.
Portable backups retain the job definition and remove the device approval.
Reviewed sync and replacement still require implementation. A live authorized
sudo account remains an acceptance gate.

The SFTP panes use exact-path breadcrumbs. A folder-size scan uses two
bounded metadata passes and reports logical file bytes. The Java folder-size
action reports allocated blocks from `du`. Rust can measure through the SFTP
subsystem without a remote command. The worker's deadline interrupts a
stalled scan on its original channel.

Single-snippet and folder analyses, reruns, and verification can review a
trusted HTTPS profile and its exact masked prompts. Local profiles remain the
default. The request revalidates the source, selected profile and policy at
dispatch. The Anthropic structured system instruction appears once in both
the review and the transmitted payload. Live approved provider acceptance is
tracked separately.

The ARM64 workspace passed 2,431 tests on Windows and 2,517 on Fedora.
Formatting, workspace Clippy, and the `aarch64-apple-darwin` cross-check
passed. Focused tests cover a failed job save, stale approvals, portable
backup, exact breadcrumb paths, scan cancellation and timeout, and HTTPS
request masking and invalidation before dispatch. The Fedora namespace
verified the real helper's binary framing, commit token, and atomic conflict
handling. The private OpenSSH fixture transferred 196,610 bytes and measured
mode `0600`, one link, and six SSH authentications across the scheduler and
isolated editor probes. It used synthetic sudo and preserved existing local
and remote destinations.

### Parallel SFTP, serial selection, snippet plans and external GGUF sets

The SFTP workbench runs siblings from a selection on separately cancellable
channels of the same verified SSH connection. Batches and potentially
colliding names stay ordered. A lower cap lets active lanes drain before
starting more work. A cap of one uses the browser channel for transfers.
During parallel transfers the browser channel stays available for listing. Conflicts
use a single dialog with batch-scoped answers. The Settings page persists the
parallel limit and an optional idle timeout. Active transfers, queued work,
dialogs and remote editors protect the connection from idle close.

The connection manager can select serial and SSH profiles together for
tagging, favorites, export and deletion. Opening a serial profile is a
single-row action. The existing import fixtures cover serial profile settings
from Java, MobaXterm and PuTTY. Native device acceptance remains deferred.

The optional initial snippet or folder analysis reviews the plan prompts
before dispatch. The saved report anchors a separate plan request, followed by
the optional flowchart. The loopback tests exercise durable save ordering,
exact prompt binding and preference revocation before dispatch. The default
keeps this extra request off.

AI Manager can reference a complete external split GGUF set from any shard.
Registration checks the group and runtime preparation leases every component.
The tests exercise changed identities, protected Unix paths and reference
removal with all source files intact. Native signed-runtime launch and the
Apple-Silicon MLX adapter remain open.

The ARM64 workspace passed 2,444 tests on Windows and 2,531 on Fedora.
Formatting, workspace Clippy and the `aarch64-apple-darwin` cross-check
passed. The private Fedora OpenSSH fixture opened nine auxiliary SFTP channels
on one terminal connection and eight alongside a standalone SFTP browser.
In both cases it cancelled a channel, used every remaining channel, opened
a replacement and verified the original shell or browser. The full fixture
recorded eight public-key authentications across this probe and the existing
privileged editor and scheduler checks.

The tested Fedora source manifest SHA-256 was
`2e27b1bb718dd19e73f8aa0f855148e9a5a61ac84fbdc49e1a117c203e12402c`.

### Manual navigation, privileged tree upload and managed MLX

The bundled reader uses validated chapter and section anchors. Internal links
and answer citations resolve to known bundled locations. Citations preserve the
exact supplied excerpt range, including within long Unicode paragraphs. Settings
Help resolves every settings section. The connection and SFTP figures use
intrinsic dimensions at all zoom levels. Native desktop click, keyboard and
layout acceptance remains separate from the Iced widget tests.

The scheduler's privileged tree action uses a one-use review for a complete
local tree of up to 128 entries and 1 GiB. The approval binds the file hashes,
exclusions, absent target, destination parent, sudo mode and verified SSH pins.
The Linux helper stages root-owned directories and files under the held parent.
It publishes the tree with `renameat2(RENAME_NOREPLACE)` and reports an uncertain
outcome if the reply after commit is lost. This route depends on a Linux
filesystem that supports the no-replace operation and on the configured sudo
mode. Reviewed tree downloads and existing-target publication remain open.

The Apple Silicon MLX route verifies a local signed index and ZIP, stores
activation and withdrawals separately from llama.cpp, and imports a private
data-only safetensors model directory. It uses the shared process supervisor,
proves ownership of the loopback listener and accepted sockets, then discards
the temporary key file after authenticated readiness. The request omits the
wire model selector so the sidecar uses the reviewed local model. Native
acceptance requires Apple Silicon, macOS 14 or later and the package's stated
minimum OS, an owner-signed runtime ZIP with its exact index and signature, and
a compatible local model. The [`mlx-lm` v0.32.2 server contract](https://github.com/ml-explore/mlx-lm/blob/v0.32.2/mlx_lm/server.py)
defines the readiness and model endpoints.

The ARM64 workspace passed 2,461 tests on Windows and 2,548 on Fedora.
Formatting, workspace Clippy and the `aarch64-apple-darwin` cross-check passed.
The native Iced widget test mounted the reader at 600 × 375 and ran the real
scroll operations at 70% and 250% zoom. Both a section and an exact answer
excerpt moved into view. MLX fixtures checked catalog withdrawal and replay
rejection, activation rollback, private key removal, rejected custom code,
file identity checks and the request payload.

The Fedora helper fixture published a complete tree and preserved a target
created after staging. The isolated OpenSSH scheduler test reviewed and saved
the action, reopened the schedule, and published 196,610 bytes with nested
directories and an empty directory. It verified one authentication for review,
one for execution and none for replay. The test used the real Linux helper and
synthetic sudo. The tested Fedora source manifest SHA-256 was
`7892ee9241db5088848dad3aa18cad294742197e15bb5032a29040ad75f5aae2`.

### SSH RSA, PDF citations, privileged downloads and journal estimates

On 2026-11-09–10, Windows ARM64 and Fedora 44 ARM64 passed formatting,
workspace Clippy with `-D warnings`, and the all-targets workspace suite.
Windows passed 2,473 tests with 30 ignored. Fedora passed 2,560 with 101
ignored. Both counts span 63 targets. The macOS ARM64 workspace and all-targets
cross-check passed. The Fedora workspace and integration run used source
manifest SHA-256
`fd827c95580ddc677cf235025e50568e2ad1af6c2476b02a924550babe601d74`.

SSH now negotiates RSA/SHA-256 and RSA/SHA-512 with the patched RustCrypto
RSA 0.10.0 provider. Its AArch64 and x86-64 `cmov` selection primitives are
the approved inline-assembly exception in [HANDOFF.md](HANDOFF.md).
The active normal/build SSH graphs were inspected for both architectures on
Windows, Linux and macOS. The real isolated Fedora OpenSSH daemon passed all
12 cases in each of the classic and hybrid exchange modes. The RSA fixture
forced each signature algorithm for host and user authentication, exercised
OpenSSH and supported PKCS#1 and PKCS#8 keys, and rejected an SHA-1-only host.
Six RSA-focused Sunset unit tests also passed natively on Windows ARM64 in a
private copy with the vendored OpenSSH reader enabled. They covered the
unsigned-probe fallback, wire bounds and shorter signature encoding.
The independent transport review remains a release gate.

A follow-up on Fedora 44 ARM64 exercised RSA/SHA-256 and RSA/SHA-512
through two jump hosts and a target using distinct credentials. Both classic
and hybrid key exchange passed. A mismatched RSA pin at the middle hop
stopped that hop before user authentication. The test then checked the
negotiated host signature and accepted user-key fingerprint at every hop.
A separate process worker authenticated with an encrypted RSA OpenSSH key,
completed a byte-exact internal-SFTP round trip and was reaped after close.
The new process-worker case used RSA/SHA-512 for the host and RSA/SHA-256
for user authentication.

A fresh offline locked dependency check confirmed `rsa` 0.10.0,
`ctutils` 0.4.8 and `cmov` 0.5.4 in the SSH normal and build graphs for the
six Windows, macOS and Linux ARM64 and x64 targets. The SSH graph resolves
only the corrected RSA version. The application also resolves the older
RSA version for its separate OpenPGP workflow.

The native Windows ARM64 development build passed the existing LPAC relay
smoke test against two disposable Fedora OpenSSH 10.2p1 listeners. The test
used a temporary RSA private key, verified PTY output and exit status, and
downloaded a byte-exact SFTP sample. It then used the first listener to jump
to the second. OpenSSH logs confirmed RSA/SHA-512 host signatures and
RSA/SHA-256 user signatures at the entry, with the pair reversed at the
target. With an intentionally wrong entry pin, the server recorded zero
authentication requests. Both listeners were loopback-only and the Windows
test reached the first via a separate pinned OpenSSH forward.

The Windows development desktop, sandbox worker and test executable had
SHA-256 values `8998174588d15ed8942997a22e15efcb40b40f536d4f56c3771f826151c3b8bf`,
`969d9ba191daefa39a488d4aded5ca56c5855d78de31b6cf832863345ab4a1ae`, and
`05ec83212f0cf67e250bb214bb737d8471abfb46f5764a1cee9733076714a77c`,
respectively. The private fixtures were removed. Both retained Windows ZIPs
predate the RSA change. Current release-package verification, native x64
Windows and macOS RSA worker acceptance remain open.

The native PDF worker passed extraction and cancellation tests on Windows
ARM64 and Fedora ARM64. The knowledge workflow preserves physical page
numbers across blank pages and index persistence. It rejects stale files and
requires explicit folder approval for PDFs. Native macOS process acceptance
remains open.

The scheduler's privileged folder download binds review to the pinned SSH
route, exact source manifest and absent local destination. On Fedora, the fixed
helpers passed four protocol tests as namespace root. Separate scheduler
upload and download tests used real OpenSSH and a test sudo executable in a
private chroot. Review and execution made four successful authentications in
total. Replaying the consumed approvals made none. The workspace tests cover
late destinations, cancellation, denied publication and cleanup at the
maximum accepted tree depth. Native end-to-end Windows and macOS publication
and authentication with an authorized real sudo account remain open.

On 2026-11-10, Windows ARM64 verified guarded retries for privileged local
directory publication. The native no-replace operation retries Win32 access
denied and sharing violation after 10, 25, 50, 100 and 200 ms. Every attempt
checks the original parent and named staging directory, the protected ACL,
the absent destination and current execution authority. Ambiguous publication
stops immediately. The eight focused tests passed, including injected
retryable errors, revoked authority, a late destination, the retry bound and
an ambiguous rename. The `kortty-files` staging ACL test also passed.

Fedora 44 ARM64 passed six focused tests with four opt-in cases ignored in
the ordinary run. The private rootless chroot returned the expected framed
manifest through a pinned OpenSSH command. The two scheduler OpenSSH cases
still returned `The privileged folder, sudo authorization, or no-replace
publisher was rejected` during preview, before publication, in the rebuilt
fixture. A subsequent run with the fixture below passed both cases.
Formatting and workspace Clippy with `-D warnings` passed on both platforms.
The source manifest SHA-256 was
`3b533ee1dd48fc2f51ff3e97aae93dfefbbe85265fb25fb48776129b626ca6eb`.
The private native profile, loopback listeners, Fedora chroot and development
artifacts were removed. Windows disk usage was 131 MiB for the checkout and
42 MiB for `target`. Both retained Windows package checksums matched.

The Fedora 44 ARM64 retest used an unprivileged loopback OpenSSH daemon
with ephemeral host and user keys. A private user and mount namespace
provided the chroot's system tools, loader paths and synthetic sudo
executable. The fixture answered only the expected password challenge.
A pinned CLI verified both the absent upload target and the download
manifest before the scheduler tests ran. Both scheduler tests then passed.
Their four successful authentications each opened SFTP and an exec channel
on the same pinned route. Consumed approvals made zero new connections.
The tests verified exact bytes, nested and empty directories, excluded
entries, and private destination permissions. Real sudo and native
end-to-end Windows and macOS publication remain open.

The scheduler estimates journal summary usage from complete reported counts
on kept runs for the same job or from labeled assumptions. Its schedule
projection uses the next seven local calendar days, including daylight-saving
changes. Token checkpoints preserve partial usage and avoid double counting
after cancellation and continuation. A loopback provider fixture and the
schedule and pricing regressions passed on Windows and Fedora. The native
scheduler estimate dialog remains an acceptance case.

### Vault webhook tests

The unlocked Vault can send a sample to a saved webhook or to a new URL
entered in its editor. Editing a saved destination can test the draft format
and summary setting with the original URL. A draft edit revokes the previous
test and invalidates its response. The fixed sample uses the selected JSON,
Slack or Teams format and contains synthetic job metadata and an optional
synthetic summary. The feature policy is checked before credential access.
Both test and scheduler workers recheck the installed feature and host
rules before each HTTPS request and reject redirects.

On 2026-11-10, Windows ARM64 passed nine focused tests and workspace
all-targets Clippy with `-D warnings`. Tests covered saved and draft
selection, immutable saved URLs, stale draft actions, modal completion,
payload formats, retry policy revocation and redirect rejection. The HTTP
retry fixture was isolated to loopback. Fedora ARM64 passed the same nine
tests. An added private test exercised the actual Vault delivery path
with native TLS and a loopback CA trusted only by the test process. The
receiver accepted two requests, a Teams card from a saved entry and a
Slack message from a new draft, both with the expected synthetic summary.
Only the saved entry remained in the vault.

The Fedora source manifest before adding the private TLS test had SHA-256
`e31e5ed975ff4f88550234e9a3ab1fac630bfc18a99b34f616f81416e3a677c8`.
The default test link was killed with SIGKILL. The successful retry kept
the development profile and used `-C debuginfo=0` and GNU ld's
`--no-keep-memory` and `--reduce-memory-overheads` for the test binary.
Its SHA-256 was
`4ec71dff2fcdcba7e5f8ffb9be1e86919a4e9792b297b354534d175ca844ec5a`.
The unmodified product sources passed formatting and all-targets workspace
Clippy with `-D warnings` on Fedora. The private Fedora source, chroot, TLS
fixtures and development artifacts were removed. The Windows checkout and
`target` occupied 132 MiB and 42 MiB. Both retained Windows package
checksums matched.
Native click-through and delivery to a configured provider remain open.

### Local CLI, live scrollback, docked agent activity and server copy

On 2026-11-10, the Windows ARM64 all-targets workspace suite passed
2,489 tests with 30 ignored. Fedora 44 ARM64 passed 2,578 with 103 ignored.
Both counts span 63 targets. The final source passed formatting and
workspace Clippy with `-D warnings` on Windows and Fedora, and an all-targets
Clippy cross-check for macOS ARM64. The final Fedora source manifest SHA-256
was `0dbfd25f615f9629eebace8bd67df464fe3d66f31dae7911fb5aa66b20396d9c`.

The configured CLI path uses a reviewed native executable and literal
arguments. The manager test and Terminal Chat passed with a small native
fixture. The tests cover exact masked stdin, stale proof, cancellation and
bounded output. Fedora also verified that replacing the original filename
does not replace the held executable and that malformed files fail without
shell interpretation. Windows verifies the preassigned Job path. The
macOS stage preserves and rechecks the approved image bytes, extended
attributes and ACL. Real-provider interoperability, native macOS staging
and original-location-dependent executables remain acceptance cases.
Unix process-group cleanup covers the owned group. Deliberately detached
helpers require a separate ownership and lifetime mechanism.

The terminal and saved local and SSH profiles now support line and
memory limits and idle compression with field-by-field inheritance.
The checks cover Java XML round trips and legacy memory normalization,
live PTY updates without further output, immediate eviction, reflow, reconnect
and reset, and retained scrollback anchors and Kitty placements.

The agent design catalog adds six histories, nine status modules and
adaptive docking. Iced tests cover native widget placement, removal from
stale streams, reuse of the timeline in wide panes, and ordered restore
and focus actions for an owned activity window. Native desktop interaction
and the remaining Java modules listed under A2 stay open.

The real Fedora OpenSSH SFTP server passed the browser-copy regression.
The fixture drove the normal review and approval messages, verified the
exact binary result, inserted a competing target at the publication request
and verified its unchanged contents, and checked cancellation, changed
source metadata and private-stage cleanup. This action covers one regular
file on a route advertising copy-data and hard-link publication.

The live SSH diagnostics regression used two real loopback SSH terminals.
It checked that the visible terminal appears, the private and provisional
terminals are filtered, and the closed transport drops out even with an
older cached running frame. Native process measurement also passed on both
hosts. Detached and hosted dialog click-through remain acceptance cases.

### Pinned SithTermFX run on Windows

On 2026-11-08, the pinned SithTermFX checkout passed 313 core and 49 UI Maven
tests on Windows 11 ARM64 using an emulated x64 Microsoft OpenJDK 17.0.21.0.2
and Maven 3.9.16. The source copy and Maven repository were disposable.
Run `mvn -B -ntp -Dproject.build.sourceEncoding=UTF-8 -pl sithtermfx-core,sithtermfx-ui -am test`
with `-Dfile.encoding=UTF-8` in `JAVA_TOOL_OPTIONS`. The first run with Windows'
default Cp1252 encoding failed two double-width text tests. Both passed with
UTF-8 at the same commit. This run covers the upstream suites. Native Java UI
acceptance, Java application smoke tasks, and Rust parity comparisons have their
own checks below.

The supplemental [PETSCII fixture](crates/terminal/tests/fixtures/petscii/README.md)
passed against the same pinned core with JDK 17 and Maven 3.9.16. The Windows
ARM64 Rust run matched the thirteen streams and all 17 mapped keys. Rust fed the
streams whole, in seven-byte chunks, and one byte at a time.

The refreshed [HP captures](crates/terminal/tests/fixtures/hp/README.md)
passed the same pinned Java replay on Windows with JDK 17 and Maven 3.9.16.
Both Rust mode streams match the captures on Windows ARM64.

The shared [TVI tab replay](crates/terminal/tests/fixtures/tvi/README.md)
was generated and replayed on Windows ARM64 with native Microsoft OpenJDK
17.0.21.0.2 and Maven 3.9.16. All three Java profiles matched the capture.
The seven focused Rust TVI tests also passed, including whole and fragmented
input for the shared tab sequence.

The [DEC tab replay](crates/terminal/tests/fixtures/sithtermfx/README.md)
was captured and replayed for all six VT profiles against the same pinned
core with JDK 17 and Maven 3.9.16. The Windows ARM64 Rust run passed all 53
focused terminal, TVI and reference replay tests.

The [Wyse fixture](crates/terminal/tests/fixtures/wyse/README.md) also passed
against the pinned core on Windows with JDK 17 and Maven 3.9.16. The two
Rust replay tests passed on ARM64. They cover 16 streams and three key
tables and preserve the two documented screen differences.

### Java desktop reference on Windows

On 2026-11-08, the pinned Java application passed all six desktop smoke
tasks on Windows 11 ARM64 with emulated x64 Temurin 25.0.4.1+1, JavaFX
21.0.12-win, and Gradle 9.8.2. The terminal link focus assertion ran.
The pinned SithTermFX dependency was installed into the disposable Maven
repository with JDK 17 and UTF-8 before invoking Gradle.

```powershell
./gradlew.bat --no-daemon --console=plain --continue --max-workers=2 -x cloneSithtermfx `
    terminalNavigationKeysSmoke terminalLinksSmoke terminalSplitTransparencySmoke `
    multiExecSmoke mermaidRendererSmoke snippetDiagramZoomSmoke
```

Use a private Gradle user home, Maven repository, Java user home, and
`npm_config_cache` for repeat runs. This run used the existing shared npm
content cache for the asset build. The run-owned npm logs and private
source and build directories were removed. These are results for the
reference app. The equivalent native Rust desktop cases are tracked below.

### Cursor approvals, floating agent history, CLI prompt files and SFTP folder copies

A2 adds the pinned Java d14 approval placement and d19 arrangement.
The cursor view fits the exact command and reviewed directory in the active
terminal. Enter or 1 approves the bound review once. Esc or 2 cancels the run.
Repeated approval keys are consumed across view transitions. Tab ends the
run and sends the exact eligible single line as bracketed paste to the
original terminal. The stream actor checks the current prompt, input and output
revisions, foreground shell, working directory and SSH connection identity as
applicable. The final writer checks output and cancellation again. The
Activity window handles sudo, limited display space, and terminal designs,
effects or graphics that can cover the command. Windows takeover requires a
compatible shell to report the current directory alongside the OSC 133 prompt.

The d19 panel adopts the selected history with independent status placement.
It supports corner drag and snap, fold, hide and restore, same-stream run
tabs, and a detached window with independent geometry. Its close action
reattaches the history. This implementation snaps immediately and uses a
decorated native detached window. Native click-through and the Java spring
animation remain acceptance and parity work. The pinned Java host releases
its explicit run selection on a new run or input prompt. Matching that
selection lifecycle requires coordinating the detached view with the pane.

B1 adds one combined prompt file or separate system and user files through
standalone argument placeholders. The review binds the expanded absolute
arguments and exact masked contents. CLI runs create owner-private files
after approval, recheck the staged bytes, and retain directory protection
until cleanup. Windows holds ancestor rename protection through process
exit. Unix checks the owner-controlled path during creation and before launch.
The manager offers the explicit-model LM Studio stdin preset. Provider
interoperability with real installed executables remains an external gate.

T3 adds a recursive SFTP-only browser copy for 256 descendants, 32 levels and
1 GiB of regular file data. The review shows the exact bounded manifest. The
copy rechecks the protected canonical ancestry and source metadata, claims a
new destination root with exclusive creation, and publishes each staged file
using the OpenSSH hard-link extension. The result reports confirmed entries
and the affected path after interruption or a lost reply. Published directory
entries remain in place after a failure. SFTP v3 lacks stable directory
identifiers, so the binding uses canonical paths, owner and permission
checks, metadata and directory inventories. An actor with the same server
identity could exchange indistinguishable objects on that route.

Windows ARM64 passed 2,503 workspace tests with 30 ignored across 63
targets. Fedora 44 ARM64 passed 2,594 with 104 ignored across 63 targets.
Formatting and workspace Clippy with all targets and `-D warnings` passed
on both hosts. The macOS ARM64 all-targets Clippy cross-check passed. The
Fedora source manifest SHA-256 before adding this record is
`5ee2d6bfddfc43a78442c1039a634bef94bd16e516911f6a83eaf239eb0cc77c`.

The Fedora PTY fixture checked the exact bracketed-paste payload, the
absence of a submission byte, and rejection of stale input, a revoked
request and the wrong working directory. The workspace event fixture
checked stale epochs and streams. On Windows the CLI fixture held a
native child while checking the private prompt files and stage cleanup.
The directory-lock regression also confirmed rename denial for the stage
and its parent, then successful cleanup after releasing the handles.

OpenSSH 10.2p1 on Fedora passed the opt-in browser file and folder copy
fixtures and the live SSH diagnostics fixture. The folder test checked
nested and empty directories, a changed manifest, cancellation, and
destination creation immediately before the exclusive directory and
hard-link operations.

The Fedora source copy and development artifacts were removed after
verification. The Windows and macOS cross-development builds were cleaned.
The Windows checkout occupied 117 MiB, with 42 MiB under `target`. The
retained Windows ARM64 and x64 archives matched their SHA-256 sidecars.

### In-terminal agent history, SSH keepalive and loaded LM Studio models

A2 adds d03 activity blocks and the d17 focus trace as native terminal
presentation. The session actor keeps up to 16 typed, masked blocks. Each
block follows an annotation on a primary terminal row through scrollback
packing and reflow. A finished block leaves its answer or summary at the
original anchor. Evicting or clearing the terminal line expires the retained
block. The scroll thumb, timestamp gutter, Quick Select, link hit tests and
cursor use the same projection. The authoritative terminal grid, protocol
responses, raw search and recordings continue to use the original cells.

The live block offers details and cancellation with its focused keyboard
shortcuts. Approving by number requires the exact single-line command and
destination in a complete visible review. The review binds the terminal
stream, input revision and run epoch. Sudo continues through its owned
review. The focus trace dims the surrounding rows, retains the readable
prompt and supports reduced motion. The design settings use a separate,
replayable in-memory preview with simulated approvals.

The raw viewport is used for alternate screens, mouse-reporting programs
and viewports with Kitty graphics. Annotation selection, copying and transfer
to reconnect history still need parity work. Native desktop keyboard and
rendering acceptance remains open across the supported window systems.

D2 adds the Java-style SSH keepalive setting with an enabled global default
of 60 seconds and a 5–600 second saved connection override. Disabled profiles
retain the interval. Import and export handle the Java transport settings
independently of terminal appearance inheritance. On connect or reconnect,
the SSH actor sends the encrypted idle heartbeat on the direct and isolated
routes. It uses the existing event loop and leaves key exchange and global
request replies available for connection work. The connected TCP socket uses
the platform keepalive flag where available. Java's separate dead-link probe
remains open and requires routing its replies independently from remote
forwarding.

B1 adds the LM Studio stdin preset that selects an already loaded model at
execution time. Switching from an explicit model clears the stored model ID
and invalidates the previous executable approval when the input changes.
The fixture verifies the actual argument vector and reviewed UTF-8 stdin.
Interoperability with an installed LM Studio remains a separate acceptance
item. A Codex preset requires a verifiable configured destination and tool
scope. The MiniMax CLI requires a supported native entry point and its JSON
message input.

Windows ARM64 passed 2,528 workspace tests with 31 ignored across 63
targets. The native renderer and event fixtures verified row painting,
focus contrast, Quick Select mapping and approval rejection on a clipped
viewport. The session tests verified idle PTY delivery, scrollback reflow,
retention and expiration, and unchanged raw search and cursor responses.
Workspace formatting and all-targets Clippy with `-D warnings` passed on
Windows. The macOS ARM64 all-targets Clippy cross-check passed.

Fedora 44 ARM64 passed 2,619 workspace tests with 106 ignored across 63
targets. Formatting and all-targets Clippy with `-D warnings` passed. The
Fedora source manifest SHA-256 before adding the verification record is
`187ac5c5c849f023551f1fef55d0fcb835de0fe31a0368e6044f1b1a2921f122`.
The direct OpenSSH 10.2p1 fixture verified the presence of encrypted
`SSH_MSG_IGNORE` when enabled and its absence when disabled. The shell
exchanged data before and after the idle interval and the attached SFTP
channel remained usable.
The matching application binary also passed the isolated-worker fixture.
Its private loopback daemon recorded one public-key authentication, an
encrypted `SSH_MSG_IGNORE` packet and the SFTP subsystem request. The worker
test read shell output on both sides of the idle interval and issued an
SFTP stat on the same connection.

The Fedora source copy and its development artifacts were removed. The
Windows and macOS cross-development builds were cleaned. The Windows
checkout occupied 118 MiB, with 42 MiB under `target`. The retained
Windows ARM64 and x64 archives matched their SHA-256 sidecars.

### Agent projection, floating history and SSH liveness

A2 maps Kitty image layers into d03 and d17 using the raw terminal rows
covered by each placement. The renderer clips translated image bands where
agent rows interrupt those placements. The alternate screen and terminal
mouse reporting retain raw coordinates and use the raw viewport.

Projected clipboard ranges bind to the displayed frame and pane stream.
They can span terminal cells and typed agent rows, preserve wide and
combining characters, and skip concealed cells. They use the existing
clipboard policy. A transition into the projection invalidates an earlier raw
selection. Inline approval text masks the destination and command, and the
numbered approval requires both to remain exact after masking. The Activity
window handles reviews that need the original text. Search, recordings and
raw terminal context retain their own ranges.

Reconnect archives only finalized agent records with an exact surviving
terminal-line anchor. Reflow and scrollback eviction move or discard those
anchors with their physical rows. The transfer omits prompts, running steps
and focus animations.

The d19 floating history follows the oldest run waiting for input, then the
newest working, unfinished or completed run in that order. A manual selection
lasts until an accepted start or a fresh input request. A detached history
keeps its native window while following a different run of the same stream.
Actions from its previous owner are rejected. Corner snaps use the Java
420 ms spring when animations are enabled. Moving the overlay keeps the
terminal geometry fixed.

D2 adds a separate `keepalive@kortty.de` liveness request for SSH terminals
and standalone SFTP. The detector arms on a matching success or failure.
Inbound bytes reset the silence count. Two consecutive three-second silent
windows close an armed connection with a timeout. The unanswered request
keeps its global reply slot because a late reply has no request identifier.
An unresponsive peer stays connected before the detector arms. A pending
reply can delay or prevent remote forwarding requests. The configurable
encrypted idle heartbeat has its own schedule.

The Windows ARM64 workspace passed 2,548 tests with 31 ignored across 63
targets. The Fedora 44 ARM64 workspace passed 2,639 tests with 108 ignored
across 63 targets. Formatting and all-targets Clippy with `-D warnings`
passed on both platforms. The macOS ARM64 all-targets Clippy cross-check
passed. The fixture now waits for visible ConPTY output
and verifies the original physical row before transferring the record.
The final Fedora source manifest before adding the verification record is
`694ad397238592aa950de83e5068e9d3e1e177dc25972867daca882e27ab316b`.

On Fedora, OpenSSH verified both the encrypted idle heartbeat and the new
timeout on the direct and isolated-worker paths. The direct relay held a
failure reply beyond its response window and released it after a guarded
forwarding request. A subsequent bind and attached SFTP operation succeeded.
Dropping packets in both directions while holding both TCP sockets open
produced the expected timeout. These four runs used the same production code
before the reconnect fixture adjustment, with source manifest
`937ec628ac7d4c01627e1d69aaf69b1dd7f35a1d945fb3c1cd618bec120b92ff`.
Separate OpenSSH cases with the final manifest verified RSA/SHA-2 private-key
authentication and remote listener bind, half-close and cancellation.

The Fedora verification tree was removed after the development cleanup.
The Windows and macOS cross-development artifacts were removed. The Windows
checkout occupied 119 MiB, with 42 MiB under `target`. Both retained Windows
archives matched their SHA-256 sidecars.

Native desktop acceptance of Agent selection, painting, snap and detached
window behavior remains open.

### Reported Agent model usage

A2 now carries per-attempt usage from decision and planning requests through
the same single-repair limit and shared deadline. The model worker captures
each completion's usage before a repair can replace it. A supplied total
takes precedence. When both split counts are known, their checked sum can
supply the total. Missing counts remain unavailable and a reported zero is
retained. Invalid OpenAI-compatible payloads with a valid usage envelope
preserve their reported metadata. Native LM Studio and Anthropic responses
classify a cancelled body read as cancellation.

The current run and epoch, terminal route and approved profile must still
match when a decision or planning report is accepted. Accepted failures
and cancellations retain their observed usage. Activity and in-terminal
history show each attempt's outcome, duration and counts. Status displays
the reported total or a labelled subtotal. Each field has independent
reporting coverage. The bounded presentation history is separate from the
command evidence used in the next model request. Private runs use only the
ephemeral presentation state for this display.

The activity archive snapshots lifetime reporting coverage and the bounded
attempt rows for nonprivate runs. The seven export formats include run
totals and coverage. YAML, XML and JSON carry typed attempt counts and
outcomes alongside the existing activity strings. Markdown, plain text and
Asciidoctor include readable attempt details. PDF uses the run summary.

Windows ARM64 passed 2,562 workspace tests with 31 ignored across 63
targets. Fedora 44 ARM64 passed 2,653 workspace tests with 108 ignored
across 63 targets. Formatting and all-targets Clippy with `-D warnings`
passed on both. The macOS ARM64 all-targets Clippy cross-check also passed.
Loopback fixtures verified the decision and planning repair
requests, missing and explicit totals, malformed provider responses and a
cancelled LM Studio body read. Widget tests verified the status, history
and compact pane layout. The export fixtures verified the seven formats,
including extracted PDF text and lifetime coverage after attempt eviction.
The Fedora source manifest before adding this verification record is
`a6c016e8de078313e0c1283e6957a9e6853d5ab1cff03e08a534247c2a898b37`.

The Fedora verification tree and the Windows and macOS cross-development
artifacts were removed. The Windows checkout occupied 120 MiB, including
42 MiB under `target`. Both retained Windows archives matched their SHA-256
sidecars.

### Live Agent response repair

The Agent worker emits a typed progress event before its second model call
when the initial reply needs JSON or schema repair. Activity and in-terminal
history show the initial attempt and a distinct running repair row. Visible
status modules show the repair, and the Swarm target shows the same status on
its own row. The repair row keeps its identity through completion or
cancellation. The initial attempt's provider-reported counts are accepted
immediately and deduplicated when the final report arrives. The running row
adds no counts. The repair uses the same reviewed context and shared deadline.

The transition requires the current Agent, epoch, terminal source, profile
and policy. It grants no command approval and adds no model input. A cancelled
or stale worker cannot add a new running row. If final metadata is unavailable,
the row settles without synthetic timing or token values. Planning keeps its
completion-only attempt display, which matches the pinned Java planning path.
Native Windows, macOS and Wayland acceptance and checks with enabled providers
remain open.

The native Fedora X11 backend ran under Xwayland in a private Mutter 50.6
session with a 1600 × 1000 virtual display. XTEST opened a local shell and
the standalone Ask Agent dialog. The local provider held the second response
while the dialog showed **AI response repair · Running** and 12 reported
tokens. Releasing it completed the row, showed the answer and displayed
29 total tokens. The usage panel reserved space for its scrollbar, and a
scroll exposed the repair's own 17-token line. The provider received exactly
two reviewed loopback requests. The checked executable SHA-256 is
`160a6e4c33f4b39206a425b0110f839c33e26c3774a017750a054472e31d1d2e`.

Windows ARM64 passed 2,607 workspace tests with 31 ignored across 63 targets.
Fedora 44 ARM64 passed 2,697 with 108 ignored across 63 targets. Formatting and
all-targets Clippy with `-D warnings` passed on both. The macOS ARM64 all-targets
Clippy cross-check also passed. A held loopback response
verified the in-flight row, cancellation closing the owned socket, stable row
identity and exactly-once counts. Workspace regressions covered replacement
of the terminal, the profile and managed permission. Iced widget operations
exercised every history and visible status design. The interactive Swarm
fixture verified that only the exact target row showed the repair notice.
The Fedora source manifest for the final layout checks is
`fc7e6197507451ceccd03935d967a00ef34ce1f52c9570aa11fe68389270c933`.

The Windows Saved Chats store classifies a sharing violation during its lock
file's creation window as retryable contention. The deterministic test holds
the creator handle and verifies that a cooperating open fails as busy until
that handle closes. The concurrent Agent-chat seed passed 64 repetitions
after the fix. The mapping is scoped to Saved Chats so the AI usage store
can apply its own retry policy.

The Fedora verification directory and the Windows native and macOS
cross-development artifacts were removed. The Windows checkout occupied
121 MiB, including 42 MiB under `target`. Both retained Windows packages
matched their SHA-256 sidecars.

### Agent responses at the model token limit

Normal Agent and Ask requests, including interactive Swarm members, validate
the decision before consulting provider stop metadata. An invalid initial
decision at the limit stops after one request and records the reported usage.
A complete decision goes through its usual run or command review. An ordinary
invalid reply still gets one repair. If that repair reaches the limit, both
attempts and the final limit notice stay available. Planning Options and Report
use the reference's separate repair policy.

OpenAI-compatible `length` and Anthropic output and context stop reasons give
explicit metadata. Native LM Studio reports output counts, which are used to
identify a likely limit and are labeled accordingly in Activity and exports.
The output-limit notice shows the actual request cap and offers the profile
setting when the configured cap is below the Agent ceiling of 2,048 tokens.
The provider layer keeps this metadata within the individual Agent request.

In a private Mutter 50.6 session, the Fedora X11 backend displayed **Token
limit** and a stopped run after exactly one reviewed loopback request. The
dialog showed the effective limit of 2,048 output tokens, the reported total
of 2,057 and guidance to shorten the task or reduce terminal context. The
checked executable SHA-256 is
`58fd8909066c798f8c75118619bc303bcf1bc84f24ddd981ecb3d53d38eb3601`.
That desktop build preceded an internal enum rename for Clippy. The final
Fedora workspace suite uses the exact post-rename source manifest below.

Windows ARM64 passed 2,615 workspace tests with 31 ignored across 63 targets.
Fedora 44 ARM64 passed 2,705 with 108 ignored across 63 targets. Formatting
and all-targets Clippy with `-D warnings` passed on both. The macOS ARM64
all-targets Clippy cross-check passed from Windows. Loopback tests exercised
complete and incomplete decisions at the limit, a repair that reached its
own limit, and native LM Studio's inferred limit. Anthropic stop reasons
were verified with parser fixtures. The Fedora source manifest is
`b98806f0bb2207fbecef8570e7534886de4f9584b9ac3227faecf0e21bebe58b`.

The Fedora verification directory and the Windows native and macOS
cross-development artifacts were removed. The Windows checkout occupied
122 MiB, including 42 MiB under `target`. Both retained Windows packages
matched their SHA-256 sidecars.

### Agent final response after eight rounds

Java `TerminalAgentService.java:649,715–727,862–877,1301–1343,1870–1967,3768–3792`
allows a final advisory decision when the eight-round run has command history.
Rust standalone and interactive Swarm Agent runs can finalize when the last
reviewed command result arrives. Standalone Run presents a separate masked
model review before dispatch. A successful fixed probe is separate and cannot
trigger finalization by itself. Answered clarifications keep the same round
number and use the existing clarification bound.

The request contains the masked task and target, available context, and
bounded evidence for commands that returned a confirmed exit status. It
remasks earlier output with the current run mask before taking tails and
encoding JSON. Confirmed password-sudo results contribute the exit status
with output withheld. The latest command and its exit status have priority
when the profile's context budget is tight.

The final response can settle only as done or blocked, and a blocked response
can identify the missing information. An invalid reply gets one repair,
including an invalid reply at a provider limit. Repair reuses the reviewed
evidence and leaves out the rejected reply. The worker waits for the owning
run to authorize the resend after rechecking its epoch, target, policy,
profile and context budget. The run retains accepted attempts and provider
counts. If the final response cannot be used, Activity retains a masked
failure and the run stops with manual guidance.

The Fedora X11 backend ran in a private Mutter 50.6 Xwayland session at
1600 × 1000. XTEST approved the fixed probe and eight individual shell
commands in the Agent window. The loopback provider checked all eight
actual exit codes and stdout tails in the final request. It received eight
ordinary requests, one final request and one repair after a length stop.
The Activity window showed the final request, the live repair and run
completion with 2,085 reported total tokens. Scrolling the Activity views
exposed the limit and running repair rows and the final answer after the
eighth command's stdout. The checked executable
SHA-256 is `0c3db0bd633fb6dba33673f1274b8c67cf92971b0d3198cd028dbe7870bd09ad`.

Windows ARM64 passed 2,623 workspace tests with 31 ignored across 63 targets.
Fedora 44 ARM64 passed 2,713 with 108 ignored across 63 targets. Formatting
and all-targets Clippy with `-D warnings` passed on both. The macOS ARM64
all-targets Clippy cross-check passed from Windows. A held loopback repair
verified that replacing the terminal stream prevented the second request.
The Fedora source manifest is
`564c497a92fdd12b6bc291ea66a6d110a5ca9c69d36ec605b845af78f35e4d09`.

The isolated Fedora workspace, Windows screenshots and native and macOS
cross-development artifacts were removed. The Windows checkout occupied
122 MiB, including 42 MiB under `target`. Both retained Windows packages
matched their SHA-256 sidecars.

Interactive acceptance for the native Wayland, Windows and macOS backends
and configured provider sessions remains part of A1 and A2 verification.

### Optional setup for Agent aliases and single-attempt planning questions

Java `MainWindow.java:12042–12070,12109–12135,13239–13263` skips the Run
or Ask setup when the preference is disabled and the submitted alias
supplies a prompt. Rust's AI Manager saves the default-enabled option in
General. A direct alias uses the normal Start lifecycle on the same live
pane after the prompt check. A direct Ask masks the submitted question and
uses it without capturing terminal selection. Menu actions and Plan aliases
open setup. Each Run still requires the fixed probe review. A local Windows
Run has a dedicated absolute working-directory confirmation before probe
review, because ConPTY does not provide the verified current directory.

Java `TerminalAgentService.java:319–339,378–384,440–451,468–500,2501–2514`
parses Questions once and gives Options and Report one format repair. Rust
Questions now accepts the `questions` status and settles once on schema
failure, including an invalid reply marked as length-limited. The failed
attempt retains provider-reported usage. Options and Report keep their
repair behavior. This checkpoint preceded the provider compatibility and
Report schema changes below.

In a private Mutter 50.6 session, the Fedora X11 backend captured submitted
aliases at a Bash OSC 133 prompt. The first Ask showed setup and made no
request. XTEST cleared the General checkbox in AI Manager and saved it. The
next Ask displayed a direct answer after one question-only loopback request.
The Run alias displayed probe approval with the live Bash directory, and
the provider received the probe result only after the separate approval.
The Plan alias opened setup. Clicking Draft plan returned a length-marked
invalid Questions reply, stopped with 11 reported tokens, and made one
request for that stage. The checked executable SHA-256 is
`c3083e6b5ae31d2543be628977f2683f0770950669e5d10ecf0482e7e2cbe485`.

Windows ARM64 passed 2,628 workspace tests with 31 ignored across 63 targets.
Fedora 44 ARM64 passed 2,718 with 108 ignored across 63 targets. Formatting
and all-targets Clippy with `-D warnings` passed on both. The macOS ARM64
all-targets Clippy cross-check passed from Windows. The Windows workspace
test exercised the dedicated directory confirmation through probe review.
The Linux PTY test checked that paste composes the alias and Enter starts
directory preflight. The final Fedora source manifest includes the
updated end-to-end planning fixture for Options repair and has SHA-256
`7697b1cfa35331c4cae96bd593e4c161c055f177972683d48150d0151a9b5d4b`.

The isolated Fedora workspace, Windows screenshots and native and macOS
cross-development artifacts were removed. The Windows checkout occupied
123 MiB, including 42 MiB under `target`. Both retained Windows packages
matched their SHA-256 sidecars.

Interactive alias checks on Windows, macOS and native Wayland, plus attached
SSH and configured provider sessions, remain part of A1 acceptance.

### Agent and Planning JSON response format compatibility

Java `TerminalAgentService.java:1976–1999` and
`JobSchedulerAiSupport.java:316–336` can resend the same masked system and
user prompts without the provider JSON response format. Rust negotiates
this only on an explicit unsupported-format HTTP 400 or 422 from an
OpenAI-compatible endpoint. Each logical generation can retry once on the
same deadline and profile. The response is still checked against the stage
schema. Planning Report requires `final_plan`, and Options retains its
`blocked` result.

Compatibility retry and schema repair have separate progress events. A
worker waits for renewed authority before resending. Interactive Agent,
Swarm and Planning recheck the original run, source, profile, context budget
and redaction rules. Scheduled diagnostics check the dispatched job and its
approved action, the current saved and live profile, policy, and the routes
and host keys for the retained evidence. The model retry stays inside the
original job and uses the existing masked diagnostic prompt.

The usage ledger settles each dispatch. Activity shows the actual token
counts for each logical generation and distinguishes schema repair from
format negotiation. A provider refusal with no token counters stays
unreported. The provider's rejection body and the invalid model response
stay out of the resent prompts.

The Fedora X11 backend ran in a private Mutter 50.6 Xwayland session. An
Ask alias reached an isolated OpenAI-compatible loopback endpoint. The
endpoint refused JSON mode for the initial generation and again for its
schema repair. Both accepted retries matched the corresponding original
request with only `response_format` removed. Activity showed the
compatibility notice, the running repair row, and the final answer with
26 reported tokens across two logical attempts. The endpoint counted four
HTTP requests. The checked executable SHA-256 is
`0624a1e58c280e2451d5f84befe075dc62f4e9c396b44dbdf4e3a63e92169d37`.

The opt-in Fedora OpenSSH fixture ran the scheduled Swarm diagnostic
worker with a reviewed job, one reachable pinned target and one blocked
target. The authorization was bound to the active scheduler ticket and to
the retained probe result. The two model requests contained the same
masked evidence and the original job recorded its individual target
outcomes. The test uses an isolated SSH server and loopback model fixture.

Windows ARM64 passed 2,636 workspace tests with 31 ignored across 63 targets.
Fedora 44 ARM64 passed 2,726 with 108 ignored across 63 targets, and passed
the opt-in OpenSSH test separately. Formatting and all-targets Clippy with
`-D warnings` passed on both. The macOS ARM64 all-targets Clippy cross-check
passed from Windows. The final Fedora source manifest has SHA-256
`6cef57826b5e6a6e98a66d64b977293e91b4c18c47b794844c260fb3d4f7fe95`.

The isolated Fedora workspace, Windows screenshots and native and macOS
cross-development artifacts were removed. The Windows checkout occupied
123 MiB, including 42 MiB under `target`. Both retained Windows packages
matched their SHA-256 sidecars.

Configured providers and the full Scheduler desktop flow remain acceptance
items. This checkpoint preceded the proposal drafting change below.

### Scheduler proposal compatibility and plain Swarm workflow drafts

The one-use Scheduler proposal applies the format negotiation from pinned
Java `JobSchedulerAiSupport.java:316–336` at the proposal's reviewed model
request. One explicit unsupported-format response can trigger a resend of
the same masked prompt and system instruction with the format option
omitted. The output cap remains 4,096 tokens on a shared 125-second
deadline. The strict proposal schema and ordered target list apply to the
answer. Execution still requires a separate approval and a one-use receipt.

The pending review retains its exact prompt, profile, context limit, job
draft and graph revision. The request nonce binds the format permit and
completion to the original view. Before the initial send, retry,
completion and approval, the Workspace checks the current live and saved
profile, current policy and redactions, and each reviewed SSH revision and
pin. A late completion for an older ticket leaves a newer review intact.

Interactive Swarm workflow generation follows the plain-source contract
in Java `WorkflowScriptGenerator.java:295–344`. The model receives the
reviewed masked input and returns script source. The decoder normalizes
line endings and accepts a leading Markdown code fence. The filename comes
from the masked goal and the selected language. The result opens as an
unsaved snippet for explicit editing and publication. Java's Swarm dialog
uses an automatic header. Its single-server dialog adds a header picker
and per-language defaults. The next workflow checkpoint covers both flows.

The native Fedora 44 ARM64 desktop test opened the Scheduler in an
isolated Mutter 50.6 Xwayland session. A loopback model received the
masked request, rejected its JSON option, then accepted the byte-equivalent
model messages with just that option omitted. The Scheduler showed the
retry status and offered separate approval for the resulting command.
On a second review, the fixture rotated the pinned SSH host key before
returning the model's format rejection. The Scheduler discarded the
review, and the model received no retry. The desktop executable SHA-256
was `304316ceea7293b6c204770db52a05e017eae495c6c0f9a6272a1a632ef79e4b`.

The two opt-in OpenSSH fixtures passed on Fedora. The proposal test
verified that the format retry starts no SSH session, two pinned targets
receive exactly their individually reviewed commands after approval, and
the receipt cannot be reused. The diagnostic test verified masked model
evidence with individual reachable and blocked target outcomes.

Windows ARM64 passed 2,642 workspace tests with 31 ignored across 63
targets. Fedora 44 ARM64 passed 2,732 with 108 ignored across 63 targets.
The Fedora test and desktop source manifest has SHA-256
`ec730b8b85bdfab06c410aecf386a284085692529eefeb9c352499491b98e8e0`.
The final Scheduler help distinguishes a one-use proposal from fixed
diagnostics. Formatting and all-targets Clippy with `-D warnings` passed
on both platforms after that wording change. The macOS ARM64 all-targets
Clippy cross-check passed from Windows. The final Fedora source manifest
before this verification record has SHA-256
`9a47dcee3524fd5c42af3f33beddadf2b3787dcda507a7a7ef631c598cf04dca`.

The Fedora test directory and the Windows screenshots and development
artifacts were removed. Both retained Windows package archives matched
their SHA-256 sidecars. Windows measured 124 MiB for the checkout and
42 MiB for `target`, excluding the access-restricted SSH key directory.

Acceptance still includes configured external providers, the interactive
Swarm workflow on a native desktop, and the remaining per-run Agent and
iterative scheduled Swarm behavior from Java.

### Automatic and selected workflow script headers

Interactive Swarm adds its automatic header locally, after extracting the
model's bounded plain-script output. Its review displays the exact block
alongside the masked model input. Neither model message includes local
author, date, profile-name, or template metadata. The generated script
uses the language's lead line and comment syntax. A complete matching
header is inserted once.

The single-terminal entry follows pinned Java `AgentRunActions.java:75–112`.
The action is available on an ended standalone Agent run with recorded
command results. A native owned dialog supports all eight Java generation
languages, up to five suggestions per language, additional instructions,
and optional hardening. The review shows the selected destination, exact
system and user messages for each language, and the header to insert.
The model receives typed and masked executed command evidence, including
exit status and bounded output. Probe-only runs and unconfirmed work do
not enable the action. The dialog rechecks the owned stream, profile,
redactions, exact reviewed inputs, and saved header and variables before
send and before another suggestion. Cancellation closes the owned model
request, and results are bound to their dialog generation and request index.

The header picker supports no header, the default, and matching
`Script-Header` snippets. A missing per-language default falls back to
automatic. Selected templates and their resolved local placeholders
remain separate from the model request. The resolver accepts comment
lines valid for the chosen script language, rejects clipboard
placeholders, and rejects executable batch comment syntax. It rechecks
selected snippets and only the variables they reference. The per-language
default save preserves other settings already on disk and detects a
conflicting edit to the same language.

The dialog lets the user edit and copy a draft, open an unsaved snippet,
or open the native script diagram. Private drafts can be edited and
copied under the current clipboard policy. They stay out of the ordinary
snippet and diagram windows. Remaining Java comparison includes
script-specific hardening prompts, persisted hardening preferences,
save-time metadata and the dedicated model-assisted Mermaid action.
Managed `[[script-header]]` templates and the Java custom-template
creation policy remain a separate enterprise-policy gate.

The native Fedora 44 ARM64 Xwayland run used Mutter 50.6 and a private
profile. The production executable ran its approved local Bash probe
and one approved `printf`. The completed activity opened the workflow
dialog. A loopback model received two plain-source requests containing
the masked goal, approved command, exit status and command result.
Author information, timestamps and custom-template text were absent
from the requests. The automatic result showed the generated filename
and header. The custom result resolved a local username variable and
the dialog saved that template as the Bash default. The generated
source opened in the Snippet Manager as an unsaved edit. The fixture
verified that the saved snippet library still contained only its original
header template.
The desktop executable SHA-256 was
`b49d6aa19c1842e3ddbd6f0f2461c4c01217ae4d51b7e15eed69d755226c40dc`.
An independent private Xwayland run used the same binary to open the
Swarm workflow dialog. The review showed the masked goal, synthetic
inventory path and exact automatic header. The loopback model received
one plain-source request without local header facts. The generated
Bash draft showed the reviewed header above the returned source.

Windows ARM64 passed 2,652 workspace tests with 31 ignored. Fedora
44 ARM64 passed 2,742 with 108 ignored. Both covered 63 targets with
formatting and Clippy `-D warnings`. The macOS ARM64 all-targets
Clippy cross-check passed from Windows. The Fedora desktop source manifest
has SHA-256
`3cab7dee8e034fcd9106f5ba5df1aba5d36a032d81dc89f8b063ae065432b422`.
The final Fedora test manifest contains a fixture-only correction and
has SHA-256
`9a4c7aedc8b0f1ad6b3df35a2c3df1a15c2c81ce462764c6951a06af64c17278`.

The Fedora source and test directory, Windows screenshots and development
artifacts, and macOS cross-development artifacts were removed. Both
retained Windows packages matched their SHA-256 sidecars. Windows
measured 124 MiB for the checkout and 42 MiB for `target`, excluding
the access-restricted SSH key directory.

### Workflow settings, flowcharts, and snippet publication

The standalone script generator uses the shared Snippet Manager
preferences for all eleven classic hardening options and five input
guards. Separate saves update the chosen preference group. The workflow
also remembers single-language mode and script font size for regular
runs. Private dialog changes stay local to the dialog. The force guard
can override a documented non-security precondition. Input safety
checks still apply.

Multiple suggestions now send distinct, individually reviewable
instructions with the alternative number and count. The result keeps
the corresponding language and suggestion number. Clearing hardening
options removes their workflow requirements. Ansible does not receive
imperative input guards.

**Workflow flowchart…** takes the current edited result and opens a
separate review for the originating Agent profile. The review shows
the exact instructions, numbered source and destination. The request
is bound to the workflow window, run, generation, selected result and
source. Editing the draft invalidates a queued result. The model's
Mermaid and references pass the bounded native parser and source
validation. A failed model request can use the local Bash parser or a
bounded structural outline. Regeneration replaces the result's
existing diagram and keeps its identity. An unchanged cached diagram
can reopen without another request.

Opening an unsaved snippet transfers the masked goal description,
workflow tag, detected operating system, and matching diagram. It
assigns a unique name with the suffix before the extension. These
drafts stay in memory until the editor's explicit Save. If a later
editor change invalidates the captured diagram, Save omits it and
reports the omission. Native structured exports preserve the
operating-system label. Java's standalone import omits that field.

The Fedora 44 ARM64 desktop check used Mutter 50.6, Xwayland and a private
test profile. It persisted a cleared hardening selection, approved a
local Bash probe and a synthetic command, generated a draft through
a loopback model, and edited the draft in the workflow window. The
diagram review sent the edited source with its own instructions. The
native preview displayed the corresponding source reference and
wrapped long source lines within its editor. The Snippet Manager
showed a uniquely named unsaved draft, with the original library
record intact and no recovery file. After clicking **Save**, the
new record contained the exact edited Bash source, masked description,
workflow tag, Linux label and one source-bound diagram. The executable
SHA-256 was
`34e408a997ba295661030e53ffa3eb4875aca7757cb59869d8be10c659e8cc30`.
The source manifest before this verification record has SHA-256
`29443539c487da48e43ed82a98b62e9237ce5f4d66cc2cdadff5cb23d023963f`.

Windows ARM64 passed 2,658 workspace tests with 31 ignored across 63
targets in serial execution. After the preview wrapping and headless
placement fixture changes, all 21 focused workflow tests passed again.
Formatting and all-targets Clippy with `-D warnings` passed, including
the macOS ARM64 cross-check from Windows.

Fedora 44 ARM64 passed 2,748 tests with 108 ignored across 63 native
test targets, plus one passing doctest. The existing PTY title test
timed out with Fedora's interactive Bash prompt. The same test binary
and the full suite passed with a test-local `SHELL=/bin/sh` and shell
startup hooks cleared. Formatting and all-targets Clippy with
`-D warnings` also passed on Fedora.

The private Fedora verification tree, Windows captures and native
development files, and macOS cross-development files were removed.
The Windows checkout measured 125 MiB with 42 MiB under `target`. Both
retained Windows archives matched their SHA-256 sidecars. The Fedora
checkout measured 29 MiB after cleaning and before removal.

Native Windows, macOS, and Wayland workflow acceptance and configured
external provider checks remain open.

### Managed script headers and analysis retention

The installation policy accepts ordered `[[script-header]]` Bash
templates and resolves each `[rule.snippets]` setting at its highest
matching scope. A denial wins among equally scoped creation rules.
The smallest byte cap wins among equally scoped retention rules.
Managed template identities derive from the organization and template
name so reordering the declarations preserves saved defaults. Templates
use the existing 8 KiB native script header limit.

The Snippet Manager shows the managed catalog in a read-only panel
separate from user snippets and their exports. The standalone Agent
workflow and Bash snippet analysis offer these templates. A workflow
can set a managed template as the Bash default. Its review shows the
local prologue and the request separately. Bash workflow drafts can
carry the managed shebang and active prologue from the Java example.
Snippet analysis reviews the resolved prologue in the apply request
and preserves the existing script's interpreter. Managed template
changes and changes to referenced variables invalidate pending
requests.

The user library checks new identities and reclassifications under
its write lock. Existing custom headers remain editable. Restore
checks at queue time, on replay and before publishing snippets. A
denial leaves the current library available and blocks the incoming
custom headers.

The analysis cap applies on history writes, including later writes
to existing histories. Oversized analysed text, before and after
source, checkpoint code and resolved header text are omitted. Exact
source anchors are dropped when the corresponding proposal source
is omitted. Findings, reasons, digests and decisions remain. The
live worker holds the exact proposed code in memory and binds it to
the saved digest before offering explicit acceptance. Reopening an
entry with omitted source shows its metadata and recorded decision.

The Fedora 44 ARM64 Xwayland desktop fixture used an installation
policy with a managed Bash template, custom-header creation disabled
and a zero-byte retention cap. It selected the managed template,
persisted its Bash default and showed the exact prologue in the
generated snippet. The loopback provider saw the
reviewed evidence without the fixture's local prologue. The managed
catalog was read-only. Reclassifying the unsaved draft as a new
custom header disabled Save and left the original snippet library
intact. The desktop executable SHA-256 was
`3f72b1ea578405644f4666de8cc5ea46bad21025b8d816058eac998cf236cc89`.
Its source manifest has SHA-256
`98fdd96e2df51b404ac52c51322a3a24bee6b1689b715798e36abdc5ec331ea2`.

A second Fedora Xwayland run exercised the zero-byte cap in the native
Snippet Manager with the final behaviour. It sent the synthetic source
to a loopback profile after review, saved the analysis and selected one
finding. The live comparison displayed both code revisions. Clicking
**Accept changes** placed the proposed code in the unsaved editor.
The analysis directory held the proposed and then accepted receipt
with neither source marker, and the user library still held its saved
original. The executable SHA-256 was
`ae9059ab0ecca35181f078df38e07725ee62583acabe4e6f15b512336ad13eb0`.

Windows ARM64 passed 2,675 workspace tests with 31 ignored across
63 targets in serial execution. All-targets Clippy passed on Windows
and for the macOS ARM64 cross-target.

The final Fedora workspace passed 2,765 tests with 108 ignored across
63 native targets, plus one passing doctest. Formatting and all-targets
Clippy with `-D warnings` passed. The suite used the test-local shell
described above. The tests covered scoped precedence, stable template
IDs, creation and restore guards, fresh dispatch authorization, and
zero-byte live review and acceptance with a fresh metadata-only reopen.
The final Fedora source manifest before this verification record has
SHA-256 `356b795f587f69444777bf8aaf3cfd6271059ad62976962e0c6968c0a7ffbe25`.

The analysis picker text was then updated to describe managed headers
and the retention rule. Formatting and all-targets Clippy passed for
that text change on Windows, Fedora and the macOS ARM64 cross-target.
The Fedora wording manifest has SHA-256
`9996f77fc70f26cce37ff5e7e0f46bb7d6869bf93f8cae869d6dfa12713df52b`.
The private Fedora tree and Windows captures were removed along with
native and cross-development artifacts. The cleaned Windows checkout
measured 125 MiB with 42 MiB under `target`. Both retained Windows
packages matched their SHA-256 sidecars. The Fedora checkout measured
29 MiB with an empty development target before removal.

Native analysis retention and managed-header acceptance on Windows,
macOS and native Wayland remain open. The pinned Java Swarm dialog
uses its automatic header, so the managed chooser lives in the
standalone workflow.

### Scoped Agent, Swarm, and Planning permissions

The policy loader accepts the pinned Java feature names `ai-agent`,
`ai-planning` and `ai-swarm` under `[rule.features]`. Each uses its own
highest matching scope under the master AI permission, with deny
winning at the same scope. Validation includes inactive rules.

Agent and Ask share the Agent permission. Planning uses the Planning
permission and Implement also requires Agent execution. The terminal
context menu uses the clicked pane for all three actions. The menu and
palette use the workspace policy loaded at startup. The standalone
workflow and current model dispatches recheck installed policy.
Agent shortcuts use the Agent permission and the Plan alias also
uses the Planning permission.
The expanded and compact Activity views hide the handoff to Terminal
Chat when the workspace disallows Chat.

Interactive and saved AI Swarm modes require Agent and Swarm. The
existing multi-target rule independently governs saved script review.
The dispatch checks preserve the original stream, reviewed saved
route, masking and approval boundaries. Scheduled AI Swarm needs both
feature permissions, and both scheduled AI kinds keep the unattended
execution rule. Worker checks cover SSH startup and exec, initial
model dispatch, compatibility retries and repair requests.

On 2026-11-10, formatting and all-targets workspace Clippy with `-D warnings`
passed on Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 all-targets
Clippy cross-check passed. The serial workspace suite passed 2,688 tests
with 31 ignored on Windows and 2,778 with 110 ignored on Fedora, across 63
targets each. A parallel Fedora attempt encountered a managed-model registry
lock conflict. The serial run and both encrypted-key worker fixtures used
Fedora source manifest SHA-256
`caea63ffed05b7443770e33ee2997d25d1503b27734cfe5bcfc58141da85b719`.

On 2026-11-10, the Fedora ARM64 desktop ran in a private Mutter/Xwayland
session. With Agent, Chat and AI Swarm denied at startup, the clicked
terminal's AI menu offered Planning. A loopback model received the first
request. After the installed policy revoked Planning, answering the model's
question showed the policy denial and the fixture recorded one request in
total. The Activity view hid the Chat handoff. Swarm opened in saved script
review mode under the separate multi-target permission.
The desktop binary SHA-256 was
`c7072a3108ebf74d4da302cf6ac6c8cb57101dfdfacb5426922c6941cfceba1e`.

Desktop interaction for these policies on Windows, macOS and native Wayland
remains open. After verification, the private Fedora checkout and Windows
captures were removed, and both Windows package checksum sidecars matched.
The cleaned Windows checkout used 126 MiB with 42 MiB under `target`. The
Fedora checkout used 29 MiB with a 4 KiB target directory before removal.

### Per-start proposals for scheduled AI Agent

The single-target native mode follows the decision and ordered execution in
pinned Java `JobSchedulerAiSupport.java:70–169`. The job editor authorizes
the exact masked request for the approved AI profile revision, schedule and
pinned SSH route. Model work starts for each manual or calendar firing.
An explicit refusal of JSON response mode can trigger one format-only retry
under the same request authority. A completed or blocked model decision
finishes the run without opening SSH.

The active Scheduler Journal holds the proposed commands in memory. Its
review shows the exact order, working directory, route and host pins.
Approving consumes a run-specific ticket when the execution worker starts.
The ticket expires after five minutes. An awaiting run uses no SSH worker
slot. Declining, cancelling, expiring or shutting down completes that run.

The SSH worker uses one pinned authenticated connection with individual
exec channels for up to three commands. It rechecks the run, vault, profile,
route, policy and command mask at the relevant dispatch boundaries. The
first nonzero exit stops the sequence. Journal details contain the result
and the position of a failed command.

Guarded private-key authentication rechecks startup authority after the
server confirms the public key and before signing. The wire test on Fedora
44 ARM64 denied that authorization and checked that the server received no
signed request for Ed25519, ECDSA P-256/P-384/P-521, and RSA/SHA-256 and
RSA/SHA-512. The matching allowed cases and the exec-only transport test
passed with both classic and hybrid key exchange against OpenSSH 10.2p1.
Both focused RSA authentication tests also passed in a private copy of the
vendored Sunset source. The fallback test checked deferred SHA-256 signing
after the server rejected the unsigned SHA-512 probe.

On 2026-11-10, all-targets workspace suites passed 2,695 tests with 31
ignored on Windows ARM64 and 2,785 with 113 ignored on Fedora 44 ARM64.
Each suite covered 63 targets. Formatting and all-targets Clippy with
`-D warnings` passed, as did the macOS ARM64 Clippy cross-check.
The Fedora verification used source manifest SHA-256
`556c8cd83707c0a823230f4479d5b8dc2094126c297f3b41576a7ec3b5df7d74`.

The workspace fixture reopened the saved model request approval. The
loopback model received a masked request for each tested start. Stale
editor and Journal approvals, rejection, cancellation, expiry and a locked
vault left the SSH listener untouched. The separate Fedora OpenSSH fixture
approved a three-command proposal. One pinned login executed two commands,
and the second returned exit 17. The scratch marker recorded the first two
commands in order. The persisted job and run record retained the failure and
its ordinal. The fixture exercised workspace messages and a real SSH peer.

Run `cargo test --locked -p kortty --bin kortty firing` for the focused
suite. The `openssh_scheduled_agent_firing_runs_reviewed_commands_in_order`
filter with `-- --ignored` selects the real scheduler and OpenSSH case.
The SSH crate's `openssh_exec_only` filter with `-- --ignored` selects the
transport and signing fixtures. Set `KORTTY_OPENSSH_TEST_CRYPTO` to `classic`
and `hybrid` for the two transport runs.

Native interaction in the separate Scheduler window and hosted tab,
independent multi-target Agent decisions and iterative scheduled Swarm remain
open. The independent SSH review remains a release gate.

Development artifacts and the private Fedora checkout were removed. The
cleaned Windows tree used 127 MiB with 42 MiB under `target`, and both
retained Windows package checksums matched. The Fedora source used 29 MiB
with a 4 KiB target directory before removal.

### Independent advice for scheduled read-only Swarm

The new mode follows the per-target query-only path in pinned Java
`JobSchedulerAiSwarmSupport.java:87–169` and
`TerminalAgentService.java:1684–1701,2755–2763`. The job review shows the
fixed system and account probe, pinned routes and masked question. A
saved approval binds the exact masked question and AI profile revision.

The scheduler runs fixed probes in pairs. Each eligible target has its own
SSH route and one masked model request at a time. The model receives the
common question and evidence from only that target. The actual provider
send checks the singleton target, current job and profile, policy, pins,
vault revision and masking rules. Compatibility retries and repairs have
the same checks. A response asking for executable work cannot reach SSH.
The Scheduler Journal displays each target's outcome and bounded advice in
the saved target order. Iterative commands, target session journals and
saved Swarm reports are tracked separately in Q1.

On 2026-11-10, two disposable Fedora 44 ARM64 OpenSSH targets each ran one
fixed probe. A loopback model received separately masked evidence and one
target's generated command response was rejected. Only the first target's
advice and the second target's blocked result were saved, in order. A
second run used a wrong pin for one target and cancelled the other at the
model authority boundary. The wrong-pinned route and the model listener
received no connections in that case.

The single-target Agent also passed both the manual and real calendar
OpenSSH cases. The production scheduler subscription dispatched the job
after the selected UTC minute. The run and saved journal recorded the
scheduled trigger. The observed SSH listener received its first connection
after exact command approval. Duplicate approval left the journal with one
completed run and produced no additional connection.

The `openssh_scheduled_read_only_swarm` and `openssh_scheduled_agent_`
filters select the opt-in cases from `cargo test --locked -p kortty --bin
kortty <filter> -- --ignored --test-threads=1`. The Fedora source manifest
SHA-256 was `cbcfe13884ffcd8c1d60101d4b78b3493f673ebb06ba9ddd7a0afe73fb7dfcb8`.
The fixtures exercised workspace events and real SSH peers. Native Scheduler
window and hosted-tab interaction remain open.

Formatting and all-targets workspace Clippy with `-D warnings` passed on
Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check
passed. All-targets workspace suites passed 2,696 tests with 31 ignored on
Windows and 2,786 with 116 ignored on Fedora, across 63 targets each.

The private Fedora checkout and development artifacts were removed. The
Fedora source measured 29 MiB with a 4 KiB target directory before removal.
The cleaned Windows tree measured 127 MiB with 42 MiB under `target`.
Both retained Windows package checksums matched.

### Saved reports and target journals for scheduled read-only Swarm

The Scheduler Journal opens a masked read-only report from a finished run.
The native scheduler store writes it atomically with the run record and keeps
the masked question and each target's status and bounded advice in review
order. Journal retention and deletion remove the report with the record.
Portable backups omit run history and reports. Cancelled runs keep their
ordinary outcome history without a saved report.

The action can also record masked probe output through the Session Journal
Manager, grouped by a common automation run identity. Recording commands is
a separate option and records the fixed command only after SSH accepts it.
Stale or private routes cannot start a capture. Capture finalization rechecks
the route and masking policy. The read-only snapshot covers the result-saving
part of `JobSchedulerAiSwarmSupport.java:157–169,266–308`. Native desktop
interaction, the Java Swarm conversation library and continuation remain open.

On 2026-11-10, the two opt-in Fedora 44 ARM64 OpenSSH tests reopened the
native scheduler store and verified the ordered success and blocked results.
The separate journals shared an automation run identity and kept only their
own masked probe output. The opt-in command capture recorded the accepted
command for the cancelled target. The stale-pinned target created no session
journal, and the cancelled run created no saved report.

Formatting and workspace Clippy with `-D warnings` passed on Windows ARM64
and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check passed. The workspace
test suites passed 2,700 tests with 31 ignored on Windows and 2,790 with 116
ignored on Fedora, across 63 targets each. The two opt-in scheduled Agent
OpenSSH regression tests also passed, including a real calendar start. The
Fedora source manifest SHA-256 was
`19025dc6a70dc84632d6ba828dba353f43498c4f4fb0e9eacdc552a7f8e886c0`.

The private Fedora checkout and Windows and macOS development artifacts
were removed after verification. The cleaned Windows tree used 128 MiB with
42 MiB under `target`. Both retained Windows package checksums matched.

### Independent target proposals for scheduled Agent

The per-start workflow keeps a single Scheduler run for up to eight saved SSH
targets. The saved job review approves one exact masked model request per
target in order. The runtime requests the next decision only after the
current target ends. The Journal shows the active target, route and exact
commands. Each command review uses a new ticket for the parent run and
current target. A target rejection, failure or review expiry can advance the
sequence. An explicit run cancellation skips the unstarted targets.

This implements the ordered target loop in pinned Java
`JobSchedulerJobRunner.java:161–224` and the separate decisions and stop on
first command failure in `JobSchedulerAiSupport.java:90–169`. Model dispatch
and SSH execution recheck the reviewed route, policy and shared credential
mask. A route can fail independently when the remaining requests can still
reconstruct the common mask. If the mask cannot be reconstructed, model
dispatch stops before any request is sent. The legacy singleton approval
encoding remains supported. Native window and hosted-tab review interaction
remain separate acceptance items.

On 2026-11-10, the opt-in Fedora 44 ARM64 OpenSSH test
`openssh_scheduled_agent_reviews_each_target_independently` saved and
reopened the approval, then observed one model request per pinned target.
The first target exited with status 17 and skipped its remaining command.
Replaying its ticket during the second review did not start SSH. The second
target ran after its own approval. One durable record contained the ordered
failed and successful outcomes. The existing manual and real-calendar
single-target Agent tests and both read-only Swarm tests also passed.

Formatting and all-targets workspace Clippy with `-D warnings` passed on
Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check passed.
The workspace suites passed 2,709 tests with 31 ignored on Windows and 2,799
with 117 ignored on Fedora, across 63 targets each. The Fedora source
manifest SHA-256 was
`7c807f9a16cf796f2791855ec8dbfc8b7140107473baaa57c5e5f0935e4d62a8`.
The staged Fedora copy preceded the bundled README wording update.

The private Fedora checkout and the Windows native and macOS cross-target
development artifacts were removed. The Windows checkout used 128 MiB with
42 MiB under `target`. Both retained Windows package checksums matched.

### Iterative targets in scheduled AI Swarm

The Scheduler runs two to eight pinned targets in the saved order. The job
approval covers each masked starting request and the bounded iterative
protocol. A fresh review ticket identifies the current run, target and
round. Only the exact reviewed commands can execute. The next request can
use the target's own masked, bounded command results, including a nonzero
exit that ended the preceding batch. The worker rechecks route, policy and
masking before releasing evidence. A target has eight command rounds and a
final round that accepts only a conclusion. A continuation requires enough
model context for its most recent round.

Terminal target outcomes and the masked question are saved atomically with
the finished run's Swarm report. A cancelled run keeps its ordinary outcome
history. The initial request
starts with empty command history. Pinned Java
`JobSchedulerAiSwarmSupport.java:87–169` also supports an initial environment
probe, configurable parallelism, target Session Journals and a saved Swarm
conversation. That initial checkpoint tracked these separately alongside
native desktop interaction.

On 2026-11-10, `openssh_scheduled_swarm_iterates_with_isolated_evidence_and_round_tickets`
passed on Fedora 44 ARM64 with two real OpenSSH routes and a loopback model.
The first target completed two reviewed command rounds, including exit 17
as continuation evidence. The second completed one. Each model request
contained its own target's history. Stale same-target and cross-target
tickets produced no SSH activity. Reopening the scheduler store recovered
one ordered report with the two conclusions and no raw commands or output.
The two read-only Swarm and three scheduled Agent OpenSSH cases also passed.
The iterative case used a manual start. The later reviewed-environment-probe
check exercised a real iterative calendar start.

Formatting and all-targets workspace Clippy with `-D warnings` passed on
Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check passed.
The workspace suites passed 2,716 tests with 31 ignored on Windows and 2,806
with 118 ignored on Fedora, across 63 targets each. The Fedora source
manifest SHA-256 was
`986454d190caa2b16c92d0f54f835e8cfec9272e6f560256351236e70e35a77e`.

The private Fedora checkout and the Windows native and macOS cross-target
development artifacts were removed. The Windows checkout used 129 MiB with
42 MiB under `target`. Both retained Windows package checksums matched.

### Session Journals across iterative scheduled Swarm rounds

The Scheduler opens one optional session journal per target after SSH
accepts the first approved command. The recorder survives later reviews for
that target, while each SSH batch releases its connection. Target journals
share an automation run identity. The worker obtains a separate journal
permit that rechecks the current route, policy and shared credential mask.
The writer masks bounded output and records the exact accepted command only
when input capture is enabled. The ordered digest covers all accepted
commands for retention and deduplication, even with input capture disabled.

At completion, refusal, expiry or cancellation, the scheduler drains the
target writer asynchronously before settling that target. Final artifacts
enter the existing automation retention path together when the parent run
ends. The scheduler admits up to four open iterative target journals at
once. A journal denied before capture or truncated after a scope change
does not change the SSH or model outcome. Native desktop interaction remains
an acceptance item.

On 2026-11-10, three opt-in Fedora 44 ARM64 OpenSSH cases exercised these
journals with real SSH and a loopback model. Two completed runs kept two
rounds for the first target and one for the second under a shared run ID.
The output-only case kept no command inputs. The input-enabled case recorded
only the exact accepted commands. The first target emitted a synthetic
passphrase registered for the second target's encrypted key, and both its
model request and journal contained masked output. The cancellation case
closed one journal as cancelled while the first target awaited its next
review. The second target received no SSH connection and created no journal.

The early-seal regression confirmed that the writer's terminal outcome is
recorded before the completion acknowledgement, including when a truncated
writer had already closed. The repair verified the native journal's exact
identity and preserved an existing outcome. An initial Windows suite run
observed a staged SFTP download cleanup failure. The isolated recheck and
the full repeat passed.

Formatting and all-targets workspace Clippy with `-D warnings` passed on
Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check passed.
The workspace suites passed 2,721 tests with 31 ignored on Windows and 2,811
with 120 ignored on Fedora, across 63 targets each. Ten opt-in Fedora
OpenSSH cases passed. The source manifest SHA-256 was
`677a391d6d5e28057ca7b3a2b846566cbe7a694ba9d067f4e2c99fd730270f45`.

The private Fedora checkout and the Windows native and macOS cross-target
development artifacts were removed. The Windows checkout used 129 MiB with
42 MiB under `target`. Both retained Windows package checksums matched.

### Reviewed environment probes for iterative scheduled Swarm

New iterative Swarm reviews show the exact SSH invocation for a fixed
system, account and directory check. Its effective command is bound to the
saved model-request scope and pinned target list. The scheduler runs it once
on each target before that target's first model request. The probe uses the
reviewed working directory and SSH account. An unsuccessful probe ends its
target and advances to the next eligible target. Older approvals retain
their original model-only starting scope.

Only the masked, bounded probe result enters the target's model context.
The context carries that result in later rounds of the same target. Later
model-proposed commands still need a fresh Journal review. Optional
per-target Session Journals cover the probe and approved command rounds.
This follows the initial POSIX probe in pinned Java `JobSwarmAgentRunner.java`
and `TerminalAgentService.java`. The native invocation keeps the exact
reviewed directory and uses the existing unelevated diagnostic command.

On 2026-11-10, a real calendar start on Fedora 44 ARM64 used three pinned
OpenSSH targets and a loopback model. The first two targets sent independent
probe results. The first required a new review for its subsequent command
and kept the probe in its second model round. The third probe exited 19 and
made no model request. All three journals kept the exact probe invocation
with input capture enabled. Reopening the scheduler store recovered the
three ordered outcomes. The three manual iterative cases reopened older
approvals and started with model requests before their reviewed SSH commands.
Unit tests verified masking, byte bounds and rejection of missing, failed
or mismatched probe results.

Formatting and all-targets workspace Clippy with `-D warnings` passed on
Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check passed.
The workspace suites passed 2,725 tests with 31 ignored on Windows and 2,815
with 121 ignored on Fedora, across 63 targets each. Eleven opt-in Fedora
OpenSSH cases passed. The initial legacy fixtures used a model configuration
without its persisted usage account. The corrected fixtures used the saved
configuration and asserted the approval after reopening. The Fedora source
manifest SHA-256 was
`327f3b0b1d774f36d9ea6142f6584dda65d3faafc216fd0d57198bda057814b9`.

The private Fedora checkout and the Windows native and macOS cross-target
development artifacts were removed. The Windows checkout used 130 MiB with
42 MiB under `target`. Both retained Windows package checksums matched.

### Configurable parallel targets for iterative scheduled Swarm

The Scheduler Action form accepts a reviewed limit of one to eight targets.
New iterative configurations default to four. An older action without a
limit loads as one and retains its original approval scope. Changing the
limit requires a new review. Pinned Java `JobAction.java:277–279` defaults to
four and accepts up to sixteen. The native maximum matches the eight saved
targets supported by one scheduled Swarm.

One scheduler run owns independent per-target diagnostics, rounds and
journals. A ticket binds its run, target ordinal, round and one-use nonce.
The Journal shows a separate proposal card for each active target. A
declined, expired or failed target can settle while its siblings continue.
The scheduler shares two model workers, four SSH workers and four open
iterative target journals. A target awaiting review or journal completion
continues to occupy its slot in the job. The run finishes after its targets
and journal writers settle. It aggregates the report in saved target order.

On 2026-11-10, `openssh_scheduled_swarm_overlaps_reviewed_targets` passed on
Fedora 44 ARM64 with two pinned OpenSSH routes and a loopback model. Both
probe workers requested independent authority before opening SSH. The test
held both model requests open at the same time and checked target-local
probe data. Stale probe tickets could not approve either proposal. The
second target's reviewed command ran while the first awaited review. The
first was declined. The reopened report preserved the original target order
and both journals contained only the inputs accepted for their own target.
Unit tests covered the saved limit and legacy approval bytes, exact sibling
tickets, reverse-order completion and cancellation while a journal closes.

Formatting and all-targets workspace Clippy with `-D warnings` passed on
Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check passed.
The workspace suites passed 2,731 tests with 31 ignored on Windows and 2,821
with 122 ignored on Fedora, across 63 targets each. Twelve opt-in Fedora
OpenSSH cases passed. An existing Windows privileged SFTP download test
returned `Refused` on its final staged publication in the first suite run.
The isolated test and full Windows repeat passed. The Fedora source manifest
SHA-256 was
`21b50852c295b33ff1c21604aeab4994ebb7fa7d888a19b8ca690a47c85dd768`.

The private Fedora checkout and the Windows native and macOS cross-target
development artifacts were removed. The Windows checkout used 130 MiB with
42 MiB under `target`. Both retained Windows package checksums matched.

### Continuing saved scheduled Swarm conversations

Eligible completed read-only and iterative Swarm runs store a stable
conversation identity and the original profile ID with the finished report.
The scheduler commits a bounded pending seed with the report. The desktop
publishes it to the private profile library and acknowledges the exact
origin after the file is synchronized. Pending seeds survive scheduler
restart and report retention. A full pending queue leaves an explicit open
action on the retained report. Profiles governed by a policy that disables
AI Chat or Swarm cannot publish or open these conversations.

The Scheduler Journal and **Saved Chats → Swarm chats** open the same
conversation. A chat retains the masked question and ordered final target
results for each turn. The library retains up to 128 conversations with up
to 16 turns each. Deleting a chat publishes an identity-only tombstone.
Retrying a seed for that origin leaves it deleted. Removing a report leaves
its published conversation in the library.

Opening the history and reviewing a follow-up are local operations. The
fresh question uses the current reviewed profile, absolute directories,
saved SSH targets and pins. An unavailable prior target is shown for an
explicit selection change. Only the newly reviewed question and current
target context enter the new run. The new command mode requires each
target's ordinary command approvals. A completed follow-up appends a masked
turn to the exact existing conversation revision. Pinned Java
`JobSchedulerAiSwarmSupport.java:266–309` and `SwarmAgentTab.java:484–528,1673–1726`
likewise reopen final summaries and send a fresh question in a new run.

On 2026-11-10, the private Workspace publisher test rejected an unrelated
profile, skipped a conflicting seed while publishing a sibling, and reopened
an interrupted publication after deletion. The tombstone was acknowledged
without recreating the conversation. The loopback follow-up test reopened
the same dialog, required an explicit selection change for a missing target,
and verified that the fresh model input omitted the prior history. The
Fedora 44 ARM64 OpenSSH case used separate exact approvals for the probe and
the generated command. The server observed the reviewed invocation, and the
final masked result appeared at the next revision of the same conversation.

Formatting and all-targets workspace Clippy with `-D warnings` passed on
Windows ARM64 and Fedora 44 ARM64. The macOS ARM64 Clippy cross-check passed.
The workspace suites passed 2,739 tests with 31 ignored on Windows and 2,829
with 123 ignored on Fedora, across 63 targets each. Thirteen opt-in Fedora
OpenSSH cases passed. An existing
Windows loopback webhook test returned `WouldBlock` in the initial suite.
The isolated test and full Windows repeat passed. The Fedora source manifest
SHA-256 was
`a6f65f2fa823a0c6936d9204326a53234a4e9107602aa297d38efd2d2a27d3dd`.

The private Fedora checkout and the Windows native and macOS cross-target
development artifacts were removed. The Windows checkout used 131 MiB with
42 MiB under `target`. Both retained Windows package checksums matched.

On 2026-11-10, a native Windows 11 25H2 ARM64 process (build 26200.9711,
Windows user metrics 96 dpi) used a separate local profile and two loopback
listeners. `cargo build --offline --locked -p kortty --bin kortty` produced
the test executable with SHA-256
`870d239aacfd756823209c00594f273be0f82ffee7f85351b00a9f9a59035aba`
from the source manifest above. Startup published both queued conversations,
including run 41 with its Scheduler record already pruned. A process restart
recovered both IDs, revision 1 and their ordered masked outcomes. Replaying
run 42 after placing its deletion marker in the stopped private profile
acknowledged the queued entry, preserved its Journal record and left the
conversation deleted. The retained conversation stayed available on disk.
Both listener counts stayed at zero. These checks read the persisted state
of the native process. Computer Use app approval expired before the window
inspection. Interactive Journal and Saved Chats entry points and the visual
follow-up review remain open.

### Agent run continuation in Terminal Chat

The native A2 handoff opens a completed or stopped standalone Agent run, or a
plan ready for review, in Terminal Chat. When Chat is permitted, Activity and
the compact history controls expose the action. A bounded retained entry on
a regular terminal also exposes it through the terminal's AI context menu.
The run contributes its masked goal, final answer or status, and typed recent
decisions and approved command results. Pending approvals, privileged password
input and unconfirmed command details are excluded from the seed.

An explicit handoff from a regular terminal creates or opens one native
saved chat for that run under the owner-only store lock. The origin survives
revision changes and rename. A private terminal keeps its chat in memory.
The composer keeps the optional follow-up for exact Review and Send. The
handoff preserves an existing draft or review and retains the new question
in Activity when the composer is busy.

The pending operation binds the original terminal stream, run epoch, AI
configuration and current masking rules. Checked publication revalidates
authority before committing a record. A changed source, policy or run
prevents the stale callback from opening a chat. A restored Agent chat can
carry the masked conversation without live terminal capabilities. Actions
against a live terminal require the exact bound stream. Private Agent chat
also requires that stream for model requests. Replacing it cancels an active
request and keeps the draft local.

Windows ARM64 passed 2,576 workspace tests with 31 ignored across 63
targets. Fedora 44 ARM64 passed 2,667 workspace tests with 108 ignored
across 63 targets. Formatting and all-targets Clippy with `-D warnings`
passed on both. The macOS ARM64 all-targets Clippy cross-check also passed.
The loopback Ask workflow verified the masked seed, single
saved record, retained draft and reopening from the original terminal. The
review fixture checked typed command history and exit status, the follow-up
and stream revocation without contacting a provider. Store and widget tests
covered concurrent opening, rename, checked publication, stale input and
compact and pending controls. The Fedora source manifest before the final
verification record is
`af7fa22b4cfb3fe7b502474750ff2cc30f184b353934046452b3812a10e6c5a0`.

The Fedora verification tree and the Windows and macOS cross-development
artifacts were removed. The Windows checkout occupied 120 MiB, including
42 MiB under `target`. Both retained Windows archives matched their SHA-256
sidecars.

### Agent Journal cards and Java Chat interchange

A regular Agent run adds a masked `AGENT` timeline card when the original
terminal stream is recording. Continue in AI Chat links the exact native saved
conversation to the card. The journal writer serializes the link with capture
and timeline edits. Saved Chat updates refresh the copy during recording. The
journal freezes its last accepted copy on stop. Its internal **Open in AI Chat**
action checks the saved chat's reciprocal journal and card identifiers. The
card displays its read-only snapshot when the saved conversation is unavailable.

Java Saved Chat XML preserves Agent origin, source references and typed steps
on the first assistant reply. The typed representation retains status, signed
exit code, elapsed time and truncation when present. Imported references are
portable metadata and cannot grant native journal or terminal access.

Only a sudo password accepted for the exact run extends its bounded, zeroizing
mask. Later model input is masked before JSON encoding. The mask also applies
to the handoff and its typed command output, including when the exact Agent
chat is already open. The manual desktop workflow remains an acceptance item.

Windows ARM64 passed 2,598 workspace tests with 31 ignored across 63 targets.
Fedora 44 ARM64 passed 2,689 with 108 ignored across 63 targets. Formatting and
all-targets Clippy with `-D warnings` passed on both. The macOS ARM64 all-targets
Clippy cross-check also passed. The Fedora source manifest before this
verification record is
`8264dfb01b62189a9837c1c2c1dc5b87e79828b31af1d7f16b14015ae1e2b91d`.
The journal tests exercised the real writer, revocation before publication,
stale revisions, exact saved-chat reopening and frozen copies. Masking tests
covered credentials with JSON-escaped characters and multiline command output.

The pinned Java `AiChatManager` loaded, saved and reloaded the Rust-generated
Agent export offline with OpenJDK 21 and JAXB 4.0.9. The saved file was
byte-identical to the Rust export, with SHA-256
`32ca3be98684e52636c6ad251a2a66eceb131022ca76664d957cece716a52102`.
The check compared the source references, message and step order, signed exit
code, elapsed time, truncation and masked Unicode command and output.

The Fedora verification directory and the Windows native and macOS
cross-development artifacts were removed. The Windows checkout occupied about
121 MiB, including 42 MiB under `target`. The retained Windows ARM64 and x64
packages matched their SHA-256 sidecars.

### Correcting snippet descriptions and reviewed HTTPS assistance

The editor's **Correct existing description** action reviews a spelling and
grammar correction in the current description's language. It preserves
placeholders and accepts an empty snippet source. Apply and Undo update only
description metadata. The exact draft and original transport still govern
reruns and accepted proposals.

Snippet AI assistance can review a trusted OpenAI-compatible or Anthropic
HTTPS profile. The review shows the exact endpoint, model, system prompt and
JSON user payload. It includes Anthropic's JSON-only instruction and limits
the combined masked prompts to 512 KiB. The request mask covers matching
vault credentials and the provider credential before serialization.
The worker reloads saved AI settings and installed policy before dispatch.
Draft, profile, vault or masking changes invalidate stale reviews or results.

On 2026-11-10, the ten focused tests, formatting, and all-targets workspace
Clippy with `-D warnings` passed on Windows ARM64 and Fedora ARM64.
The Windows run used a private profile with ordinary Windows
permissions after the restricted process could not open the owner-only
fixture. Coverage includes Unicode and CRLF preservation, placeholders,
empty source, Apply, Undo, rerun, payload limits and refused requests with no
connection after policy, vault or saved profile changes.

A private Fedora process trusted an ephemeral CA for its loopback HTTPS
receiver. The test opened the real Workspace assistance flow for both
providers. It observed no request during review and exactly one POST per
approved format. Both receivers got the exact reviewed system and user
prompts and model. The synthetic credential appeared in the provider's
expected authentication header and was masked from the prompts. Apply and
Undo preserved the original source bytes for both formats.

The Fedora source manifest before adding the private test had SHA-256
`ba3ae1b9f21ad94bf5ddad911fe8bbd205515f74b5b521b493a3d933687872d6`.
The test binary, linked with the bounded-memory options recorded above,
had SHA-256
`75957099dd66ffa765b68817e0c81b627775f524147a0cdbe3bbea7efe53e2bc`.
The private test hook, receiver, CA and build artifacts were removed.
The Windows checkout and `target` occupied 132 MiB and 42 MiB after
cleanup. Both retained Windows package checksums matched.
Native click-through and requests to explicitly configured live providers
remain acceptance cases.

### Scheduler file-action Session Journals

The Scheduler offers per-target capture, retention and closing-summary
estimates for all fourteen current file-action variants. Run metadata uses
the corresponding Java action key. A target that begins execution can
receive a bounded masked result as a typed system note. File descriptions,
paths and exclude patterns follow the separate input-capture option. The
worker masks resolved SSH, sudo and archive credentials before writing
those fields. It uses confirmed counts for a partial reviewed folder copy
and leaves protocol diagnostics in the Scheduler result.

The recorder rechecks current capture and masking rules. A revoked capture
is marked incomplete and retains the transfer's actual outcome. File runs
use the existing per-target retention and ordered digest. A complete file
journal ends with its system outcome note in the Java-compatible capture
log and timeline.

On 2026-11-10, Windows ARM64 and Fedora ARM64 each passed 24 automation tests,
the SFTP form round-trip test, formatting and all-targets workspace Clippy.
The suite verifies all action keys,
capture revocation, metadata after early truncation, manual-note
compatibility, and an encrypted ZIP job blocked by a locked Vault.

Two additional tests used private real OpenSSH services on Fedora. The
encrypted ZIP decrypted to the exact binary source with input capture
enabled and disabled. Only the opted-in run showed the masked source path.
Byte scans verified exclusion of the archive password, credential ID and
payload from the journal files. A three-target test produced separate
journals for the blocked and cancelled targets with one run ID. The
remaining target had zero connections and journals. Eleven existing OpenSSH SFTP
regressions also passed, including strict host keys and single-use reviewed
copies in both directions.

The Fedora runtime source manifest before the private test hook had SHA-256
`6bae59e8233a0fbc8f32274c5c37931535690b220e636fb55f127c42973880b7`.
The temporary test binary had SHA-256
`334770dd0cfa290d8e5474ca451231d78fab43a5985455d71c7e4f5fcde2d457`.
Fedora workspace Clippy with `-D warnings` also passed after applying the
Windows-only test adapter change. That source manifest had SHA-256
`4b3cdde4dc38daef723758313b414b4674f577503501bbcb3c2b239b6e5b48d1`.
The private Fedora source, SSH keys, fixtures and development artifacts
were removed. The Windows checkout and `target` occupied 132 MiB and 42 MiB
after cleanup. Both retained Windows package checksums matched. Native
click-through and Java desktop interchange remain acceptance cases.

### Scheduled AI Agent Session Journals

The Scheduler offers per-target Session Journals for fixed system and
filesystem diagnostics, reviewed one-use proposals, and proposals reviewed
at each manual or calendar start. Target journals share the scheduler run
identity and use the existing retention and closing-summary settings.
Command input is opt-in and comes from acknowledged SSH exec requests.
Output and fixed diagnostic advice are masked before capture. The ordered
command digest distinguishes runs with different acknowledged activity.

A per-start proposal that has reached target review can record a typed
outcome even when no command was acknowledged. A blocked or declined target
in this state has no fabricated shell command boundary. Unstarted targets
and rejected one-use preflights leave no journals. Capture checks current
policy, the pinned route and any bound vault revision. Withdrawing capture
authority stops further journal writes and leaves the scheduler outcome
available. The scheduled run settles after the pending target journal closes.

Formatting and workspace Clippy with `-D warnings` passed on Windows ARM64
and Fedora ARM64. All 34 selected application tests passed on each. They
covered saved settings, status-only capture, vault revocation, shared capture
slots, review expiry and one-use scope. The Windows test executable SHA-256
was `b26de5b213f7886e09b38f19b956355f98590f138b153aca68398087d34f84f2`.

Seven isolated OpenSSH cases passed on Fedora with a private model fixture.
The fixed system diagnostic recorded masked output and advice with command
input disabled. The one-use proposal recorded the acknowledged command
and masked output, rejected replay and left the journal empty on preflight
denial. The per-start cases checked ordered manual execution, a real calendar
start and separate outcomes for independent targets. The existing iterative
Swarm capture and cancellation cases also passed. The Fedora source manifest
before the temporary fixture edits had SHA-256
`fc51d4664ec6ec40757914f1a1f1fece8073d5fcb0f913b742875eee5747a2e2`.
The temporary Fedora test executable SHA-256 was
`ded2e7e17ad2fdd9a60fb6eea5f50db36d18d65181ffe36123a53171cba3b2cd`.
The Fedora fixtures, private source copy and development artifacts were
removed. The Windows checkout and `target` occupied 133 MiB and 42 MiB
after cleanup. Both retained Windows package checksums matched.
Native Scheduler and Journal Manager interaction, a configured model, and
Java desktop interchange remain acceptance cases.

### Reviewed SFTP copies with scheduled predecessors

Ordinary reviewed SFTP uploads and downloads now support Condition mode with
predecessors. The folder review lists their names and IDs and the optional
maximum age. The receipt binds those IDs and age to the existing reviewed
route, roots, exclusions, direction and calendar. Jobs without predecessors
retain their previous receipt format. A failed condition blocks the one-date
occurrence before dispatch and leaves the receipt available for an explicit
manual start. The manual confirmation refreshes the blocker list before
accepting an override. Pinned Java `JobDependencyMode.java:10–15` and
`JobSchedulerServiceDependencyTest.java:21–42` describe the corresponding
calendar admission behavior.

Windows ARM64 and Fedora ARM64 each passed seven calendar tests, seven
dependency tests and 28 selected Scheduler application tests. Formatting and
workspace Clippy with `-D warnings` passed on both. The calendar regression
covered a blocked occurrence, an explicit manual start and the absence of an
automatic catch-up after a later predecessor success. The application tests
covered receipt scope, the pre-existing receipt format and a changed blocker
while the confirmation was open.

Three isolated OpenSSH tests passed on Fedora. Two scheduler histories
used the same reviewed copy and fixed calendar time. A failed predecessor
produced one blocked result, zero SSH connections and a byte-identical
receipt. A successful predecessor admitted one transfer and published the
exact bytes. Replay was blocked before connection. The other tests covered
reviewed upload and download, preservation of existing files, exclusions
and strict host-key handling. Two supplemental privileged-tree cases ended
during setup because they require a separate `/fixtures` mount and mock sudo.

The Fedora source manifest before the evidence documentation update had
SHA-256 `db9d5464fa64d46bb42da74ffc00ffa079b48a0708fca189e528425bf405c039`.
The test executable SHA-256 values were
`f3d8aaafe56248b4a5b72ea64da18efb5fe843f919321545fecabad0d102bb5c`
on Windows and `3ea18bc6618693bb972bf9a9dfa89842fa7446fe0c16bf9d3145cecad6e237d4`
on Fedora. The private source copy, fixtures and development artifacts were
removed. The Windows checkout and `target` occupied 133 MiB and 42 MiB and
both retained Windows package checksums matched. Native dialog acceptance,
Chain and cascade starts, recurring approval and replacement remain open.

### One-use SFTP copies started by job chains

Ordinary reviewed folder uploads and downloads support Chain mode. The review
binds the mode, exact predecessor IDs, maximum age and optional calendar slot
to one receipt. A Chain start needs fresh predecessor successes since the
copy last ran. It may occur outside the calendar slot. Chain, calendar and
explicit manual starts share a receipt that can be claimed once, before the
copy writes. The Gate receipt formats are preserved. The Scheduler matches a
live request's run ID, job, action, trigger, time and cancellation signal
before issuing an execution context. Dependency dispatch also rechecks
committed prerequisites before SSH.

The scheduler persists each job's latest prerequisite outcome independently
from journal display and retention. Deleting a later failure leaves it in
effect. Legacy profiles recover results that are still current against the
saved last-run timestamps. Definition-only profiles keep their prior byte
representation and portable backups exclude dependency history and receipts.
The pinned Java `JobSchedulerService.java:720–760` supplies the calendar and
Chain trigger reference.

Windows ARM64 passed 29 scheduler-core tests, 28 Scheduler application tests
and 42 profile-backup tests. Fedora ARM64 passed 30, 28 and 48 respectively.
Workspace formatting and Clippy with `-D warnings` passed on both platforms.
The existing GnuPG interoperability case stayed ignored in the filtered Fedora
backup run. The tests covered legacy history, deletion and restart, the exact
receipt scope, cancellation identity and byte-compatible profile restore.

Six isolated OpenSSH cases passed on Fedora. The Chain test transferred exact
bytes after both prerequisites succeeded and checked that an older success or
newer failure could not release the copy. It used both Manual and Dependency
as the first trigger and rejected reuse through the other trigger. Retagged
requests were rejected before a connection. The Gate case used the live
Scheduler's calendar path. The privileged upload and download cases used
synthetic sudo in a private user and mount namespace. The pinned fixture
recorded four test authentications and zero additional connections on replay.

The Fedora source manifest before the evidence documentation update had
SHA-256 `8817c2dd3ccec2ce39ca0fbabb53ccfb05660f1e9287e2fe2be04a838a407a99`.
The test executable SHA-256 values were
`aaf0a03db8b9f22eaa7b228acc0a3da9f8509f58cc4a96378522d9e6afe0dd20`
on Windows and `d35f1d90e0c755f54b8be9605839d85677afd72a7b18cd73acf99c2804f2ba68`
on Fedora. Private fixtures, the Fedora source copy and development artifacts
were removed. The Windows checkout and `target` occupied 134 MiB and 42 MiB.
Both retained Windows package checksums matched. Cascade authorization, native
dialog acceptance, real sudo, recurring approval and replacement remain open.

### Reviewed SFTP copies in manually confirmed cascades

**Run with successors** now asks for an explicit cascade confirmation when a
reviewed folder copy is in the reservation plan. The preview and actual start
share the scheduler's ordering, busy checks and disabled-branch rules. The
confirmation identifies the root and discloses each reviewed copy and its
availability. A root copy needs an unused receipt. An unavailable successor
stops its branch if reached. Privileged folder copies require a separate run.
Other jobs retain their independent calendar and Chain triggers. The pinned
Java `JobSchedulerService.java:314–352,767–827` provides the cascade reference.

The confirmation is bound to the current graph, root, members and exact unused
receipts. At dispatch the root can bypass only the conditions shown in its
confirmation. Successors recheck outside prerequisites and use the successful
stages of the same cascade for internal ordering. The worker rejects an
unconfirmed, retagged, replaced or consumed receipt before SSH. It still claims
the receipt atomically before writing. Workers with separate receipts can
wait briefly for the same approval-file lock. Cancellation while waiting
leaves the receipt available.

Windows ARM64 passed 29 scheduler-core tests and 34 selected application
tests. Fedora ARM64 passed 30 and 34 respectively. Workspace formatting and
Clippy with `-D warnings` passed on both platforms. The application tests
exercised an updated review after a receipt was spent, changed conditions
before dispatch, independent branches, parallel receipt claims, cancellation
and portable backup. Five isolated OpenSSH tests passed on Fedora, including
the confirmed copy as both root and successor. They verified exact file bytes,
no dial after dismissing the prompt, denial of trigger substitution and
replay, and the existing Gate, Chain, manual and strict host-key paths.

The Fedora source manifest before the evidence documentation update had
SHA-256 `917cf138afa120a5b48315bfc0fe9af38f59bcbd9d1150efe3d0ea205c611ffe`.
The test executable SHA-256 values were
`0c2e08278e7d44b9ceaa09e371f1dabbc06ebc72d9015285e584ccdc25a51f15`
on Windows and `a46977990c26a5b7a52079dff50966eb4ecafee4a5863ab83bcd2e36a52b0c67`
on Fedora. Private fixtures, the Fedora source copy and development artifacts
were removed. The Windows checkout and `target` occupied 134 MiB and 42 MiB.
Both retained Windows package checksums matched. Native cascade-dialog
acceptance and reviewed replacement remain open. The next entry records
bounded approval for recurring calendar copies.

### Reviewed SFTP copies with recurring calendar approval

The SFTP job editor offers **Allow recurring scheduled copies on this
device** for an enabled calendar in Condition mode. The read-only preview
shows the roots, direction, calendar, exact expiry in UTC, current plan and
limits. **Approve recurring runs** binds the authorization to the saved job,
calendar and time zone, dependencies and maximum success age, paths, excludes,
SSH route and host pins. The defaults are seven days, 12 attempted runs, 64
entries and 32 MiB per run. The limits are 30 days, 128 attempted runs, 256
entries and 256 MiB per run. Portable backups carry the settings. Each device
stores and grants its own authorization. Manual runs, chains and cascades use
the separate one-use workflow.

An eligible firing claims one durable attempt before connecting. Empty plans
and connection failures count. Replay, expiration, missing authorization and
exhaustion stop dispatch before SSH. An occurrence that is already due can
start later while its approval is valid, consistent with the overdue check in
the pinned Java `JobSchedulerService.java:573–590`. The editor discloses this
behavior. Each run computes a fresh bounded plan and checks source digests.
The workflow publishes only to absent destinations. It rechecks the device
grant, policy and root bindings before creating directories, staging or
publishing files. The local root is bound to its filesystem identity. SFTP v3
provides a canonical path and ownership and mode attributes for the remote
root. External remote renames can race the separate check and write requests.

Windows ARM64 passed 29 scheduler-core and 40 application tests. Fedora
ARM64 passed 30 and 40 respectively. Workspace formatting and all-targets
Clippy with `-D warnings` passed. Six isolated OpenSSH cases on Fedora
covered the new recurring upload and download and the previous one-use
paths. Both recurring cases checked an empty run, a later file after
reopening, rejection of the entire oversized plan, replacement of the local
root, and denial before SSH when the budget was spent. The application tests
also covered portable backup and the editor's calendar-only authorization.

The verified Fedora source manifest SHA-256 was
`ffa5be80a7b43de1ca4f17db3eb1da936d5baa2d48abfd0187efe79806095539`.
The application test executables had SHA-256 values
`710e050b16b6243b2db94fb6cd3aeb46e80a35a98a00d66fdbdcf3f4c4df924c`
on Windows and `792123fedd072faf556face3ede69bc994c234a06ec68ae8dc63dd9ceff6f5c5`
on Fedora. The private Fedora copy and development artifacts were removed.
Both retained Windows package checksums matched. Native dialog acceptance
and reviewed replacement remain open.

### Reviewed server ownership in the SFTP browser

The Server pane in the shared SFTP Manager now offers **Change ownership…**
for a batch of up to 64 selected regular files and directories. The preview
shows the exact folder and escaped entry names with their current numeric
UID and GID. Either field can be left blank to retain that value per entry.
The pinned Java reference combines the owner, group and permissions fields
with multi-selection and an optional recursive mode in
`SFTPManagerTab.java:5014–5122`. The Rust ownership action applies to the
reviewed paths. The existing permissions action remains separate.

The original authenticated SFTP worker enables the action when the server
advertises version 1 of `lsetstat@openssh.com`. It checks the current policy
and route, validates the full batch before writing, then checks the canonical
path and listed attributes again for each entry. The worker submits only
changed numeric fields and reads back the requested and retained IDs. It
reports verified and uncertain results separately if the batch stops partway
through. The review is bound to the displayed listing and selection. Revoking
file transfers or changing privacy mode clears it and stops active work.
Cancellation can close a stalled SFTP channel. SFTP v3 has no atomic entry
identity comparison for this request. A concurrent rename can race the check
and the request, and ownership is shared among hard links to the same file.

Workspace formatting and all-targets Clippy with `-D warnings` passed on
Windows ARM64 and Fedora ARM64. The SFTP application suite passed 77 tests on
Windows and 82 on Fedora. Three separately enabled Fedora tests used an
OpenSSH SFTP server process. The ownership test changed two files from GID
1000 to supplementary GID 10, retained their UIDs, and verified the refreshed
browser listing. It also checked refusal after privacy mode or selection
changes, a replacement symlink and lack of extension support. The two
existing server-copy tests passed. Wire tests verified the unchanged-field
encoding, complete-batch preflight, cancellation, read-back mismatch and a
lost write acknowledgement after one verified change.

The Fedora snapshot manifest SHA-256 was
`bf9f399628226d8af0545672fbb530381b0da26e3aff3a6418da3a98733885bc`.
The Windows and Fedora test executable SHA-256 values were
`250829c6013c66e2fb500fd41670c68f535805094b9b818f9b804588b95be076`
and `293798d0280fb407111ae3167ad55d8f5d2206d2c6201c39e63f865b0b9053ef`.
The development artifacts and private fixtures were removed. Both retained
Windows package checksums matched.

The next entry records local Unix ownership and recursive reviews. Named
principal resolution and native desktop dialog acceptance remain open.

### Reviewed local and recursive SFTP ownership

The Local pane offers numeric ownership changes on Unix. Both supported
panes can include descendants of selected folders. At most 64 roots and 256
total entries are admitted, including skipped entries. The second review
shows exact escaped paths, current IDs, requested IDs and the skipped count.
It binds the request to the visible selection and requires a separate Apply.
The pinned Java local and server actions are `SFTPManagerTab.java:5014–5122`
and `:6180–6278`. The existing permissions action remains separate.

Before the first write, both workers rescan the complete plan and compare it
with the review. They process descendants before their folders, check current
policy between operations, and consume the plan once. Local Unix changes use
no-follow directory traversal, descriptor-based ownership changes, and
same-inode read-back. Local recursion skips symbolic links, hard-linked files
and special entries. The server plan binds the original SFTP worker and host
key, skips symbolic links, special entries and ambiguous names, and uses the
existing no-follow `lsetstat@openssh.com` path. Recursive server scans require
a canonical non-drive SFTP path. As with individual SFTP changes, the protocol
does not provide an atomic identity comparison before the write. A concurrent
remote namespace replacement can race checks or redirect the post-write
listing refresh. The batch reports confirmed and uncertain changes separately.

Workspace formatting and all-targets Clippy with `-D warnings` passed on
Windows ARM64 and Fedora ARM64. The SFTP application suite passed 80 tests on
Windows and 89 on Fedora. Five additional Fedora tests passed. A local test
changed the group with the same inode and retained UID. OpenSSH changed a
file, its folder and the selected root in that order, using supplementary
GID 10. Adding a file after a two-root review caused zero `lsetstat` requests.
The tests also verified skipped symbolic links, unaffected referents, the
separate local browser approval and denial of a replayed server review.
`cargo check --locked --offline --target aarch64-apple-darwin -p kortty
--all-targets` passed as a cross-check from Windows.

The Fedora source manifest SHA-256 was
`ab8e132284d24fde60e0e4d53f0ad7f90144de1aac3db89563c379139e11c45d`.
The Windows and Fedora test executable SHA-256 values were
`3f543d43ff5535f439d5489bb8e905aae59f806012b98b901415df189e1e39d7`
and `60eebb9f8e590a8da6789fd657cafef147840aea67797aaebe52adfd1a731b74`.
The Fedora build used GNU ld's `--no-keep-memory` to reduce memory pressure.
The private Fedora source snapshot and its development artifacts were removed.
The Windows development and macOS cross-check artifacts were removed, and
the two retained Windows package checksums matched.

The next entry records named principal resolution. Combined permissions and
native dialog acceptance remain open.

### Named owner and group review in the SFTP browser

The ownership form accepts numeric IDs or account names in its separate user
and group fields. Either field can remain empty. A named request opens a review
showing the exact name, resolved numeric ID and selected paths. Both local and
remote plans recheck the full selection and resolve the names again before the
first write. Changed or missing mappings invalidate approval. Numeric-only
requests retain their existing single-level and recursive workflows.

Local Unix uses the reentrant system account lookup through the native
`kortty-files` boundary. It bounds record storage to 256 KiB, checks the
returned name exactly and discards the result if the guard changed. The
system's account service can still delay a lookup. A server name requires
the original verified SSH exec lease and `/usr/bin/getent`. The fixed lookup
accepts one matching record, exit status zero and empty stderr within five
seconds and 4 KiB. The write remains the existing numeric no-follow SFTP
request. The isolated worker accepts a typed account lookup with no input
stream. Its standalone SFTP route still refuses general exec requests. A
rejected lookup keeps the form available for correction or a numeric ID.
Names are limited to 64 bytes of portable ASCII spelling.

Recursive review is unavailable for remote drive paths. Individual numeric
changes retain that support. A refresh that resolves to another server folder
preserves the original listing and reports the already verified outcome.
SFTP v3 still has a race between path verification and a concurrent remote
namespace change during a write.

Windows ARM64 passed 86 SFTP application tests and three account-request
protocol tests. Fedora ARM64 passed 95 SFTP application tests, the three
protocol tests, three native Unix resolver tests and seven opt-in cases. The
local fixture resolved `wheel` to GID 10, changed the group and verified that
the inode and owner stayed the same. The real OpenSSH fixture resolved the
exact account and group, refused an unknown name, left the file unchanged
during review and then applied the GID change over direct and isolated SSH.
Both modes used one authenticated connection. Standalone isolation still
refused a general exec request. An OpenSSH SFTP-only fixture rejected name
lookup and applied the numeric GID on the same connection. Workspace Clippy
with `-D warnings`, formatting and the macOS ARM64 all-targets cross-check
passed.

The Fedora source manifest SHA-256 was
`8503dcfdd440541a8b18a097449b2f8ae88f6ddbaba8af447e585b253d69b447`.
The Windows and Fedora application test SHA-256 values were
`a98a972a944addcd9c4643e92b0aefbcacf3e934a4a70446ada1440bb25f75c7`
and `0269798cf42a6f76556aae54026f38dd8360397e5547ffb4a3d3cafb8a8b72a9`.
The Fedora desktop executable that launched the isolated worker had SHA-256
`18e0b50c39967b4224461d5819dbac936dd7761cd0b338c4acb065be58808982`.
The development artifacts and private fixtures were removed. The two
retained Windows package checksums matched.

### Combined ownership and permissions in the SFTP browser

The pinned Java SFTP browser combines optional owner, group and permissions
with a three-digit octal field, a symbolic preview and nine synchronized
checkboxes. korTTY Rust provides the same controls in **Ownership and
permissions…**. Its fields start blank so a multi-selection retains each
entry's value unless the user explicitly supplies a replacement. An exact
mode uses the low nine POSIX permission bits and clears the three special
bits. The review lists current modes and the requested mode before writing.

The remote worker uses two separate `lsetstat@openssh.com` writes when it
must change both fields. It verifies ownership before setting permissions.
OpenSSH applies permissions first if both attributes are sent in a single
packet. [OpenSSH source](https://github.com/openssh/openssh-portable/blob/V_10_2_P1/sftp-server.c#L1401-L1425)

Mode-only requests use a permissions-only packet. Numeric and mode-only
requests require no command channel. The local Unix worker uses
one held descriptor and verifies it against a no-follow stat of the entry
under its checked parent. This lets the worker verify even a mode of `000`.
Starting a new local review still requires access to open the entry.

The result counts complete entries separately from a verified ownership
change followed by a failed or uncertain permissions step. Both sides keep
descendant-before-parent order. SFTP v3 still has a race between a checked
path and a concurrent server namespace change. Native ownership and
permissions dialog acceptance remains open.

The Windows ARM64 SFTP application suite passed 92 tests with one opt-in
test skipped. The restricted Codex account received access-denied errors
while setting up private Windows file fixtures. The identical executable
passed under the normal Windows account in its own private test directory.
Windows also passed all 19 SFTP protocol tests and the three account lookup
protocol tests. The Windows application test SHA-256 was
`dbab943dbdf86060fb1a3ad5f34bdb46de7b3265f53b7b866d777b073b75ea4b`.

Fedora 44 ARM64 passed 102 SFTP application tests, 19 SFTP protocol tests,
three Unix account resolver tests and eight opt-in cases. The browser test
against OpenSSH 10.2p1 used an SFTP-only route. It confirmed that review
performed no writes, the mixed change sent GID 10 before mode `640`, and a
second entry received only mode `600`. The test read the results back from
SFTP and verified that both inodes, UIDs and file contents were retained.
The local named group change confirmed mode `640` on the same inode. A
separate recursive test verified `000` on descendants and directories.
Formatting, workspace Clippy with `-D warnings` and the macOS ARM64
all-targets cross-check passed.

The Fedora source manifest SHA-256 was
`933745987b6de8a160cc820451bc40cd74b0b661db7aa6d7b546dee240403e70`.
The Fedora application test and desktop worker launcher SHA-256 values were
`8b3fa735cf54b1fee1c4a0811508efa7d2331e3b8a14ac49b91c034f4a1dce25`
and `486065dfa8632ba31e7ce5727ed3e0417d13ccd2ac07141aef26e55e85bedf6e`.
The Windows and Fedora development artifacts and private test runs were
removed. The two retained Windows package checksums matched.

### Reviewed server archives in the SFTP browser

**Create archive…** in the Server pane accepts up to 16 selected files and
folders, ZIP or TAR.BZ2, a compression level, optional glob exclusions and
a new relative filename or an absolute destination. The read-only review
shows the exact destination and included archive members, input size and
excluded path count. It is bound to the current selection, original verified
SFTP worker, destination, and archive options.

The browser reuses the Scheduler's native scanner and encoder. It verifies
the included manifest and excluded subtree roots before reading data and
after staging the archive. The worker checks protected canonical directory
chains, reads back the staged bytes, and publishes with a no-replace SFTP
hard link. File data goes through korTTY on the same SFTP channel. Limits
are 4,096 entries, 256 MiB of input and 64 MiB of archive output. The
browser reports a known publication separately from a lost publication reply
or a staging folder that could not be removed.

SFTP v3 supplies separate path and metadata operations. Concurrent edits by
the same server account can still race those operations.
Later checkpoints cover Local-pane archives, 7z and optional Server archive
metadata. Reviewed extraction and native dialog acceptance remain open.

Windows ARM64 passed 92 SFTP application tests, 74 Scheduler tests and
two new archive engine tests. The engine tests check manifest equality,
exclusions, changed requests, cancellation and loss of the SFTP stream.
The Windows application test SHA-256 was
`d78e1db80e7aba0e6c27996ef0c32bc39112fc64ac76abb2edf11301db85ea53`.

Fedora 44 ARM64 passed 178 active SFTP, Scheduler and archive engine tests.
Four opt-in archive tests passed against OpenSSH 10.2p1. The SFTP-only
browser fixture checked ZIP and TAR.BZ2 members, empty directories, payload
bytes, exclusions and a read-only preview. It preserved a destination that
appeared at publication. Source changes introduced after preview and during
staging stopped publication. The late-change case sent no hard-link request
and removed the private stage. The three Scheduler fixtures checked ZIP with
an optional local copy, TAR and TAR.BZ2, and vault-backed AES ZIP.

Formatting, workspace Clippy with `-D warnings` on both native targets and
the macOS ARM64 all-targets cross-check passed. The Fedora source manifest
and application test SHA-256 values were
`3a4e22297b7eadf61f08e7ce61efd9cac6d0e3483e12e66faef73ef5e4335530`
and `921d83f5cdb80c34969bd2ba91044ec4f4ed057d39e82a40a22c1a0ca6f6bd56`.
The private runs and development build artifacts were removed. The two
retained Windows package checksums matched.

### Reviewed local archives in the SFTP browser

The Local pane uses the same two-step archive form for ZIP, AES-256 ZIP and
TAR.BZ2. It uses an independent cancellable local worker. An existing local
listing remains usable for this action after the SSH connection closes.
The form holds masked password and confirmation fields for the active
operation and passes the password only to the local encoder on approval.

ZIP follows the pinned Java local layout. Each selected folder supplies
relative file names and each selected file supplies its basename. The review
rejects duplicate names and file/directory conflicts. TAR.BZ2 preserves
selected root names and empty folders. The browser shares the bounded native
encoder with the SFTP browser and Scheduler. It verifies local entries by
directory and file identity, reads reviewed file sizes, rescans before
publication and uses a private stage with atomic no-replace publication.

The preview binds a new sibling filename or absolute local destination to
its reviewed parent. It rejects destinations beneath selected paths and
checks physical identities of selected roots against destination ancestors.
Ordinary files and directories are eligible. A separate filesystem mount
that aliases an unscanned excluded subtree remains outside the identity walk.
The next checkpoints cover 7z creation. Reviewed extraction and native
dialog acceptance remain open.

Windows ARM64 passed 97 SFTP application tests, 74 Scheduler tests and
five archive engine tests. The browser fixture submitted a local AES ZIP
with no SSH worker, required matching passwords, verified the preview
left the destination absent, and decrypted the resulting file. The local
archive tests also checked Java-compatible ZIP paths, duplicate members,
TAR.BZ2 empty folders, changed sources and destination parents, cancellation
and preservation of a competing destination. The Windows test SHA-256 was
`8ed8278fd1eca4f921879ebccbde9caaf586305014c112e771f1186bf4fc3bb3`.

Fedora 44 ARM64 passed 187 active SFTP, Scheduler and archive engine tests.
The local worker also rejected a symlink substituted after review. The four
OpenSSH 10.2p1 fixtures confirmed the existing server-browser ZIP and
TAR.BZ2 workflow and the scheduled ZIP, AES ZIP, TAR and TAR.BZ2 workflows.
Formatting and workspace Clippy with `-D warnings` passed on both native
targets. The Fedora source manifest and application test SHA-256 values were
`81a9beeacdf55c4b7c252adae609a341075c108e64ee66868a6a1d4946950cb6`
and `7d226d71a39d5d769d0d82c2403fc4a8b79f441b8dd82be31c273e51b530e801`.
The macOS ARM64 all-targets cross-check passed. The private test runs and
development artifacts were removed. The two retained Windows package
checksums matched.

### Native 7z creation in the SFTP browser

The Local and Server panes offer unencrypted 7z in **Create archive…**.
Level 0 uses Copy and levels 1–9 use single-threaded LZMA2 with up to an
8 MiB dictionary. The encoder retains selected root names, empty files
and empty folders and uses the same reviewed exclusions as the other
formats. It buffers one checked input member at a time and enforces the
64 MiB output limit across seeks and header writes. Both panes use their
existing review, cancellation and no-replace publication paths.

This checkpoint pinned `sevenz-rust2` and `lzma-rust2` to 0.25.0 and enabled
the 7z writer's `compress` feature with LZMA `optimization`. The normal and
build dependency graphs placed both codecs in the desktop application for
the six Windows, Linux and macOS ARM64 and x64 targets. The SSH crate's graph
is separate. The package includes the codecs' Apache-2.0 license and notice.

Windows ARM64 passed 98 SFTP tests, 75 Scheduler tests and six archive
engine tests. Windows `bsdtar` with libarchive 3.8.9 listed the Copy and
LZMA2 archives and independently read the exact test payload from both.
The archive tests checked the codec methods, empty files and folders,
exclusions, input size, cancellation and seek-aware output limits.

Fedora 44 ARM64 passed 190 active tests and all four opt-in OpenSSH 10.2p1
fixtures. The Server pane fixture verified both 7z methods and the existing
read-only preview, stale-source, late-change and no-replace behaviors.
The Scheduler regression suite also passed after restoring its established
invalid-compression diagnostic. Formatting and workspace Clippy with
`-D warnings` passed on both native platforms.

The Windows packaging smoke reused three verified ARM64 executables in a
private run. The resulting ZIP contained both 7z notice files with the
exact workspace SHA-256 values. This check covers packaging inputs and
license inclusion. The tested 7z implementation ran in the application
test executable.

The Fedora source manifest SHA-256 was
`840828c3f6dca642249c21aaf9227aaa3662fa1146fc2388350e16d0bf3732f2`.
The Windows and Fedora application test SHA-256 values were
`03b3ea808b34b3df53b59c56e32b303778c1a620deb12ff85ed729023f61df8d`
and `43e44f569dd8ade12e7a8c80cc0fd1e200c0f1b02aae1a2f1d12c723ae446835`.
The macOS ARM64 all-targets cross-check passed. Private test runs and
development build artifacts were removed. The two retained Windows package
checksums matched.

### AES-256 7z creation in the Local pane

The Local pane's **7z (AES-256)** format uses the same reviewed path,
exclusions, size limits and atomic no-replace publication as plain 7z. The
masked password and its confirmation apply to one operation. Level 0 stores
members and levels 1–9 use LZMA2. The writer encrypts both member data and the
header with the member names, including when all selected members are empty
folders. This follows the pinned Java local archive action's `-mhe=on`.
Server browser and Scheduler archive requests reject the local-only variant.

The app builds a patched `sevenz-rust2` 0.25.0 with `compress` and `aes256`.
It derives one AES-256 key per archive with a fresh 16-byte salt and the
7-Zip work factor of `2^19`. The header and each nonempty file stream receive
separate random IVs. Password and key buffers zeroize on drop. The patch
removes the decoder's process-wide key cache and uses standard Rust LZMA.
The package includes the patched codec's license and the 7z notice.

Windows ARM64 passed 101 SFTP tests, 75 Scheduler tests and six archive
engine tests. Two opt-in OpenSSH fixtures remained ignored. The Local pane
message test verified password confirmation and a read-only preview with
SSH closed. [7-Zip 26.06][7zip-26-06] independently listed the exact members
for Copy, LZMA2 and directory-only archives with the synthetic Unicode
password. It extracted the expected file bytes and refused to list member
names with the wrong password. The test executable SHA-256 was
`b52a20fb6139fa9beff86ca765c559b0a769babaf99ec47dcc8ff8502418c791`.
Formatting, Windows workspace Clippy with `-D warnings`, and the macOS ARM64
all-targets cross-check passed.
The Windows packaging smoke reused verified binaries and matched all three
7z license and notice files to the workspace. The private test directory
and development artifacts were removed. Both retained Windows package
checksums matched. The checkout and target directory used 139 MiB and 42 MiB
respectively. The Fedora source transfer
did not run after automatic approval review rejected the full working-tree
payload. Native Fedora execution and graphical dialog acceptance remain open.

[7zip-26-06]: https://github.com/ip7z/7zip/releases/tag/26.06

### Reviewed ownership and permissions for Server archives

The Server archive dialog offers optional owner, group and a three-digit
octal mode. Pinned Java `SFTPManagerTab.java:5689–5697,5910–5927` offers an
owner/group field and defaults the mode to `644`. The Rust dialog keeps the
existing private default `600` and shows it in the preview. Explicit `644`
and `000` are both supported. SFTP-only accounts enter numeric IDs. The
original verified SSH command channel resolves names to numeric IDs for the
preview and verifies the same mapping before applying it.

The SFTP worker reads back every byte of the private archive. It sends
`FSETSTAT` on that same open handle, applying and checking ownership before
permissions. The policy guard runs before each write and before publication.
The worker checks the source manifest, stage and destination again before
using the existing no-replace hard-link request. Cleanup uses the latest
verified stage attributes, including when ownership succeeds and a later
permissions request fails. The server can deny a change or the final link
under its filesystem rules, in which case the operation reports the failure.

Windows ARM64 passed 103 SFTP application tests, 75 Scheduler tests, six
archive engine tests and 21 SFTP protocol tests. The dialog message tests
covered malformed fields, a numeric-only SSH route, a disconnected route,
and isolation from the Local pane. The protocol tests checked exact paired
UID/GID writes before mode, byte mismatch before any metadata request, and
policy revocation after an ownership update. Two opt-in OpenSSH application
fixtures were ignored on Windows. The application test SHA-256 was
`2d64caf742c4e207cd56b630345f270ea5bed4bf1cfa44e343a98bc684365f36`.
Formatting, Windows workspace Clippy with `-D warnings`, and the macOS ARM64
all-targets cross-check passed. Windows development artifacts and private
test profiles were removed. The checkout used 140 MiB with 42 MiB in
`target`, and both retained Windows package checksums matched.

Fedora 44 ARM64 verified the handle protocol independently and compiled the
five checksum-verified Rust SFTP modules in a private probe. OpenSSH 10.2p1
accepted an actual supplemental group change and exact modes `644` and `000`
with `lsetstat` disabled. It preserved a competing destination and denied an
`fsetstat` request without publication. The Rust probe also rejected changed
bytes and preserved the last verified attributes after the policy guard
revoked a second write. All six Rust probe cases passed. Its executable
SHA-256 was `158e601208f18991e2237e0472914d03bd166f5fb9bcf319000ca651bdc87bf6`.
The private Fedora source copies, test files, and executable were removed.

Run the native `openssh_browser_server_archive_` application fixtures with
an installed OpenSSH `sftp-server` on Linux or macOS. Named account lookup
through a real SSH command channel, a change to a different UID on a server
that permits it, and graphical dialog acceptance remain separate cases.

Pinned Java **Extract here** applies to one Server-pane archive and keeps
the archive on the server. Its Local pane offers archive creation. The
following checkpoint implements extraction on native server helper routes.
The OpenSSH 10.2p1 directory rename checks destination absence before a
separate `rename` call. The POSIX rename extension permits replacement.
Neither gives the required no-replace guarantee for a complete directory.
[Pinned Java SFTP action][java-server-extraction], [OpenSSH directory rename][sftp-directory-rename].

[java-server-extraction]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/src/main/java/de/kortty/ui/SFTPManagerTab.java#L5311-L5314
[sftp-directory-rename]: https://github.com/openssh/openssh-portable/blob/V_10_2_P1/sftp-server.c#L1232-L1254

### Reviewed server-side archive extraction

The Server pane's **Extract here…** reviews one regular archive on the
original authenticated SSH connection. The preview lists the exact source,
available sibling destination, checked members and link targets, and total
expanded bytes. Applying the review consumes it. The client requires the
same selection, verified route and policy, and checks the returned receipt.

The native `kortty-server-archive` helper handles ZIP, TAR, TAR.GZ, TAR.BZ2,
TAR.XZ and 7z with the Java suffix aliases. It limits input to 64 MiB and
expanded regular file data to 256 MiB. The manifest limits paths, depth and
member count. The decoder checks full file data and verifies compression
trailers. It rejects encrypted inputs, special files, ambiguous archive
paths and escaping links. A separate fixed SSH command accepts one bounded
request and returns one bounded reply. Only paths, metadata and the review
token cross the SSH channel for extraction.

The helper runs as the SSH account on Linux or macOS. It opens the source
through protected directory descriptors and binds approval to the archive
digest, identity, parent and exact destination. It creates an exclusive
private tree, verifies the staged entries, and publishes using the native
no-replace directory rename. A destination collision never triggers a
replacement or a different output name. A lost Apply receipt is reported
as uncertain. An interrupted server process can leave its private stage.
The helper must be deployed explicitly at the documented fixed path.

Fedora 44 ARM64 passed 16 native helper tests and Clippy with `-D warnings`.
They included all six formats, checked links, exact framed requests, a
changed archive, a target created after review, a collision at atomic
publication and cleanup refusing a substituted staging entry. Independent
GNU tar, Python ZIP and 7-Zip 26.06 fixtures also passed for the six archive
formats. They verified long Unicode names, links, exact file contents and
the unchanged source archive. The audited source manifest and helper
executable SHA-256 values for that run were
`d8e2c82dd722d13a840a0b559fe25a3cd854c2d110c7da6602a03a3ecd6741ce`
and `95bcec7c9ca03888dfd6c35ccec91fa9d437765c8ccf3c8a44059ef428490230`.
The source copies, development artifacts and private fixtures were removed.

Windows ARM64 passed 106 SFTP application tests, 75 Scheduler tests, six
archive engine tests and 11 helper library tests. The extraction cases
checked eligible siblings and sources, canonical receipts, and fragmented
and invalid SSH relay output. The application test executable SHA-256 was
`fde23de89db7214902fe2ba67207dc416be88edda2efb5ca73e81ac3dfc393ea`.
The library test for the isolated worker's fixed command tag passed.
Workspace Clippy with `-D warnings`, formatting and the macOS ARM64
all-targets cross-check for the client and helper passed. Both retained
Windows package checksums matched after development artifacts and private
test profiles were removed. The checkout used 140 MiB and `target` 42 MiB.

Native desktop acceptance still needs the helper deployed on a test server,
with both direct and isolated authenticated command routes. Verify the
dialog, denied and missing helper behavior, cancellation while applying,
and receipt loss. Run native macOS helper fixtures on its target filesystem.

### Optional destination-only removals in reviewed SFTP synchronization

Pinned Java `SFTPManagerTab.java:1854–2026` offers the option in both
directions and asks separately before permanent deletion. The native
comparison follows that approval flow and also accepts removal-only runs.
The review lists each destination path. The worker copies missing entries
first, then handles reviewed files and directories child-first. A directory
must be empty when removed. Excluded, blocked and private recovery paths
remain outside both transfer plans. One-use manual approval binds both compared
trees, the direction, and the original verified SSH worker. Scheduler
receipts and their copy proofs retain their original format.

Windows opens the exact child with delete access, checks the reviewed file
identity, and counts removal after a positive absence check. Unix uses
descriptor-relative no-replace moves to a private quarantine, checks the
moved identity, and can restore a failed removal without overwriting a new
entry at the original name. Server removal uses the OpenSSH POSIX rename
extension to move into a protected private quarantine. `REMOVE` and `RMDIR`
operate on the checked quarantine entry. Failure after a move reports its
recovery path. SFTP v3 has no stable identity to distinguish replacements
with identical reported attributes. Policy and source absence are checked
again at the mutation boundaries.

The exact Rust Unix local backend and anchored directory module passed three
Fedora 44 ARM64 tests in a reduced harness. The tests covered precise file
identity, nonempty folders and a replacement blocking no-replace rollback.
Four independent OpenSSH 10.2p1 cases used the checkout's SFTP modules.
They verified quarantine removal with a new live path, preservation of a
changed replacement and of a late child, and refusal without the POSIX
rename extension. The SFTP probe executable SHA-256 was
`d9333e03c2264d377a6ac3767b50196e27f5f9a70003c411ba2b84a0817e3b65`.
Both private Fedora runs and their build artifacts were removed.

Windows ARM64 passed 111 SFTP, 75 scheduler and six archive-engine tests,
with one ignored test in each of the first two groups. The SFTP tests cover
the second confirmation, the removal-only run and local browser refresh,
the native deletion handle, and recovery stages excluded as candidates or
roots in either direction. The application test executable SHA-256 was
`2523f494a5d3b3f4b12980a1e013488a290dca54d82e3363d227996d0109917a`.
Workspace Clippy with `-D warnings`, formatting, the diff check and the
macOS ARM64 all-targets cross-check passed. The private Windows run and
development build artifacts were removed. The checkout measured 141 MiB,
including 42 MiB under `target`. Both retained Windows ZIPs matched their
SHA-256 sidecars.

The two ignored `openssh_reviewed_sync_removals_` desktop-worker tests require
`SFTP_SERVER` on Unix. Native window acceptance still needs both directions,
the permanent-removal review, and recovery after losing a mutation receipt.

### Reviewed replacement of one existing local file

The manual folder comparison offers a separate byte-reviewed replacement for
one single-link regular local destination. It follows the pinned Java comparison rule
for a newer source or different size within a strictly shorter-than-two-second
age delta. Each source and destination is bounded to 4 GiB. The worker binds
the review to the exact relative path, both snapshots, SHA-256 values and the
original live verified SSH worker. The final dialog has its own one-use ticket.
Source-only copies, destination-only removals and scheduled copy receipts keep
their separate approvals.

The worker checks the current comparison, downloads into a private stage and
verifies the approved source digest before moving the original. Local
quarantine rechecks the exact file identity and approved destination digest.
It retains the original filename inside the private sibling directory. Final
publication requires an available destination. The target name is briefly
absent between those two filesystem operations. Concurrent arrivals are
preserved. Rollback revalidates the retained original and restores it only to
an available name. Cancellation or uncertain post-move proof preserves the
displaced entry and reports its exact recovery path. Successful replacement
also retains the original and refreshes the displayed destination folder.
At this verified checkpoint the final review stated that the new file used
private download permissions and the server modification time. The next
checkpoint addresses destination permissions. The pinned synchronization
action uses the transfer queue and `LocalFinalizer`, rather than the separate
in-place `SFTPSession.downloadFile` method. Its existing-target contract copies
POSIX read, write and execute bits or the Windows access ACL onto a new file;
it does not copy the original inode, ownership, extended attributes or access
and creation times. The precise reference trace is recorded below.

Fedora 44 ARM64 passed all eight native tests in a reduced harness containing
the checksum-pinned local removal, replacement and anchored-directory modules.
The harness used the same `LocalStamp` fields and Unix file identities, the
single-link check and RustCrypto SHA-256 through `pgp` 0.21.0. It covered
changed identity and bytes, cancellation before and after the move, exact
restore, occupied rollback names, retained backups and changed metadata on
the same inode. The private run and its development build artifacts were
removed.
The replacement module SHA-256 was
`00bef443d9489a9dd521717d871c5943601e750217d417bcc7fa813616f56969`.

Windows ARM64 passed 123 SFTP, 75 scheduler and six archive-engine tests,
with one ignored test in each of the first two groups. Six application and
transaction regressions exercised separate confirmation and stale tickets,
the complete checked download before displacement, a changed local target,
same-attribute changed server bytes, cancellation, denied final publication
with restoration, and a competing target with the original retained at the
reported recovery path. The application test executable SHA-256 was
`068d2d48f86c261458222cfc02d59177fa813b9d45a80f07cf2666598283d6d4`.
Native backup tests also verified the original's Windows write-sharing
restriction after quarantine. The backup directory allows write sharing for
the filesystem's internal rename operation and keeps its private ACL and
delete-sharing restriction. Workspace Clippy with `-D warnings`, formatting,
the diff check and the macOS ARM64 all-targets cross-check passed. The private
Windows run, both sharing-probe directories and development build artifacts
were removed. The checkout measured 141 MiB, including 42 MiB under `target`.
Both retained Windows ZIPs matched their SHA-256 sidecars.

Native desktop acceptance, a real OpenSSH application replacement, upload
replacement and multi-file selection remain open. Atomic compare-and-publish
and scheduled replacement approvals remain separate T3/Q1 gates.

### Reviewed local replacement permission parity

The pinned Java action calls `SftpTransferQueueHost.enqueueSyncDownload` at
`SFTPManagerTab.java:1957`. The
[queue handoff](https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/src/main/java/de/kortty/ui/sftp/SftpTransferQueueHost.java#L181-L185)
selects overwrite and modification-time preservation. The
[download worker](https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/src/main/java/de/kortty/core/sftp/transfer/SftpTransferQueue.java#L907-L911)
calls `PartTransfers.download` and then sets the server modification time.
[PartTransfers](https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/src/main/java/de/kortty/core/sftp/transfer/PartTransfers.java#L294-L319)
writes and syncs a private part before finalization.

For an existing local destination,
[LocalFinalizer](https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/src/main/java/de/kortty/core/sftp/transfer/LocalFinalizer.java#L156-L184)
copies the nine POSIX read, write and execute bits, or the Windows access ACL,
to the part before moving it over the target. It does not copy owner/group,
set-ID or sticky bits, extended attributes, access time or creation time.
For a new file, its separate rule uses the remote mode masked with `0755`,
or `0644` if absent, and the inherited folder ACL on Windows. The current
replacement checkpoint concerns existing local destinations only.

The Rust review binds the destination access permissions as well as the existing
identity and byte digest. The worker rechecks the captured permissions on the
held original and applies them to the checked staging file before publication.
Unsupported or denied application must take the existing rollback path. The
private stage remains protected while final file permissions are applied.

Fedora 44 ARM64 passed ten native tests in a reduced harness with the exact
local removal, recovery, POSIX-permission and anchored-directory modules.
They include retained executable bits, removal of special bits from new
content, refusal of changed original permissions, and publication through
the held descriptor after the final mode removes read access. Namespace
verification uses descriptor-relative file identities. The POSIX-permission
source SHA-256 was
`cc493891f78898b35d6f4a505d9816f774c766515f5296227387fd6ce59917aa`.
The anchored-directory source SHA-256 was
`0514805ebaa5f9da917fb3e25bfa328d831fb1a7375871aea0e3736b38a07ad5`.
The private native runs and their development artifacts were removed.

A private Windows probe used Microsoft OpenJDK 21.0.12. Its
`AclFileAttributeView.setAcl` produced explicit ACEs and an unprotected DACL
after both a private-part restriction and the final original-ACL copy.
An initialized descriptor applied through `SetKernelObjectSecurity` on the
held staged handle produced the same state, including from a protected
initial file. The audited
[OpenJDK 25 provider](https://github.com/openjdk/jdk25u/blob/master/src/java.base/windows/classes/sun/nio/fs/WindowsAclFileAttributeView.java#L163-L174)
uses the same legacy native DACL setter. Applying the observation to JDK 25
is a source-based inference. Native JDK 25 acceptance remains open.
The Rust snapshot retains the original raw ACL and inheritance control for
change detection. Final application converts inherited entries to explicit
entries and verifies the ordered ACL and unprotected state. Five native
ACL tests passed after this correction. All probe directories were removed.
Windows replacement currently requires an explicit access DACL. A null or
absent DACL is rejected before publication and the original is retained.

Windows ARM64 passed 124 SFTP, 75 scheduler, six archive-engine and six native
file tests. One SFTP test and one scheduler test remained ignored. The seven
replacement workflows include the original full-stage and recovery cases
and rejection of a changed Windows ACL with unchanged file identity and
ordinary file metadata. The successful application workflow checks the final
copied access ACL. The application test executable SHA-256 was
`36d3b1f27f1f46576becd86bbfccefa19675c01d123f5a67d0e9269a16d4f973`.
Windows workspace Clippy with `-D warnings`, formatting and the diff check
passed. The macOS ARM64 all-targets cross-check passed after accommodating
its 16-bit POSIX mode type. Failed publication restores private access on
the unpublished payload before removing it. A successful publication keeps
the reviewed final permissions.
The private Windows runs and owned ACL probes were removed. Development
artifacts were cleaned after all checks. The checkout measured 142 MiB,
including 42 MiB under `target`. Both retained Windows ZIPs matched their
SHA-256 sidecars. A final Fedora audit found no remaining directories under
the two used `/tmp/kortty-sync-*` test prefixes.

The following checkpoint extends this rule to new destinations in reviewed
browser folder downloads. Java uses remote POSIX permissions masked to
`0755`, with `0644` as its fallback, and the destination folder's inherited
ACL on Windows.

This keeps the existing reviewed backup flow: the original moves to its
private sibling recovery folder, leaving a short interval in which the public
name is absent. The final publication is an atomic no-replace operation. A
competing destination is preserved, and failed restoration reports the retained
original's exact path. This is not an atomic comparison and replacement of the
original directory entry.

Native desktop and real OpenSSH application replacement acceptance remain open.

### New destination permissions for reviewed browser folder downloads

Manual directional folder copies now carry an explicit browser origin.
For new local files, Unix applies the pinned `remoteMode & 0755` rule or the
`0644` fallback after the complete private part has been checked. Windows
derives effective file inheritance from the held actual destination parent
through `CreatePrivateObjectSecurityEx` and the effective token. Native tests
compare it with real `FILE_CREATE` results, including creator substitution,
object/container inheritance, inherit-only and no-propagation flags. Final
ACL application uses the same explicit, unprotected list as the preceding
permission checkpoint. Parent identity and raw access ACL/control are
rechecked before publication.

New local browser-copy directories use exclusive ordinary native creation.
They inherit the process umask and parent directory access rules. Private
staging and recovery directories keep their private creation methods. macOS
checks the empty private stage for extended ACLs before transferring payload.
The checked payload is published only to an available name, with the source
file's whole-second modification time. Failed publication restores private
access on the unpublished part before cleanup.

The pinned Java Scheduler uses `JobSchedulerRemoteSession.download`, with
ordinary `newOutputStream` and `createDirectories` behavior. Its file-permission
policy is a separate pending porting requirement. The explicit scheduler
origin retains the existing scheduled copy behavior and proof formats in
this browser-only checkpoint. Ordinary single-file browser downloads and
durable resume also need this finalization rule at their own entry points.

Fedora 44 ARM64 passed twelve native component tests in the checksum-pinned
reduced harness with umask `022`. They include `0644` fallback, privilege-bit
masking, explicit mode `000`, final file mode and identity checks, ordinary
directory inheritance compared with `fs::create_dir`, and existing-name and
symlink refusal. The private run and its build artifacts were removed.
Ten native Windows file tests passed after correcting their handle-sharing
and directory-traversal fixture setup. Windows ARM64 also passed 126 SFTP,
75 scheduler and six archive-engine tests. One SFTP and one scheduler test
remained ignored. The browser cases check masked privileged modes, source
modification time, effective parent ACL, complete private staging and a late
destination collision. The application test executable SHA-256 was
`902606a9fd12e2610a964205874b9019142628bedadacdff42e9616393a15277`.
Windows workspace Clippy with `-D warnings`, formatting, the diff check and
the macOS ARM64 all-targets cross-check passed. The new ignored Unix
`openssh_reviewed_folder_download_uses_file_modes_and_ordinary_directories`
fixture compiles and awaits a native application run with `SFTP_SERVER`.

The private Windows runs, native ACL probe and development build artifacts
were removed after all checks. The checkout measured 142 MiB, including
42 MiB under `target`. Both retained Windows ZIPs matched their SHA-256
sidecars. A final Fedora audit found no remaining directories under the
used browser-download and earlier permission/replacement test prefixes.
Native desktop acceptance and the ordinary-download, durable-resume and
Scheduler permission requirements remain open.

### Ordinary single-file download permissions and checkpoint ownership

New ordinary browser file downloads now share the final access helper with
reviewed folder synchronization. Unix applies `remoteMode & 0755`, with the
existing `0644` fallback for absent permissions. Windows derives the final
file ACL from the actual destination parent. The complete payload remains
private until finalization, and final publication still requires an absent
destination. Ordinary Java queue downloads default to not preserving source
modification time; this change does not add synchronization's timestamp rule
to that path. Downloads without a verified host use the same completion path
but cannot retain an interrupted transfer.

Each resume attempt derives final access afresh, including after application
restart. Existing receipt fields already retain the remote permissions, so
this browser change needs no receipt format migration. Dedicated Windows
creation and reopening methods add `WRITE_DAC` while preserving the existing
sharing, identity and private-access checks. A failed lock contender never
arms permission restoration. Otherwise its destructor could change the live
owner's file while that owner finishes publication. Failed finalization
restores private access on a retained checkpoint through its held handle.

Completed-receipt reconciliation now inspects identity and length without
requiring content-read access. Unix uses no-follow, parent-relative metadata;
Windows uses a metadata-only handle. This permits cleanup of an exact empty
stage after publication even when final file permissions deny reading the
contents. An unverifiable file never gains additional access for inspection.

Windows ARM64 workspace Clippy passed. Thirteen native file tests passed,
including old checkpoint reopening, lock contention, ACL restoration and
metadata inspection of a file whose contents are denied. The application
passed 130 SFTP tests, 75 Scheduler tests and six archive tests; one SFTP and
one Scheduler integration test remained ignored. The four new browser tests
use the normal queued Copy and Find/Resume actions. They cover private
staging, source modes `0751` and `06777`, a late destination arrival, operation
without a verified host, and restart after a five-byte checkpoint. The
Windows restart case changes parent inheritance between attempts and compares
the completed file with ordinary operating-system file creation. The test
executable SHA-256 was
`4ece474d16e79e1a7e3dd5f150f042ae27e4010f22c4acd49ab078947ae18b4f`.
Formatting, the diff check and the macOS ARM64 all-targets cross-check passed.

A reduced native Fedora ARM64 component probe passed all nine tests on
2026-11-11 after explicit approval for its source transfer. It used ten
pinned source files, exact identity/stamp extracts and labelled fixture/hash
adapters. The first run exposed an incorrect module layout in the harness;
the corrected run kept every source hash and the same 25-file transfer scope.
The compiler was Fedora Rust 1.99.1 and the unprivileged test UID was 1000.
Coverage includes ordinary directory creation, exact stage publication,
reopened checkpoint access, losing lock contenders and completed files with
no content-read permission. This is filesystem component evidence, not a
native application, real-server SFTP or cryptography interoperability result.
The artifact manifest SHA-256 was
`f951319869742df048290686906c8e027facd4ca6fdbde282ab5d2409fcf47ba`;
the payload checksum file SHA-256 was
`b04c8be2b36d3656af4fdce574f8ca8b5e5e494d3e181f436a449eb9fcab6451`.
The generated harness Cargo.lock SHA-256 was
`91197921862fcdb2b6e2488d933d520f4cf071006f8851da67b2e2b6457268fc`.
Cargo cleaned its 244.8 MiB development output. The owned guest run directory
and recorded shell were removed and their absence independently checked.

All Windows test runs exited and removed their private temporary directories.
Native and macOS cross-target development outputs were cleaned after their
checks completed. The checkout measured 143 MiB, including 42 MiB under
`target`. The two previously retained Windows packages still match their
SHA-256 sidecars; they are not packages of this new implementation. The
bounded Fedora source bundle was subsequently used only for the approved
component probe described above.

The ordinary folder queue implementation is described below. Scheduler
permissions remain separate implementation work. Native desktop and real
OpenSSH acceptance of the new single-file completion path remain open.

### Ordinary SFTP folder queue and selective retry

The SFTP Manager now expands ordinary upload and download folders into
independent child transfers. An existing ordinary directory becomes a merge
destination. Each file has its own conflict decision and complete-or-absent
publication. Newly created directories use ordinary local or server creation
rules; cancellation keeps created directories and completed files. This
matches the pinned Java queue contract, rather than imposing whole-tree
rollback. Browser child downloads use the file access rules described above.
The explicit queued-folder operation leaves non-queue copies, Scheduler and
privileged whole-tree transfers on their existing paths.

Parent rows aggregate nested child outcomes without occupying transfer
channels. Retrying a failed or cancelled folder queues only unfinished
children. A retained file offer keeps its original child ID across reconnect
to the same verified peer, and resume completion updates every ancestor.
History pruning retains whole groups and pins groups with live partial offers.
Discarding an offer releases that pin and permits a fresh child retry. This
is per-child state in the open manager, not a persisted whole-folder journal.
Late expansion replies cannot add children after cancellation. Child results
do not navigate a visible pane into a nested destination; queue completion
refreshes the currently displayed paths, including newly created empty folders.

Each root admits at most 20,000 descendants, 32 nested directory levels and
1 TiB of listed file sizes. A shared execution budget also checks actual file
offsets, preventing source growth from bypassing the byte bound. Retry and
resume reuse the same child's highest attempted offset. Direct listings are
bounded by entry count and 2 MiB of names; case and Unicode aliases are
rejected explicitly. Local metadata-only child classification allows content
read failures to be reported on individual files. The later source-link
checkpoint extends this queue to links that resolve to regular files.

Windows ARM64 workspace Clippy passed after the changes. The final application
test binary passed 155 SFTP tests, 75 Scheduler tests and six archive tests;
one SFTP and one Scheduler integration test remained ignored. Thirteen native
file tests also passed. The first native-file run under the restricted token
reported access errors; the same synthetic private fixtures passed under the
normal user token. The final executable SHA-256 was
`24646c7fd635a612da0031052395e5d3d1a845c3c83e37dcf687f1a115188d89`.
Four protocol-double UI workflows cover merging and per-file Skip, selective
retry, interruption after a five-byte prefix followed by reconnect/resume,
and cancellation preserving completed children. Thirteen new workbench tests
cover aggregate state, child lanes, retries, cancellation races, bounds and
history retention. Component tests cover folder creation, merge races,
metadata access, aliases and execution byte limits. An initial alias test
exposed mutation on a rejected duplicate; the final implementation preserves
the original name. The resumed-prefix fixture respects Windows byte locks.

The macOS ARM64 all-targets compilation check, formatting and final diff check
passed. Native and cross-target development outputs were cleaned after every
build finished. All owned Windows run directories were removed; both Fedora
component runs left no guest copies or owned shells. The prepared local source
bundle was removed after retaining compact verification records. The checkout
measured 143 MiB, including 42 MiB under `target`. The two previously retained
Windows package archives still match their SHA-256 sidecars; they predate this
checkpoint. Existing SSH test credentials were preserved.

Native desktop acceptance, real OpenSSH folder workflows, parallel-channel
acceptance and native Unix folder tests remain separate gates. The Fedora
component result above covers single-file filesystem operations and does not
establish folder-queue interoperability. The full porting goal remains open.

### Scheduler ordinary download creation and approval renewal

Reviewed Scheduler folder-copy plans now create new local directories with
ordinary destination inheritance. Each new downloaded file keeps private
staging, then receives the access permissions of an ordinary locally created
file in its actual destination parent. The separate ordinary scheduled
single-file download uses the same policy. Remote mode bits do not determine
Scheduler destination access. Scheduler downloads retain their local write
time, following pinned Java `JobSchedulerRemoteSession.java:484–504`.
Browser synchronization retains its own mode and timestamp rules.

Windows derives and applies the native file ACL, including inherited ACE flags
and DACL control. Tests compare the complete ACL with ordinary control files
for legacy and automatic parent inheritance, token-default fallback in both
models, and final permissions that deny reopening the contents. Native testing
identified the required automatic-inheritance request/control combination.
The original Browser ACL-list setter still has separate behavior.

Unix samples ordinary creation through an empty `0666` descriptor in the actual
parent without querying or changing process-wide umask. Linux prefers an
anonymous `O_TMPFILE`. Named fallbacks move into a private quarantine and
verify the moved identity before deletion. A foreign entry is restored without
replacement or preserved with a reported recovery path. Parent identity,
ownership, mode and inheritance are rechecked before final publication. Linux
copies the native access ACL and applies the sampled mode. macOS has bounded
native ACL capture/application and clears inherited ACLs from newly created
private staging objects before receiving data. Failed macOS finalization also
removes extended ACLs while restoring private access.

The new `kortty-scheduled-sftp-download-creation-v1` binding invalidates previous
ordinary one-use and recurring download approvals. Their stored JSON formats
remain unchanged. Tests cover no-predecessor, Gate and Chain receipts, old
standing scopes, fresh claims and the persisted-job editor review flow. Upload
and privileged scopes are checked against the exact prior serialized domains.
The Scheduler review explains the normal local creation and timestamp rules.

Windows ARM64 workspace Clippy and the macOS ARM64 all-targets compilation
check passed. Eighteen native file tests passed. The application passed 159
SFTP tests, 79 Scheduler tests and six archive tests; one SFTP and one Scheduler
integration test remained ignored. The new application checks include private
staging, ordinary final access, cancellation, a late destination, ignored remote
modes `0000` and `07777`, and fresh approval after reopening a saved job. The
application test executable SHA-256 was
`98af4f4a5f45df805d02b37c37a7b31e6969295e1df8737e58e46f4899dbf783`.

A reduced Linux filesystem harness is prepared with 15 pinned source snapshots
and 32 transferred files (140,713 bytes). Automatic approval review rejected
this expanded source transfer because the prior explicit consent covered the
earlier ten-source payload. No new source was transferred. Explicit consent for
the prepared scope is pending. The artifact manifest SHA-256 is
`4b0f62c858c3c46ca1845e35e9b50fe7aea6d26858653f7be660a1682e644e27`;
the payload checksum-file SHA-256 is
`32d96e36229b02fa8b14c0966c8a33e21289af396bd7cf329659cb33f537454b`.
The harness labels exact definition extracts, synthetic fixtures and its hash
adapter; it does not establish SFTP, SSH, Scheduler UI or cryptography-provider
interoperability. The unchanged source modules and harness tests passed a local
Linux x86-64 all-targets cross-check using the shared Cargo target directory.
The temporary harness Cargo.lock SHA-256 was
`6745a95df6b7da54e98799f756951f78706ee597e83fd3f075addc2e832e4a24`.
This checks Linux compilation, not native filesystem behavior. Final Windows
workspace Clippy and formatting passed after the source was frozen.

All owned Windows test and cross-check run directories were removed. Native,
macOS and Linux cross-target development outputs were cleaned after the builds
finished (2.8 GiB removed). The checkout measured 144 MiB, including 42 MiB in
`target`. Both previously retained Windows archives still match their checksum
sidecars and predate this checkpoint. Existing SSH credentials and the bounded
prepared Scheduler component payload remain for the pending transfer decision.

The legacy whole-folder Scheduler download is described in the next checkpoint.
End-to-end staging under owner-denying umasks remains open: the existing private
directory builder cannot reopen a newly created mode-`000` directory. The new
anonymous access sampler can handle a mode-`000` file through its held descriptor.
Native Unix filesystem execution, native macOS ACL acceptance, real OpenSSH
Scheduler workflows and desktop review acceptance remain separate gates. The
full porting goal remains open.

### Legacy Scheduler folder download inheritance and cancellation

Ordinary scheduled folder downloads to a new target now build the payload tree
inside a separate owner-only outer directory. The payload root receives the
ordinary directory permissions derived from the actual destination parent.
Nested directories and files then use native ordinary creation within that
root. This preserves normal inheritance down the complete tree while the outer
directory keeps all downloaded bytes private during transfer. The source file
mode and source modification time do not become Scheduler destination metadata.

The Windows directory derivation retains native ordered ACEs, inheritance flags
and control for legacy, automatic and token-default ACLs. Tests compare root,
nested and empty directories and files against a tree created normally by the
operating system. Unix directory capture samples an empty ordinary `0777`
creation, preserving ownership, SGID and ACL metadata. Linux captures directory
default ACLs separately; matching inherited metadata is left in place to avoid
clearing inherited SGID. Named sampling probes use the identity-checked private
quarantine and preserve foreign entries. macOS applies inherited ACLs inside
the verified private barrier.

Complete publication moves the held payload to a vacant destination without
replacement. The worker rechecks the parent, inheritance, source name, private
barrier and cancellation, including before Windows sharing-error retries. A
late collision preserves the arrival. Failure cleanup recurses only when the
held unpublished payload still occupies its expected name. A substituted entry
is preserved. Successful publication removes only the empty outer wrapper;
cleanup failure can be reported independently of confirmed publication.
Scheduler supervision retains both confirmed and uncertain publication outcomes
when cancellation wins the result race. Privileged trees and non-queue browser
copies retain their separate paths.

The first integrated cancellation test exposed a spin: standard `write_all`
retried a permanently interrupted local SFTP writer. The exact stuck Windows
test process was stopped and its private run was removed. The SFTP core now
propagates the local interruption, closes the remote file handle and validates
local write counts. Ordinary uploads also propagate a cancelled local read.
Protocol tests check the remote close, reusable session, partial writes and
invalid counts. Twenty-four SFTP crate tests passed, along with 21 native
Windows file tests. The integrated application verified ordinary inheritance,
the private barrier during each read, empty folders, cancellation, a late
destination and a changed parent. The Linux component cross-check compiled
current Unix primitives and their tests from a private local copy; no source
was sent to Fedora.

The final Windows ARM64 app test executable passed 163 SFTP tests, 79 Scheduler
tests, six archive tests and one targeted Scheduler cancellation outcome test.
One SFTP and one Scheduler integration test remained ignored. The targeted test
includes confirmed and uncertain local publication when the supervisor aborts
and joins the worker. Workspace Clippy and formatting passed. The final test
executable SHA-256 was
`ee01404b872d122e00c9b1eaf556e3e312cff16b7f0460e1472a38a693bd60e9`.
The final macOS ARM64 app all-targets compilation passed. The reduced Linux
x86-64 component all-targets cross-check passed using the current Unix source
files and the same labelled fixture adapters. Its temporary Cargo.lock SHA-256
was `6745a95df6b7da54e98799f756951f78706ee597e83fd3f075addc2e832e4a24`.

All Windows runs exited, their private directories were removed and 3.0 GiB
of native and cross-target development output was cleaned. Retained Windows
package checksums still match; those archives predate this checkpoint. The
source-transfer review bundle and existing SSH access were preserved.

Native Unix filesystem execution, native macOS ACL behavior and real OpenSSH
whole-folder Scheduler workflows remain acceptance gates. This checkpoint
covers new-target publication; existing-target Scheduler updates still require
the remaining reviewed replacement work. Owner-denying umasks can prevent the
current directory builder from reopening a new directory, and remain open.
The earlier manifest-pinned Fedora payload remains frozen for its pending
approval. Five source files now differ from that frozen snapshot; its
current-source guard requires new pins before testing the current tree. The
full porting goal remains active.

### Regular-file source links in the SFTP Manager

The browser's copy selection accepts symbolic links. Selected links and links
found during folder expansion resolve to canonical regular source paths. The
link's original name remains the destination name, including when its target
lives outside the selected folder. Directory links, broken links, loops and
special targets are skipped. A skipped direct link records an outcome in the
queue and lets the next item start. This applies the pinned Java queue engine's
file-link behavior to directly selected remote links as well as folder children.

The resolver checks local parent identity, link metadata, link value and target
metadata before admitting a regular source. Remote resolution checks the
canonical path, followed attributes and the original link's attributes again.
It preserves raw remote names and bounds each resolved remote path to 16 KiB.
The folder listing budget includes the resolved path bytes. Ordinary file
transfer checks apply when the resolved file is read. Existing destination
links continue through the ordinary conflict choices with replacement disabled.

Expanded children store the resolved source path for retry. A fresh direct
queue attempt resolves its link again. A conflict decision keeps the resolved
path from the attempt that produced it. Durable downloads save that same path
and recheck the retained prefix on resume. This lets the saved download finish
from its original source after the alias is retargeted.

The Windows ARM64 run executed 169 SFTP tests, 79 Scheduler tests, six archive
tests and one targeted cancellation-outcome test. All executed checks passed.
The four unconditional browser link workflows verify direct and nested links,
queue continuation after a broken link, conflict retargeting and durable resume
after restart. Resolver tests cover a malformed canonical path and retargeting
with identical file metadata. The folder expansion fixture also verifies raw
canonical target bytes and skips directory, looping, broken and special links.
Two additional workflow fixtures returned early because native Windows link
creation failed with privilege error 1314. They cover local upload and keeping
an existing local destination symlink. One SFTP and one Scheduler integration
test remained ignored. The final application test executable SHA-256 was
`656feb13b994d4abd1ad1277bc1782b37eff019ad0cf4d9d95d0a9a73ac2b948`.
The subsequent macOS ARM64 app all-targets compilation included these changes
and passed.

Native Unix link execution and real OpenSSH link workflows remain acceptance
gates. The Windows local-link fixtures need permission to create symbolic
links. The separate Scheduler creation component source-transfer approval remains
pending. The approved 25-file download component harness already ran on Fedora
and is recorded under `target/verification/20261111-download-components/`.
The full porting goal remains active.

### Initial terminal size for local shells and SSH

The Terminal settings page and the Connection Manager expose initial columns
and rows. Each unset profile field inherits its global value. Each unset global
field uses the emulation's preferred grid. Valid overrides are 40–500 columns
and 10–200 rows, matching the Java settings controls. Older profiles and window
settings without the fields retain the emulation defaults.

Local PTY creation uses the resolved grid even if the pane is measured while
the worker is starting. The worker applies the measured viewport once the PTY
opens. SSH resolves the grid at dial time and passes it to the PTY request and
the attached session. Explicit reconnects continue from the current pane grid.
Changes to the size settings apply to subsequent launches. Serial and IBM
retain their transport-specific sizing.

Java connection XML schema 2 reads and writes `terminalColumns` and
`terminalRows`. Java shell connectors read the values independently of the
appearance inheritance flag, so an export with only grid settings keeps
appearance inheritance enabled. The separate Java project interchange is
described below.

Windows ARM64 passed workspace Clippy with `-D warnings` and 269 selected
application tests. Two opt-in integration tests remained ignored. The tests
cover settings persistence and validation, per-axis inheritance, saved local
launch, the SSH dial and control-tab anchor, and the Java XML round trip. The
application test executable SHA-256 was
`e8152a4a7abb55ea5f411fb90beaefbc7a6ed2d4d1816c6b1654efbfdeac3c3c`.
The macOS ARM64 app all-targets compilation, formatting and diff checks passed.

### Java project initial terminal dimensions

The Java project reader takes the initial columns and rows from each linked
local-shell or SSH catalogue connection. Split leaves with a separate
connection ID use that entry. Missing dimensions in a linked Java entry use
Java's 80-column and 24-row defaults. Unsaved local shells use Rust launch
defaults. Invalid catalogue dimensions produce an import note and the affected
pane uses inherited sizing. Native project layouts carry both optional axes,
and existing layouts without the field deserialize normally.

Restored local shells use the carried dimensions when creating their PTY. A
restored SSH pane uses them on its first sign-in even if the window has already
measured the disconnected pane. The connected session applies the measured
viewport, and subsequent reconnects use that geometry. Saving the project
again retains the configured dimensions independently of live resizing.

Java export verifies the route's unique catalogue match and then verifies the
required axes. It shows size conflicts in the preview and rechecks each bound
pane before publishing. An explicitly sized local shell can use Java's default
route when its size matches that default or a matching catalogue entry. When
exporting a saved Rust SSH route, matching profile settings fill dimensions
absent from the project. Native restoration reads current saved-profile
preferences for those inherited values.

Windows ARM64 workspace Clippy with `-D warnings` and 290 selected application
tests passed. The application tests cover Java import and export, native and
portable layout round trips, catalogue changes after preview, and the first
SSH dial after a restored pane was measured. Two opt-in integration tests
remained ignored. The application test executable had SHA-256
`c6d0544a9c73622daf9a4d50fe60e28431ca1e9457d2d11bdc060669e4d05ef3`.
The macOS ARM64 app all-targets compilation passed.

The Windows Rust SSH test connected to the pinned Fedora host and read
`stty size` from its interactive shell. The server reported 132 columns and
43 rows on startup and 113 columns and 41 rows after a resize request. The
same session remained usable after auxiliary exec completion and cancellation.
Native graphical project restoration and a round trip through the pinned Java
application remain separate acceptance checks.

### Reviewed hybrid skill selection in Terminal AI

Automatic selection retains the local ranking and explicit connection pins.
With at least one eligible candidate, a chat review offers classification when
there are zero or one strong local matches or exactly two strong matches
within two points. A strong match has a score of at least four. The classifier
receives the masked current question and initial terminal selection when
present. Eligible unpinned skills contribute IDs, scopes and masked names,
descriptions and tags. It has an eight-second deadline, a 128-token output
ceiling and a 64 KiB user payload limit.

The classification review shows the exact messages and a separate quota
forecast for configured budgets. Confirmation dispatches one request through
the selected chat profile. The result is checked against the eligible IDs,
deduplicated and ordered by local rank. Up to two selected skills follow the
explicit pins in the updated main preview. An empty, invalid or failed
classification leaves the local guidance available. Canceling drops the
pending worker and invalidates a displayed quota forecast. Configuration and
source changes invalidate active reviews. The main chat retains its own
approval and refreshes its quota after classification.

Windows ARM64 workspace Clippy with `-D warnings` and 406 selected application
tests passed. Loopback fixtures checked the exact masked classification messages,
the separate main confirmation, quota refresh and one ledger entry per request.
They also exercised malformed and ineligible replies, cancellation, stale
completion, and a persisted catalogue edit before dispatch. The chat preview
was restored after classification when the rule preview had been open. Two
opt-in managed runtime tests remained ignored. The macOS ARM64 app all-targets
compilation passed. Live providers, native graphical
acceptance, agent-style hybrid selection and reviewed quotas on other consumers
remain separate gates.

### Quota forecasts for reviewed AI requests

Terminal Chat, skill classification, manual questions, Snippet AI Assist and
journal answers share the read-only forecast and freshness rules. The forecast
applies the Java default character and word estimate to the reviewed messages,
including chat history, memory and retrieved excerpts when selected. It uses
the actual dispatch output limit. The manual and journal answer limit is at
most 1,024 output tokens. Native LM Studio structured Snippet Assist and
journal answers bind the JSON system instruction before review, estimate and
dispatch. The adapter preserves that instruction once. A journal answer
rejects a review bound to a different provider instruction before HTTP.

The estimate, output ceiling and pending reservations feed the projected
remaining quota and warning thresholds. Usage accounting records provider
usage, and reports incomplete usage when applicable. The send action reads a
fresh forecast when the current forecast is stale and waits for a new
confirmation. Results from superseded reviews are ignored. A failed dispatched
request refreshes the forecast before a retry where the review is retained.

Windows ARM64 workspace Clippy with `-D warnings` and 475 selected tests passed.
The loopback fixtures verified the actual messages and output caps, stale
forecast refresh with a new approval, late-result rejection, original-profile
reruns and provider-reported usage. The native LM Studio journal fixture
verified the reviewed system and user text on the wire and rejected mismatched
provider binding before HTTP. Two opt-in managed runtime tests remained ignored.
The macOS ARM64 app all-targets compilation passed.
Model-specific tokenizers, graphical quota meters, remaining request surfaces,
live-provider interoperability and native graphical acceptance remain open.

### Reviewed standalone Terminal Agent Ask requests

Standalone Ask opens a masked request preview before model dispatch. The menu
and setup flow can capture an explicit terminal selection. The direct Ask alias
starts the preview with the submitted question and skips setup. The preview
binds the live terminal stream, privacy mode, saved profile, assigned skills,
and effective system and user text. A replaced stream or changed profile,
policy, credential mask, or skill assignment invalidates the review.

The prompt includes explicit Agent connection skills and locally ranked Agent
or shared skills. Ambiguous relevance can open a separate classifier review
for the masked question, optional masked selection, and eligible metadata. The
classifier result opens a new Ask preview and approval. The quota forecast
uses the exact reviewed messages and the profile's effective output cap, at
most 2,048 tokens. The classifier uses its separate cap of up to 128 tokens.
Private panes omit usage forecasts and ledger entries.

The approved Ask system stays fixed through format compatibility and the
existing response repair. Repair uses the original approved user input with
the local format instruction and must fit the same context limit. Both paths
recheck dispatch authority. Ask accepts a completed or blocked advisory decision.

The selected Windows Terminal Agent and AI suites passed 251 tests. The
loopback fixtures checked exact preview dispatch, approval invalidation,
private usage handling, response repair and provider format compatibility.
Four opt-in integration tests and native graphical acceptance remain open.

### Reviewed Terminal Agent Plan requests

Questions, Options and Report each open the shared masked model request review.
Confirming the final answer or choosing an approach prepares the next preview.
Every dispatch requires that stage's current approval. The preview shows the
exact system and user text and a quota estimate for the first request using
the selected profile's output cap, at most 2,048 tokens.

The local Agent skill selection uses the masked goal and, in later stages,
accepted questions, answers and the chosen approach. Assigned terminal skills
remain in the main prompt. A separately approved classifier can select skills
from eligible metadata and that same masked context. Classification omits the
terminal route, changes the preview together with its quota forecast, and
requires a fresh stage approval. Private panes omit usage forecasts and ledger
entries.

Dispatch, compatibility retries and response acceptance recheck the reviewed
route, profile, policy, saved skill assignments and credential mask. A stopped
terminal revokes an open review or cancels its in-flight request. Questions
keeps its single-response rule. Options and Report can each repair one invalid
reply within the context limit. Saved plans and Implement retain their existing
separate workflow.

The selected Windows suites passed 255 tests. The loopback fixtures verified
all three exact previews, stage-bound approval, masked classification and
evidence, configured quota caps, context-bounded repair and route revocation.
Four opt-in integration tests and native graphical acceptance remain open.

### Reviewed standalone Terminal Agent Run requests

The environment probe and each command keep their own execution reviews. A
successful probe, the last command in an approved batch, an answered
clarification, and a completed password-sudo operation prepare a masked model
preview. Resuming a paused step prepares the same review. The turn advances
only on explicit model approval. A run with reviewed evidence at the eight-turn
limit opens a separate advisory final-response review.

Connection skills and local relevance use the masked goal, accepted plan and
answered clarifications. An optional classifier has its own review and quota
forecast. It uses eligible metadata and this masked user context. Classification
refreshes the main request and its forecast and requires a new approval. The
preview counts the exact request and the effective output cap, at most 2,048
tokens. Private panes omit usage forecasts and ledger entries.

Run checks the source stream, profile, managed policy, saved pins, credential
mask and exact target identity at review and dispatch. The worker checks the
original local directory or authenticated SSH connection before each attempt.
It reads the SSH session actor's latest reported directory and confirmed `cd`
evidence before sending. Changes become observable when the shell reports
them. Decision reviews also bind the sudo capabilities. Previously learned
passwords stay in the run mask before escaping historical evidence.
Compatibility retries reuse the approved text. The final response can repair
one invalid or token-limited reply if the exact repair fits the reviewed
context limit.

The selected Windows ARM64 workflow suites passed 259 tests with four opt-in
tests left out. The loopback tests cover the reviewed request for each round,
independent command approvals, masked classification and evidence, refreshed
quota, and a final repair whose approval expires when the source is replaced.
The SSH directory comparison passed its targeted regression. Windows workspace
Clippy and macOS ARM64 application all-targets compilation passed. Native
dialog and configured provider acceptance remain open.

### Reviewed model requests for interactive Swarm targets

The focused member in an interactive Swarm shows its exact masked Advisory,
Agent decision, or final response request. Each member uses its own connection
skill assignments and approval. An optional classifier has a separate preview
and quota forecast. Its input uses the masked goal and approved user context,
with eligible skill metadata. Target labels and command output stay in the
reviewed main request. Classification refreshes that request and its forecast
before it can be sent. The probe and each proposed command have their own
execution reviews.

Live members recheck the original pane, stream, profile, and skill pins. Saved
members bind the reviewed route and directory where applicable, SSH host keys
for every selected hop, registered identity settings, and the credential mask
and vault revision. Fresh saved Advisory reviews can use an unpinned route
without opening an SSH connection. A saved follow-up preserves its combined
credential mask and sends the new approved turn. Each model or classifier
dispatch and model retry rechecks the review. The queue retains the member's
own slot and isolates its outcomes from the other targets. A busy model worker
leaves the exact preview available and refreshes its quota for renewed approval.

The Windows ARM64 workflow selection passed 262 tests. The new loopback cases
exercise two live targets with different pins, separate classifier and model
approval, busy workers, exact request bytes, the approved probe and command
transitions, and a saved follow-up that rejects changed pins or profile
settings. The saved advisory fixture sent no SSH traffic. A targeted test
also checked revocation after a jump-host key change. Windows workspace Clippy
and macOS ARM64 application all-targets compilation passed. Native window
acceptance, the opt-in saved OpenSSH workflow, and a configured provider run
remain open.

### Reviewed AI comparison of completed Swarm targets

After an interactive Advisory or Agent round completes, the comparison review
shows the exact masked goal, ordered target conclusions, errors or bounded
transcript excerpts, and the quota forecast. The prompt can include global
and organization AI memory under the current settings. The combined mask uses
the known credentials of every captured target before shortening evidence.
The model receives one reviewed text request, with at most 2,048 output tokens.
The comparison validates a Markdown table with one row per target and an
English `Errors` final column. Failed or cancelled comparisons retain the
deterministic local report. Accepted Markdown is included in copied and saved
reports. Model-generated link, image and HTML delimiters are escaped. The
provider call is recorded through the profile usage account.

A saved follow-up compares the current turn and its current results. An
accepted comparison attaches to the exact latest saved turn under the store
lock. It preserves the turn count and updates the content fingerprint. A
changed record, profile, managed policy, or included memory invalidates the
review. Legacy turns can be loaded without a comparison.

The Windows ARM64 selection passed 271 tests. Loopback tests compare the
reviewed messages with the actual request, check quota and usage, reject a
cancelled review, and exercise local fallback and a new round. The saved
follow-up fixture checks that earlier turns stay outside the request and the
current turn retains its accepted comparison. The storage tests verify exact
content revisions and backwards compatibility. Windows workspace Clippy and
macOS ARM64 application all-targets compilation passed. Native Swarm window
acceptance and a run against a configured provider remain open.

### Reviewed skills for one-use Scheduler model proposals

The proposal preview uses scoped local skill guidance and the masked job goal.
Explicit connection skills are labelled for their SSH target. Automatic Agent
and shared skills follow the pins. When the Java relevance threshold leaves
classification useful, an optional preview shows the masked goal and eligible
skill metadata if the complete request fits the profile context limit. The
classifier and the main proposal show separate quota
forecasts using their effective output limits, capped at 128 and 4,096 tokens.

Classification is a separate model approval. A valid result replaces the
automatic skill guidance and opens a new proposal preview with a new approval
identifier. Cancellation and unusable replies return to the locally selected
guidance. The proposal, compatibility retry, and eventual command approval
check the current graph, draft, model profile, saved skills, SSH routes and
host keys. A graph revision change discards an open one-use review. The HTTP
guard rechecks persisted authority and policy. The final command confirmation
identifies the displayed result and binds its exact commands and SSH targets
to one use.

The Windows application tests cover the exact classifier and proposal payloads,
two ledger entries, the refreshed preview, command confirmation, and refusal
before dispatch when saved connection skills change. The loopback fixture
keeps the SSH socket untouched. The selected Scheduler and AI suites passed
208 tests with one opt-in OpenSSH test ignored.

### Reviewed skills for per-firing Agent and iterative Swarm

New reviews select skills locally using the masked job goal. Ambiguous local
relevance can open a separate classifier review for all selected targets. Its
request contains the masked goal and eligible skill metadata. The classifier
has its own quota forecast. Its result updates all target previews together and
requires a fresh standing approval. Cancellation and unusable results keep the
local selection.

The standing preview shows the exact system instructions and initial input for
each SSH target. Connection pins are used only on their assigned targets. The
aggregate quota forecast estimates initial input and sums the effective output
caps across the targets. The Swarm template identifies the fields filled by the
initial SSH probe. Probe results and later rounds can change usage.

Versioned generation receipts bind the effective system, exact masked guidance,
target, profile revision, initial input and workflow limits. Classified reviews
also bind the ordered skill IDs. Firings reuse these IDs with the saved exact
guidance. Saved local-guidance and legacy receipts keep their formats and
fingerprints. A change to the reviewed text or classified IDs requires a new
review. The Scheduler checks guidance before the initial probe, model dispatch
and SSH execution. Each model worker rechecks saved guidance and SSH route
authority after its dispatch permit, including on a provider compatibility
retry. Proposed commands still require a run-bound command review.

The Windows suites passed 221 tests with one opt-in OpenSSH test ignored.
Reopened-job fixtures confirmed masked guidance on the model wire, two targets
using one saved classification, and refusal of a stale dispatch permit after
the connection skill was removed. The combined forecast matched the estimated
input and effective output caps of the two starting requests.

Native graphical checks and configured-provider acceptance remain open.

### Reviewed skills for scheduled read-only Swarm advice

The setup review shows one exact masked system and input template per saved
SSH target. Only the output field is filled after that target's fixed probe.
The combined forecast uses the reviewed template size and the effective model
output caps. The runtime diagnostic output can change actual usage. An
optional setup classifier has a separate preview and a 128-token output cap.
Classification uses the masked goal and eligible metadata. A valid result
refreshes the templates, combined quota and approval identifier.

A versioned standing approval binds the ordered target pins, exact system and
template, masked guidance, selected profile and classified IDs. A firing
rechecks the current target's pins, exact route and profile before launching
the probe and at each model attempt and response. The approved pin union keeps
automatic skill selection scoped even if another target becomes unavailable.
Read-only replies cannot authorize commands. Existing approvals retain their
serialized form and legacy model request path.

The Windows ARM64 Scheduler and advice selection passed 141 tests. The setup
fixture verifies separate target pins, classifier input and accounting, the
renewed review and combined quota, and the persisted standing approval. A
guided worker loopback verifies the exact request and refuses a repair after
authority is revoked. A receipt test verifies legacy serialization and rejects
pin tampering. Windows workspace Clippy and macOS ARM64 all-targets
compilation passed. The native Scheduler window, the guided OpenSSH workflow
and a configured provider require runtime acceptance.

### Reviewed comparison of scheduled Swarm results

Read-only and iterative Swarm jobs can approve a separate standing comparison
before saving. The preview shows the exact masked goal, system instructions,
ordered target identifiers and the status and result fields filled after each
firing. A separate quota forecast accounts for the bounded result fields and
effective output limit. The receipt binds the template, profile revision,
target order and managed redaction rules.

The Scheduler waits for the completed target reports before dispatching one
model request. It checks the active run, saved profile, policy, pinned routes,
mask and approved memory again at dispatch. Each result field is masked before
shortening. The provider receives only the reviewed fields. Provider usage
follows the profile's usage ledger. Cancelling the run discards the comparison
and skips saved-chat creation. When authority changes, a worker is unavailable,
or the provider response fails validation, the original target report and
aggregate outcome are retained.

A valid comparison contains a Markdown table with one row per target and an
`Errors` column. The Scheduler stores it atomically with the journal report.
Saved conversations copy the comparison into the first turn. Older reports
and approvals remain readable.

The Windows ARM64 selection passed 163 application tests and 15 job store
tests. The integration fixture reopens the approved job, checks the exact
masked comparison on a loopback HTTP endpoint, verifies journal persistence
and cancels a second run before model dispatch. The macOS ARM64 application
all-targets check passed. The opt-in OpenSSH browser test was skipped. The
native Scheduler window and a firing against a configured provider and pinned
SSH targets still require acceptance.

### File size limits for knowledge sources

The pinned Java advanced source editor sets a file size in MiB for individual
files and directories. The scanner applies the smaller of that limit and the
format ceiling. The Java default is 50 MiB. Rust accepts an optional limit in
bytes for both source types. Explicitly reviewed limits can reach 2 MiB
for UTF-8 and 8 MiB for PDF input. Default and previously saved numeric
limits keep their original 128 KiB and 2 MiB format boundaries. Matching
the Java size range requires the larger corpus and scalable index work
under B6.

The limit applies to bytes read from the checked file handle before decoding
or PDF extraction. It is enforced in folder preview, indexing, automatic
refresh and the final source digest check before retrieval. A folder review
identifies files excluded by the configured limit. An automatic source that
grows past its approved limit keeps its prior index until the source can be
verified again.

The saved index scope includes configured limits. Changing a folder's limit
requires a new folder review. Changing an individual file's limit returns its
sync mode to Manual until Automatic is selected again. A reopened automatic
approval must match the saved limit. Existing numeric limits keep their
encoding in source definitions and index scope digests. The new size
representation marks the expanded boundary in both places. Larger line
locations fit the existing KTRAG and citation encodings.

Windows ARM64 passed 22 Knowledge Stores and PDF tests. Fixtures cover
the exact byte boundary, source limits across reopening, stale approvals,
and preservation of existing passages during automatic refresh. The
signed managed embedding and native PDF worker tests were skipped.
Windows workspace Clippy and the macOS ARM64 all-targets check passed.
Native Knowledge Stores controls and the macOS PDF worker still require
runtime acceptance.

The expanded limits passed the Windows ARM64 release tests, including the
native helper using an approximately 7.5 MiB PDF and cancellation during
input transfer. Text retrieval and the reopened citation preserved a
location beyond line 131,072. The macOS ARM64 cross-check passed.

### Persistent local HNSW knowledge search

The pinned Java local index uses hierarchical cosine search with degree 16
and construction and search widths of 200 and 64. Rust builds a deterministic
local graph over normalized vectors for 257 to 2,048 passages. Active sources
supply entry points for filtered search. Selections of up to 256 enabled
passages or at most half the index use exact ranking over their contiguous
source ranges. Retrieved context can refill from exact results when the
graph candidates cannot fill the allowed excerpts.

KTRAG v4 stores typed citations, vectors and the optional graph in one
private, atomically replaced snapshot. KTRAG v1 through v3 remain readable.
The 18 MiB limit applies to the complete snapshot. Indexing checks the
projected vector payload when it learns the embedding dimension and before
requesting further batches. For Qdrant stores, the configured collection
dimension enables this check before embedding. Saved source digests and
citation checks guard retrieval and subsequent model dispatch. Unchanged
automatic refreshes reuse the existing graph.

The repeatable local fixture uses runbook, script and configuration files
with 256 or 1,024 passages and synthetic normalized vectors at 768 or 1,536
dimensions. It compares the top five graph candidates with exact ranking
and measures source verification and snapshot loading separately. This corpus
covers up to 4 MiB of source text. A separate Windows ARM64 release fixture
measures 2,048 and 4,096 synthetic passages at 768 dimensions. It matched
160 of 160 exact top-five results in each tested selection. With all sources
enabled, graph p95 was 0.266 and 0.391 ms. Exact ranking was faster with half,
one quarter or two of the 32 sources enabled. The hypothetical snapshots
used 14,687,576 and 29,374,778 bytes. The larger shape exceeds the current
18 MiB snapshot budget. Larger real corpora and further production capacity
remain work under B6.

The production 2,048-passage fixture at 768 dimensions used 14,687,576 bytes
in its reopened snapshot. The Windows ARM64 release ranking p95 was
0.351 ms with all sources enabled and matched all 160 exact top-five results.
The benchmark process had a sampled peak working set of 60,837,888 bytes.
The regular update fixture also checked filtered exact ranking, cancellation
with the previous snapshot intact, and reviewed citation retrieval.

Windows ARM64 passed 30 Knowledge Stores and PDF tests and the opt-in
measurement. At 1,024 passages and 1,536 dimensions, HNSW matched all 160
exact top-five results and took 5.96 ms p95 for ranking with all sources
enabled. Exact ranking took 15.47 ms p95. Measurements with fewer sources
enabled drove the exact ranking threshold. Source verification and snapshot
loading were measured separately.

### Optional Qdrant knowledge backend

The pinned Java backend stages Qdrant generations and switches a remote
manifest after replacement. Rust stages bounded point batches and switches
the active generation by publishing the private KTRAG v5 snapshot with its
source coverage. The wire payload contains vectors, source ordinals and
opaque point and generation IDs. Verified passages and citation locations
stay in the local index. Qdrant results must belong to the expected
generation and enabled sources and match the stored point positions.

Knowledge Stores exposes the backend, service URL, collection, embedding
dimensions and optional credential environment variable. Qdrant uses
system TLS for HTTPS and permits HTTP on numeric loopback only. Manual
indexing, background refresh and reviewed retrieval disclose or bind their
destinations. Activation waits for source revalidation and local publication.
Failed staging preserves the previous active generation. Retirement targets
only known generations. Strict collections require the two payload indexes
described in the README.

Qdrant REST routes for collection inspection, waited upserts and filtered
queries follow the pinned Java adapter and Qdrant API reference. Rust uses
its own local receipt format. Windows ARM64 passed 45 tests and the
separately run live smoke with the official Qdrant 1.18.2 Windows x64
service. The smoke used an ephemeral API key, staged a synthetic generation,
searched with source filters and removed the owned collection. The service
needed a short private storage path for this Windows run. Windows workspace
Clippy and macOS ARM64 all-targets compilation passed.

AI Manager now accepts a typed Qdrant API key from the Vault or an
environment variable. The settings and index scope carry the credential
reference. Manual index and search consent and draft publication bind to
the reviewed Vault revision. Automatic refresh and explicit retrieval use
the live Vault when starting. Each Qdrant request checks the revision
before dispatch and after its response. Terminal Chat and snippet analysis
also check the retrieval revision before sending reviewed excerpts to a
model. The Windows ARM64 selection passed 104 application tests and seven
Vault crate tests. The official Qdrant 1.18.2 service passed the loopback
smoke with both environment and Vault keys.

A Windows ARM64 release smoke staged, queried and deleted 2,048 synthetic
768-dimensional points in each authentication mode. The Vault run used a
strict collection and confirmed staging refusal for each missing payload
index before the collection was ready. Configured HTTPS, strict query
limits below the enabled selection, and native interaction with the picker
remain open.

### Control API event backpressure and MCP restart

The bounded Rust client keeps subscription overflow notices in wire order
with retained events and reports its own losses separately. Windows ARM64
passed a real socket test with two filtered subscriptions and a pending
pane read. The correlated reply and ping arrived alongside the exact two
server overflow counts and a separate client loss count.

The same native `kortty-cli mcp` process survived private Windows desktop
restarts. The first unscoped pane list discovered the new listener. A
catalog-only reconnect required a fresh pane list for targeted requests
even when the pane ID was reused. The native output wait returned a timeout
and the bridge continued. Managed MCP denial returned `blocked_by_policy`
while ordinary CLI ping still succeeded. The bridge exited on EOF. The
private profile and owned processes were removed. Affirmative consent
and exact approved input remain separate native acceptance items.

### Programmatic desktop notifications

`notification.show` requires string title and body fields and admits
either field on its own. It validates before applying the five-second
per-connection limit. It bounds the sanitized fields to 80 and 200
UTF-16 units and prefixes the title with `korTTY · `. The `shown` field
reports admission, and `supported` reports native submission availability.
A Windows window that disappears before submission returns
`shown: true, supported: false`.

The Windows ARM64 desktop and CLI submitted one title-only request to
Windows Shell and received `shown: true, supported: true`. A separate
private run verified managed denial and an ordinary Control API ping.
The Linux x64 windowing and Control crates and the macOS ARM64 app and
native notification paths compiled. Visible presentation and native
macOS and Linux submission remain acceptance items.

### Bash here-document references and native SFTP download resume

The Bash structure scanner counts expansions inside unquoted
here-documents. It keeps each body and terminator out of the outline,
applies quoted delimiter and tab-stripping rules, and requires a later
matching terminator. This preserves outline navigation following a
whitespace-delimited arithmetic shift. Windows ARM64 passed the structure
and durable SFTP resume suite with 83 tests and two opt-in cases.

Both opt-in cases passed from the Windows ARM64 application test binary
against pinned Fedora OpenSSH. The download discarded a real SFTP data
reply at byte 131072 and reopened the workbench. The resumed connection
read the complete saved prefix, began the suffix at byte 131072, and
published all 524288 bytes with the expected SHA-256 and Windows parent
access rules. The upload rechecked its lost write reply and changed-prefix
case. A separate pinned SSH check confirmed both temporary directories
were absent. Folder-child retry and other native clients remain open.

### Folder retry and saved diagram source navigation

SFTP Retry binds a retained offer to its exact file row. A folder retry
resumes its eligible saved children and requeues other failed children in
their existing order. Completed and skipped children remain settled.
Pause, cancellation and guarded prefix verification apply to queued
resumes. A changed prefix stops the attempt. Cancelling a waiting resume
keeps the checkpoint available for a later attempt.

Windows ARM64 verified the normal queue actions against pinned Fedora
OpenSSH. The test interrupted the second file at byte 131072. After
reconnect, a paused parent retry waited for Continue and resumed the
original child. It compared the complete prefix, final digest and local
access rules. The completed sibling kept its bytes and modification
time. The single-file upload and download regressions passed in the same
run. All three remote scratch directories were removed and their absence
independently verified.

Reopened library diagrams now route a source click to the original
snippet editor session. A scoped hit maps to absolute lines, and the
active draft and persisted diagram must still match the saved source.
A stale hit can still navigate the diagram's scoped source preview.
The Windows application regression covered saved selection, source
edits and switched or replaced editor sessions. Native graphical focus
and live parallel-lane acceptance remain open. The macOS ARM64
application and test compilation passed.

### Reviewed changed-file batches and named highlighting selection

The directional SFTP review offers one changed file or all eligible
changed files in either direction. The preview binds both SHA-256
digests, observable source attributes and the local file identity.
Each entry stages verified bytes and retains its displaced original.
An independently stale entry can fail while eligible siblings continue.
An uncertain server namespace mutation stops the batch and reports
the recovery paths. The transfer history retains per-file outcomes.

For a server target, the staging account must own the file. The
operation uses the local source modification time, retains the
original POSIX mode and attempts to retain the original group. POSIX
rename moves the original into private quarantine. The worker checks
its digest there and publishes through an exclusive hard link. The
public name may be vacant during that interval. SFTP v3 does not
provide a stable remote file ID or atomic compare-and-publish.

The Windows ARM64 application exercised two files in each direction
against pinned Fedora OpenSSH. It verified the saved originals,
contents, modification times and server modes `4640` and `0751`. A
second pinned connection occupied the target between quarantine and
publication. Both publication and restoration preserved that entry,
the original remained recoverable and the second batch item stayed
untouched. The three live resume regressions also passed. An independent
SSH check confirmed the absence of all four temporary remote directories.

The connection editors for local and SSH profiles now use a named
highlighting picker. It includes the inherited default, explicit
`None`, built-in and custom sets and a preserved missing stored ID.
The editor indicates when global highlighting is off. Windows ARM64
tests verified the picker choices and saved-profile behavior. Native
graphical acceptance and the global controls in Terminal settings
remain open.

### Combined directional synchronization and terminal preferences

One directional review now binds the missing-file digests and the
changed-file replacements. The worker creates missing folders and
publishes missing files first, then applies the checked replacements.
It refreshes canonical server parent metadata after its own writes and
keeps the exact file-level checks. Separately confirmed removals run
after successful copies and replacements. Declining removals uses the
same prepared copy and replacement plan. The final operation consumes
its approval once.

The Windows ARM64 application test binary exercised a mixed copy and
replacement in each direction against pinned Fedora OpenSSH. Both
operations preserved the source modification times and retained the
displaced files. Declining server removal preserved the destination-only
file. A separate confirmation removed the exact reviewed local entry.
The other four live transport probes passed, and the five remote
scratch directories were independently checked absent.

Terminal Settings now expose the global highlighting switch,
full-screen behavior, the default set for inherited panes and the
policy-controlled actions. The same rule editor opens as a child of
Settings. Rule saves survive cancellation of the independent Settings
draft. The Settings save checks the on-disk revision and merges its
changed global fields into the current rules.

Saved SSH and local profiles now resolve native custom themes by their
catalogue IDs. Built-in references retain their serialized form.
Unavailable choices stay visible, and deletion does not release a
native ID for later use. Java connection XML retains valid unknown
theme IDs and explicit fallback colors. Java custom-theme definitions
and project catalogue bindings are covered in the next checkpoint.

### Java theme and project appearance exchange

Settings reads a Java `themes.xml` into a review before changing the
draft. Identical definitions are counted and conflicting definitions
abort the import. Existing connection references resolve an imported
custom ID without editing the connection. Export writes the custom
library to a new file and uses the same exchange IDs as connection XML.
Existing native themes receive exchange IDs in Settings before they
can be exported as definitions. A linked connection keeps its explicit
field overrides and inherits other fields from the library. Java
background-image paths and Agent panel colors survive interchange.
The next theme checkpoint covers their native settings and rendering.

Java project restore uses the primary connection's appearance and tab
font zoom throughout the split. Each pane retains its own route and
initial dimensions. A toolbar local shell inherits global appearance.
The native layout retains this presentation, and local and SSH actors
start with it. Java export rechecks the reviewed primary catalogue
appearance before publication.

### Reviewed foreign-owner SFTP replacement

Eligible files have a known foreign UID, ordinary permissions and a
protected canonical parent. Before writing, kortty copies the original
into a private backup, verifies that copy and compares the complete
original through the opened target handle. The target stays at the same
path and inode. The review describes partial visibility while writing.
The result identifies the backup and reports an unconfirmed write or
source modification time when applicable.

The Windows ARM64 test against pinned Fedora OpenSSH used a synthetic
group-writable fixture whose UID came from the existing subordinate
mapping. It verified the preserved UID, GID, mode and hard-link alias
and compared the private backup with the original bytes. OpenSSH refused
the source modification time, and the result identified that refusal.
An independent SSH check confirmed removal of the remote test directory.

### Theme backgrounds and Agent panel colors

Theme editing exposes the six Agent panel roles from the pinned Java
dialog. The selected pane's theme controls the docked activity,
detached activity, Agent, Ask, Planning and related Agent views. Swarm
uses its focused live pane, with the active application theme as a
fallback. Legacy XML palettes migrate the exact former defaults and
retain user-specified colors. Changes made in Settings also travel in
the Java theme export.

Java stores the background path in its model. Its pinned terminal
renderer has no background-image workflow. The Rust Settings form
shows the imported path as metadata and previews an explicitly chosen
local image. It normalizes the image, stores it privately by digest
on Settings Save and includes it in profile backup and restore. The
native terminal scales the image to cover and blends it behind
default cells at 25 percent opacity. Explicit ANSI backgrounds,
selection and Kitty graphics retain their layers.
Settings retains published copies for other running instances. An
exclusive Replace restore retires obsolete images after making the
restored settings durable and verifying the old file's content ID.

On Windows, the pinned Java ThemeManager read and saved a Rust
`themes.xml` with all six edited colors, an exact custom ID and an
image path. Rust loaded the Java result. The pinned Java
ProjectManager also accepted the exported remote tool tabs. The
native graphical and Unix split-planner checks remain open.

### Resuming reviewed server replacement uploads

After an interrupted same-owner replacement upload, a fresh
synchronization review can reuse the original private staging folder.
The receipt binds the authenticated route, host key, original server
bytes and attributes, and local file and parent identities and digest.
The stage is confined to the reserved private replacement staging
directory name. The worker verifies the complete prefix through the
newly opened handle and resumes at the verified offset. It keeps the
receipt through read-only finalization, then retires it before the
first metadata mutation or destination quarantine. The separate
target reservation stays held through publication and cleanup. A lost
publication receipt and a foreign-owner in-place write keep their
existing recovery boundaries.

A Windows ARM64 test against pinned Fedora OpenSSH lost a write
receipt, reconnected, verified the original file and resumed at byte
163840 after a fresh review. It verified the final bytes, mode and
timestamp and removed the receipt and stage. The ordinary and
foreign-owner replacement regressions also passed.

### Server folder disk usage

The Server pane measures selected folders in their displayed order
with one quoted `du -sk` command on the verified SSH connection.
The common order preserves hard-link accounting across roots. The
direct and typed isolated exec paths bound runtime and combined output
and bind each reported size to an exact requested path. SFTP checks
the canonical selected roots on the browser channel before and after
exec. The browser labels allocated bytes and partial results. SFTP-only
connections retain the logical scan. Cancelling `du` closes its private
exec channel and leaves the browser channel open. Cancelling during a
stalled SFTP request can require reconnecting the browser.

A Windows ARM64 application probe against pinned Fedora OpenSSH
verified both direct and isolated sessions. The ordered selection
measured 1,024, 0 and 32 KiB across a sparse file, a shared hard link
and a partially unreadable tree. The probe also exercised the browser's
measure, cancel and refresh actions. A disposable Fedora listener with
`MaxSessions 2` returned the same allocated sizes. With `MaxSessions 1`
or `ForceCommand internal-sftp`, direct and isolated sessions reported
logical file sizes and kept the browser usable. Native desktop controls
still require graphical acceptance.

### Scheduler Journal selection and Settings Escape

The native Scheduler Journal sorts all five columns for completed
and active runs, with the newest Started time first by default.
Completed runs can be selected together and removed with one reviewed
operation after active runs finish. The confirmation binds the visible
run IDs. Changed filters or rows invalidate that confirmation.

Settings handles Escape while an input has focus. An open overlay,
search, shortcut recorder, theme form or file browser has precedence
over closing the parent Settings draft.

### Run journal retention, dialog keyboard focus and archive estimates

The Scheduler Journal has a persisted 0–3650 day retention preference.
A fresh profile starts at 14 days. Existing profiles without a preference
keep age pruning disabled. Keep Existing restore preserves this behavior
when it adds missing preferences beside an existing native run journal.
The scheduler uses local calendar days and keeps records at the cutoff.
It prunes after saving the preference, on startup and after a durable
run commit. Dependency results and per-target Session Journals retain
their existing lifecycles.

Settings and AI Manager search results transfer keyboard focus to the first
enabled editor in the revealed row. AI Manager Escape closes the active
picker, search or child form first. The parent uses the existing dirty
close confirmation in both a detached window and a hosted tab.

The Server archive setup shows selected file bytes and allocated folder
usage before review. It can reuse a complete measurement of the same
folder set and runs other estimates on the verified SSH connection. The
dialog labels partial or unavailable sizes. Its review uses the separately
validated logical manifest after exclusions. Entering review cancels an
outstanding estimate. A name lookup can retry while the estimate releases
its local command slot or the server refuses the session open, within the
existing lookup deadline.

The Windows ARM64 application passed the focused Iced and retention tests.
Five pinned Fedora OpenSSH probes covered the archive setup and review on
direct and isolated routes, including a held local command slot. Native
graphical acceptance remains open.

### Scheduler Session Journals and file editor Cut

The scheduler commits exact per-target journal UUIDs with completed runs,
including AI Agent firings and deferred Swarm comparisons. The run's Session
Journal action checks its current row and configured root and opens the
retained native journals. A unique same-job retention receipt can resolve a
removed duplicate to its retained original. Run removal retires the links,
and portable job definitions omit them. Virtual recording replay remains a
separate action.

The Edit menu and the macOS system menu route Cut to the active writable
file editor. The editor uses its existing clipboard and undo handling.
Terminal Ctrl+X retains its terminal input route.

Windows ARM64 tests covered opening two target journals after reopening the
run store, duplicate receipts, policy and root rechecks, and Cut and undo
through the workspace menu. The macOS ARM64 cross-check includes the
focused system-menu routing test. Native graphical acceptance remains open.

### Scheduler journal details, Java retention and AI provider discovery

The Scheduler Journal shows the selected run's linked target journals,
including outcome, AI usage, retention, and duplicate or unavailable state.
Selecting a row resolves the persisted links without opening viewer windows.
The explicit viewer action rechecks the selected run and journal root.

The retention controls can import the explicit preference from Java
`global-settings.xml`. The review stages the value for Apply. Export
writes only the saved retention preference to a new Java XML file.
The Rust XML functions and the pinned Java JAXB model round-tripped 0,
37 and 3,650 days. Native dialog acceptance remains open.

The LM Studio profile wizard and AI Manager offer model discovery from the
approved native `lms ps --json` and `lms ls --json` commands. Picking a
result sets its model key in the explicit preset. Real LM Studio and native
dialog acceptance remain open.

Qdrant search checks the collection's current strict query limit before
dispatching a vector. The query uses the smaller of the effective selection
and the server cap. A zero or invalid cap stops dispatch. Returned passages
still require the active generation and verified enabled local IDs.
An authenticated Qdrant 1.18.2 loopback test on Windows ARM64 returned one
of two enabled synthetic passages and retired the test generation.

### Reviewed HTTPS journal answers and retained Scheduler totals

The Journal Viewer and Manager can select a saved HTTPS profile for a
manual question. The user reviews the destination, model, instruction
and exact masked evidence before dispatch. Preparation masks known
credentials before clipping question and evidence fields. The answer
worker rechecks saved settings, managed policy, credentials and physical
evidence before each request. Returned findings remain exact reviewed
quotations. Local evidence remains available in the review.

The Windows ARM64 native TLS fixture verified the exact OpenAI-compatible
and Anthropic request messages. The OpenAI-compatible response-format
fallback sent the same reviewed messages once more with the format
field omitted. The private test CA applied to the fixture connection.

The Scheduler Jobs list shows retained Session Journal AI totals for each
saved job. The selected job's Action page shows newest retained activity
and total run count, AI usage and storage. The bounded background catalog
read labels incomplete coverage. Cost totals use complete compatible
currencies. Native graphical acceptance remains open.

### AI Manager default selection, mirror holds and SFTP listing columns

The AI Manager saves the default marker immediately for an eligible saved
profile. The saved baseline excludes unrelated edits. The workspace adopts
the new default after the write, and the dialog retains its other drafts.
New profiles carry the default marker through explicit Save. This follows
the pinned Java `AiManagerDialog.persistDefaultProfileSelection` workflow.

Broadcast and Multi-exec use the pinned Java `MirrorTargetGuard` rule.
Mirroring leaves out destinations with a waiting coding agent, an active
terminal-driving built-in Run or a pending paced paste. Direct focused
input retains its route. Queued mirrored writes recheck the latest
published coding-agent state before writing.

The local and server SFTP panes expose Name, Type, Size, Date, User, Group
and Permissions sort columns. Type uses the pinned Java file-category
glyphs. Fixed hidden and ordinary folder/file groups survive both sort
directions. Size puts unmeasured folders first in ascending order and last
in descending order. The parent navigation row has no selection or transfer
identity. Narrow panes scroll horizontally and retain bounded pages.

The Windows ARM64 tests exercised the rendered checkbox at 760 by 480 in
both surface forms, the mirrored-input writer, and local and synthetic
server listing fixtures. The macOS ARM64 application and test cross-check
passed. Native window interaction and live coding-agent process acceptance
remain open.

### Snippet commands, recorded quota, Control input and SFTP account labels

The snippet editor can copy compact or embedded commands for Bash, Python,
Perl and Ruby. Embedded commands preserve source when compact lexical
rewriting cannot safely flatten it. Large payloads wrap Base64 data in
a here document. The compact AI fallback reviews the exact request and
validates a single line against the masking policy and the original
placeholders. Copying the reviewed result is an explicit editor action.

AI Manager opens the saved profile's recorded quota for the selected
period when Limits and timeouts is shown. The meter uses the ledger's
reported and reserved counts. A queued refresh supersedes stale profile
or period results. The view retains manual refresh and partial-data
status.

The Control API returns JSON-RPC `-32024` with
`host_shortcut_conflict` and CLI exit code 2 for a complete conflicting
host Agent shortcut. An explicit supported literal send or bracketed
paste follows the existing terminal write route. MCP checks the
shortcut after its existing consent and pane revalidation.

The SFTP browser shows numeric server owners and groups immediately and
fills account labels through bounded read-only requests on the verified
SSH connection. It can continue across batches for the same listing.
The account names are advisory, and ownership changes retain the raw
numeric identifiers. Exec refusal retains numeric labels.

### Source selection, managed recovery, local file ownership, journal search and MLX removal

The native Editor selects the exact source range when an outline row is
activated. The snippet editor shares the range check. Markdown and
AsciiDoc use the existing bounded heading scanner. Every Editor row is
bound to the revision that produced it, including different placeholder
occurrences on a single line.

`[rule.terminal] session-restore` follows the pinned Java rule resolver.
User settings outrank group settings and group settings outrank the
baseline. The least automatic choice wins within one tier. Settings shows
the effective mode as managed. The native override preserves the saved
device preference, the manual File action and the interrupted-startup
confirmation requirement.

The SFTP Local pane adds advisory ownership names without delaying the
listing. Unix resolves the captured numeric user and group IDs. Windows
reads owner security through the anchored local directory, uses the
account name or SID and leaves the POSIX group column unavailable. Results
from an older listing are discarded.

Cross-journal search ranks masked curated excerpts with the catalog
metadata and can fuse their reviewed local-embedding ranks. Entry
navigation checks the reviewed XML on a worker and compares the displayed
document before selecting the entry. Rotated capture citations continue to
use the exact physical part, sequence, offset and digest.

MLX runtime removal binds the reviewed activation state and package
directories. It refuses active leases, clears runtime activation and
removes the reviewed packages. It retains registered models, saved
profiles and the accepted signed catalog and revocation state. Apple
Silicon acceptance requires a signed release package and a compatible
model.

### Bash declarations, signed MLX acquisition, journal chats and attachment quotas

Bash recognizes local and explicit global declarations, export and
readonly flags, indexed and associative arrays, loop variables and
destinations from `read`, `mapfile`, `readarray` and `getopts`. A plain
assignment reuses a visible declaration or creates a global row. The
reference count follows the lexical function, including uses before a
declaration. Exact name ranges remain available in the snippet and file
editors. Here-document detection distinguishes redirections from
arithmetic shifts.

The MLX manager checks a signed stable catalog on request. It presents
compatible package choices and requires approval for the selected
address, size and SHA-256. Activation checks the package against the
signed catalog and current installed trust state and uses the existing
lease and rollback path. The legacy tag is used only after a missing
primary index. Native use requires a compatible signed Apple Silicon
release.

Journal search uses the embedded, reviewed XML snapshot of a recorded
Agent chat. Its masked message bodies contribute to the shortlist and
exact citations. The citation checks the original journal version and
opens the Agent entry with its recorded chat expanded.

Terminal Chat attachment flowcharts show the quota projection for the
exact masked request and its effective output cap. A stale forecast
requires a refreshed review. The same dispatch rechecks the attachment,
saved profile and execution policy.

### Shared MLX update policy, managed download restrictions and Python parameters

The saved runtime update policy governs both llama.cpp and MLX on
supported Macs. Notify exposes a signed compatible offer. Automatic
Stable stages the exact verified archive and waits for an idle activation.
The commit rechecks the persisted authorization and installed state.
Verified withdrawals enter durable trust before a replacement can be
offered. Resident runtime validation stops a withdrawn sidecar.

Selecting an older signed MLX version pins that runtime ID. The pin
survives a restart and holds an available release during background
checks. Selecting the latest version or following stable releases clears
the pin. A current installed version can change pin state through the
reviewed metadata operation.

Managed `allow-runtime-downloads` applies to both runtime families. The
effective mode is Off under a deny rule. Online checks, redirects and
activation recheck the rule. Signed offline imports remain available.

The Python structure scanner finds exact parameter ranges through
multiline, nested and continued signatures. It keeps the reference's
4,000 UTF-16-unit signature bound. Both editors use the shared scanner,
and the snippet panel attributes references to their lexical scope.

The anonymous application release API returned HTTP 404 for the
configured repository and its latest release on 2026-11-12. The
retained Windows v0.1.0 packages verify locally. Native application
update acceptance requires a reachable owner-published stable release
with exact platform asset names and GitHub SHA-256 digests.

### Model policy enforcement and Python structure bindings

The pinned enterprise policy defines independent runtime acquisition, model
acquisition and embedded model rules. `allow-model-downloads` covers the
Hugging Face search, review, transfer and resume path. Workers recheck
before each HTTP hop, between transfer chunks and before registry
publication. A denied publication rolls back the prepared files. The
local discard action remains available for a paused transfer.

`allow-user-models` covers embedded GGUF chat, GGUF embeddings and MLX.
The client checks the current policy before taking or reusing a lease and
again before dispatch. Manual registration and standalone model controls
follow the pinned Java scope. All current native registrations are subject
to the deny rule until the trusted `[[ai-runtime.model]]` provisioner is
implemented. The managed MLX connectivity test applies its short output
limit after creating a valid profile.

The Python structure view reports class body assignments and `self` and
`cls` writes as class fields. Dotted references require a receiver that
identifies the containing class. Ordinary and async loops and bracketed
comprehensions record exact name ranges for simple and unpacking targets.
Comprehensions track a separate scope and resolve the first iterable in
the surrounding scope. The file editor selects those ranges without
building a reference index.

### Detached AI Manager on Windows ARM64

A native private run started with a 1120 by 780 client area and two
synthetic profiles. After a General edit, changing the default wrote the
new profile marker immediately and preserved the draft. Close prompted
for a decision. Keep editing restored the draft. Escape from the focused
timeout field reopened the guard. Discard closed the dialog. A restart
loaded the new default and the original General setting at 760 by 480.

The minimum-size run exposed a horizontal scrollbar over the tab labels.
The tab strip now reserves layout space below the buttons when they
overflow. The corrected native run opened Local Models from the lower
part of its tab and closed from the fixed footer. A synthetic installed
model download deny rule hid the Hugging Face search and download controls,
showed the managed notice, and allowed the local paused-download lookup.
Hosted window interaction and multi-monitor and DPI changes remain open.

### Installation-policy models and bound approvals

Global `[[ai-runtime.model]]` declarations support Java-compatible aliases for
llama.cpp and MLX. The native profile picker checks administrator-controlled
local source paths and projects eligible models as read-only choices. GGUF can
use a complete split group. On supported Apple Silicon, MLX uses a data-only
safetensors directory. A policy alias resolves directly from the installation
policy and does not create a writable registry entry.

The profile binds the exact declaration and anchored source identity. Resident
sidecars check the current policy and leased files before accepting a new
request. The RAG index scope and reviewed publication, journal summary choices,
and scheduler profile revisions bind the same identity. Ordinary profile
receipts retain their existing encoding. Remote HTTP(S) and Windows UNC source
acquisition, signed inference with protected GGUF and MLX sources, and native
picker acceptance remain open.

### Remote installation-policy GGUF sources

Complete GGUF files can come from a configured HTTPS address, or from
HTTP and Windows SMB UNC paths with an administrator-provided SHA-256.
The acquisition checks the current declaration while receiving bytes
and immediately before publishing the validated GGUF. The request uses a
lease on the file whose bytes were independently hashed after publication.
The signed runtime receives that file through a protected Windows path
or an inherited Unix descriptor.

A configured pin authenticates the cached bytes after restart. An
unpinned HTTPS declaration uses the system TLS trust chain to acquire
fresh authority per process. Matching authenticated bytes can keep the
existing cache file while another process uses it. Windows SMB uses
anchored read handles and validates the supplied pin. Native SMB
acceptance requires a share with a complete known GGUF. Unix descriptors
bind the inode and the worker detects changed file stamps. In-place writes
by the Unix cache owner can still race with the runtime read. Write
exclusion, native signed inference and picker checks remain open.

### Kernel-protected policy model launch

Linux enables fs-verity on the leased cache inode before launch. When the
filesystem lacks fs-verity, it can use a sealed memory file for models up to
64 MiB within a 128 MiB process-wide budget. kortty hashes the protected bytes
again against the reviewed identity and gives the signed runtime the exact
descriptor. The Fedora ARM64 Btrfs test rejected enablement with an open
writer and subsequent writes to the protected inode. A child inherited the
same bytes after the original name was replaced. The native tmpfs test
exercised the sealed fallback through the production protection function.
The release-signed Linux ARM64 CPU sidecar loaded a protected GGUF from
that inherited descriptor in a separate native probe. It rejected an
unauthenticated request and returned a completion after authentication.
The full application acquisition and profile workflow on Fedora remains
to be checked.

The x64 Windows test executable under ARM64 emulation verified the full
HTTPS policy alias flow with the release-signed CPU runtime. The remote
model stayed outside the normal model registry, returned an authenticated
completion, and the test stopped the owned sidecar. The release index
currently lists Windows x64 packages. A signed native Windows ARM64
runtime is still required for the same acceptance test on that target.

On macOS, an administrator can bind a remote declaration to a pre-staged
`protected-cache` with a SHA-256 pin. The signed runtime inherits the
administrator-controlled file. The idle monitor stops that sidecar so the
next request starts with a newly verified descriptor. Native Apple Silicon
launch and idle restart remain to be checked.

### Scoped AI profile creation and editing rules

The Profiles page follows independently resolved `allow-create` and
`allow-edit` values. Add and Setup Wizard require creation permission.
Existing profiles stay selectable under the edit lock and remain available
to the application. The editor, deletion, default reassignment among saved
profiles and connection tests are locked. A new profile can still be
configured and selected as the default when creation is allowed.

The settings writer compares proposed profiles with the current saved
catalog under the mutation lock. It checks the current policy before
staging and again at checked publication. The rule covers first writes,
replacements and the immediate default write. The independent web-access
rule can still clear web search when editing is locked.

The compact AI Manager layout keeps its page and footer actions visible
when a long status notice wraps. The notice remains scrollable.

### Provisioned AI profiles

Global `[[ai-profile]]` declarations use the Java provider names and exact
`policy-` identifiers. The AI Manager lists the provisioned profiles after
the user catalog and displays their route, model and credential status
read-only. The settings file persists local references and leaves profile
definitions and credential envelopes in the installation policy. A model-less
loopback LM Studio or OpenAI-compatible profile resolves a unique loaded
chat model from LM Studio's authenticated native metadata route. Startup,
profile refresh and connection testing resolve asynchronously. The exact
model key appears in the review and is rechecked before sending. Queued
journal and scheduler jobs for unresolved profiles wait for the discovery
result while unrelated work remains eligible.

The HTTP transport decrypts the Java-compatible envelope at use. Requests
and durable reviews bind the current provisioned definition and any policy
model it references. The profile revision changes on credential rotation.
The default, AI roles, knowledge store bindings, and saved connection
selections persist the exact policy identifier. New journal summary
approvals and saved Terminal Chats use the same identifier. Journal
approvals continue to bind the reviewed model, destination, and policy
authority independently. Saved chats resolve the selection when reopened
and require a new request review. Java conversation archives carry the
named identifier as provenance on import.

New scheduled Agent and Swarm approvals bind the exact policy identifier,
reviewed destination, model, and policy authority. Named profile revisions
are independent of the runtime handle. The Scheduler rechecks the reference
before generating proposals, sending a standing comparison, and executing
reviewed commands. A missing name cannot resolve to another profile.
The Swarm publication queue and saved conversation keep the exact identifier
across restart. Follow-up requests require a new review.

New snippet analysis and split-generation records store the exact
provisioned profile ID. Continued verification, application, and split
recovery keep the source record's reference. A deliberate profile change
in a new analysis uses the current selection.

Provisioned usage accounts keep the exact policy ID with a private
hashed filename. A recognized numeric policy ledger remains authoritative
while reservations are open. After they settle or expire, the migration
keeps its counters and reset period through a recoverable checkpoint.
The AI Manager reads the resulting usage in the managed profile details.

Existing numeric selections, Swarm publication entries, and the journal's
numeric local endpoint sentinel remain readable. Existing Scheduler proof
bytes remain valid. Provisioned runtime IDs sit outside the unsigned 64-bit
user ID range in every catalog. Low-number catalogs retain the historical
numeric aliases. In high-number catalogs, saved numbers resolve to user
profiles and provisioned profiles are available by exact policy ID.
The detached Windows ARM64 AI Manager displayed the managed default and
both personal profiles when one user ID equaled the old managed alias
and the other was the maximum user ID. The personal entries opened their
own forms and the catalog kept the named default marker.
Native tests with real HTTPS provider credentials, signed embedded runtimes
and the supported desktops remain acceptance items.

### Scoped automation capture and SFTP conflict settings

The effective `session-journal.automation-allowed` policy follows the
Java per-field scope and resolves same-tier conflicts in favor of denial.
Scheduler jobs and interactive Swarm runs can execute under a capture
denial. General journal permission and the automation capture permission
must both allow a new capture. Automation retention still uses its own
limits and general journal permission. The three managed retention caps
accept positive signed 32-bit integers.

The central SFTP settings display and lock the effective managed transfer
conflict action. Saving other preferences under the policy preserves the
stored personal conflict preference. Open file managers receive applicable
changes, including a managed override of a private manager's local choice.
The horizontal Settings scrollbar has its own space below overflowing
tabs so their labels and click targets remain visible.

### Portable project round trips

Portable `.kortty` files keep the flat native JSON format for workspace
layouts and project metadata. The reader decodes the workspace directly
from JSON so tab and pane indexes in maps retain their numeric types. This
covers native files saved after a Java import and after confirmed replacement.

### Local shell integration and terminal Agent recognition

The 2026-11-12 D1/A1 checkpoint extends the pinned Java behavior in
`core/LocalShellTtyConnector.java` and
`shellintegration/ShellIntegrationInjection.java`. Bash, Zsh and Fish
integration now also supports validated custom executables and interactive/
login startup arguments. Windows POSIX executables must resolve to eligible
absolute paths outside the Windows system and WindowsApps locations.
Configuration validation remains independent of executable availability, so
reading a foreign-platform saved profile does not require launching its shell.
The platform launch path performs the executable checks.

PowerShell and CMD snippets bring the shipped set to five. Local automatic
integration remains explicitly opted in and off by default. The selected
shell, vetted startup arguments and startup-file semantics are preserved
through private temporary wrappers or fixed launch commands; user startup
files are not rewritten. PowerShell retains the existing prompt function.
CMD deliberately uses the standard path prompt and OSC 133 A/B only: it has
no C/D command boundaries, exit status, duration or OSC 7 directory reports.
The session capability and global prompt-navigation gates still apply.

Connection Manager, Quick Connect and saved-profile launch/restore preserve
the integration preference for supported presets and custom shells. Java
connection XML imports and exports `shellIntegrationAutoInject`; the native
`rustLocalShellIntegration` extension remains backward compatible. Omitted
flags remain false, and contradictory Java/native flags are rejected.
Supported default-local-shell Java project routes retain the opt-in through
native layout serialization. Export checks the current saved connection
identity and integration preference rather than silently replacing an
integrated route with a plain default shell. Portable routes requiring a
custom command, working directory or Teamwork integration remain explicitly
omitted. This does not publish machine-local profile references or broaden
custom-program command-execution authority.

Terminal `agent` shortcuts have their own separate opt-in, also off by
default. The setup/status view explains the selected pane's readiness and
the relevant shell snippet. Recognition requires a fresh empty prompt marked
by both OSC 133 A and B, a live UTF-8 Xterm primary screen and the existing
password, alternate-screen, competing-run and broadcast guards. Regression
coverage includes repeated prompts, split transport packets, stale input,
ordinary input with shortcuts disabled and local shortcut editing/Tab routes.
Custom-program Agent Run remains blocked by the native-shell capability gate.

For native SSH panes, the user installs the snippet in the remote shell's
startup file and reconnects; local automatic injection does not modify remote
profiles. The connection now prepares an in-memory credential mask whenever
terminal AI is permitted, independently of logging, journal or MCP capture.
This includes incognito terminals without enabling persistent logging.
Older SSH sessions must reconnect to initialize that mask. The status view
reports this requirement. An `ssh` subprocess in a local terminal remains a
local target rather than becoming a native SSH Agent target.

The final Windows workspace run recorded **3,305 passed and 53 ignored tests
across 67 test summaries**, including **2,713 passed and 50 ignored** in the
application binary. These are nested counts, not separate totals. Windows
Clippy with warnings denied and formatting passed. Native ConPTY checks
covered PowerShell 5.1 and CMD Agent routes, and opted-in Git Bash `-i` and
`-il` startup integration. The native SSH Agent route against Fedora passed.
Fedora's focused PTY run recorded **29 passed and 4 ignored**, with Clippy
passing. The `aarch64-apple-darwin` all-workspace/all-targets check passed as
cross-compilation evidence only. See the compact
[verification record](target/verification/20261112-shell-integration-agent.md).

These automated route and startup checks do not establish manual native GUI
acceptance. PowerShell 7, live Zsh/Fish startup, native macOS/Linux GUI flows,
live Java exchange and the broader Agent acceptance matrix remain open.

The Windows ARM64 static-CRT release build completed in 6 minutes 41 seconds.
Its private-profile ZIP smoke passed read-only Control ping, verified exactly
one owned restored PowerShell local pane and closed the application gracefully.
No forced termination or remaining owned process was reported. The extracted
package and private profile were removed. This proves package startup, route
restoration and clean shutdown; it is not a manual GUI or packaged alias-input
acceptance test.

The verified [ARM64 ZIP](target/dist/kortty-0.1.0-aarch64-pc-windows-msvc-20261112-shell-agent/kortty-0.1.0-aarch64-pc-windows-msvc.zip)
is retained with its [checksum sidecar](target/dist/kortty-0.1.0-aarch64-pc-windows-msvc-20261112-shell-agent/kortty-0.1.0-aarch64-pc-windows-msvc.zip.sha256).
Archive SHA-256 is
`792275b96af85c5ee640308e06562aa52236573998941ba6dfcf892d758b51c5`;
application executable SHA-256 is
`cf66ee3e6cf77a0e3c9272edc203c1a734ad217888587e700bcf521e89c5ced7`.
Automatic approval review blocked moving, replacing or deleting the previous
ARM64 archive without a further stated reason. The verified new ZIP therefore
uses a dedicated directory, and the older archive is retained for that specific
reason. The retained Windows x64 ZIP still predates this checkpoint and needs
a current build and verification. D1/A1 remain in scoped acceptance; the other
implementation and release gates in [PORTING-PLAN.md](PORTING-PLAN.md) remain
in force.

### Connection input settings and managed policy

Saved SSH, local shell and serial profiles support **Paste line delay (ms)**
and **Strict terminal mode**. A blank delay inherits the application setting.
The accepted range is 0–1000 ms, and an explicit zero disables pacing. The
Java connection XML fields `pasteLineDelayMs` and `strictTerminalMode` preserve
an explicit zero and false independently of presentation inheritance. Imported
Java delay values are clamped to the supported range. Strict mode is applied
before terminal output on launch or reconnect, with Automatic using the
sandbox default. Teamwork keeps input settings for the exact same-target split
and reconnect. Its strict override accepts On and preserves sandbox filtering.
The managed paste warning floor continues to apply independently of delay.
HP2392 paced paste uses its existing per-line character and block handling.
IBM emulations retain single-line paste.

The `control-api` feature distinguishes an omitted rule, explicit allow and
deny. Explicit allow starts the listener and locks the setting while preserving
the saved personal preference. Feature keys and values follow the pinned Java
normalization. MCP keeps a separate personal opt-in. Disabling MCP retains its
saved write tools preference, and active writes require permission and consent.

`file-transfer = "deny"` preserves remote listing and metadata search,
ownership, permissions, hosted and restored SFTP tabs, and supported server-only
operations. The SFTP worker checks installed policy and the bound SSH route at
dispatch. Server copy uses SFTP copy-data and no-replace hard-link publication.
Archive extraction runs through the bounded server helper. Local archives also
remain available. The built-in text and image views follow
`load-into-snippet-editor`. External editing and ordinary upload, download,
sync and content comparison require transfer permission. The existing remote
archive creation reads file content and builds the archive on the client, so
it requires transfer permission. Q2 tracks server-only archive construction.
Policy is checked at dispatch. A transfer already in progress when a policy
changes can finish.

`[rule.updates].feed-url` selects an HTTP(S) metadata endpoint for native
application releases. The selected native archive must carry a SHA-256 digest
and use either an approved same-origin HTTPS destination or the fixed GitHub
release destination. Policy withdrawal or a feed change invalidates an active
offer. AI runtime updates use a separate channel.

`[rule.terminal].session-restore-output` locks encrypted terminal output recovery
while retaining the personal preference. Capture and recovery require an
unlocked vault. The recovery snapshot caps output per pane and across the
layout, excludes incognito tabs, and authenticates the layout with the saved
output. Project exports and profile backups exclude the output. Disabling
recovery output clears displayed recovery previews and removes the encrypted
output from the saved snapshot.
The manual restore offer includes a vault unlock action when saved output is
available. Automatic restore skips the output and shows a notice if the vault
is locked.

Native acceptance still requires connection editing and launch, Control startup
and locked controls, live SFTP under installed transfer denial, the managed
update endpoint, and recovery across restart and vault lock. The porting plan
tracks the residual policy schema audit and user-deferred hardware checks.

The final Windows ARM64 suite passed 3,316 tests with 54 ignored. This
includes a real local Control listener, loopback HTTP update metadata,
the SFTP worker and restore routes, and encrypted recovery and purge.
Clippy passed with warnings denied. The macOS ARM64 all-targets
cross-check passed. See the
[verification record](target/verification/20261112-step2-connection-policy.md).

### SFTP file operations and server-only archives

An attached SFTP session now borrows up to two transfer lanes. A managed cap
of one locks the parallel transfers setting and retains the personal value.
Reducing the effective cap lets active lanes finish before additional lanes
open. Standalone SFTP uses its configured effective cap.

**Settings → SFTP** saves resume and cancellation preferences and supports
reviewed import and export of Java SFTP preferences in `globalSettings` XML.
`sftpResumePartialTransfers` defaults to true. `sftpKeepPartialOnCancel`
defaults to false and takes effect only while resume is enabled. Turning resume
off releases retained transfer offers and history pins and changes queued
retries to fresh copies. Interrupted transfers can keep their verified partial
data when resume is enabled. An explicit cancel also requires the keep setting
for retention. Remote stage removal requires the verified route and stage
ownership. After a disconnect, a durable discard receipt can defer cleanup
until the same route is verified again. A failure that prevents safe cleanup
can leave a private staging path for recovery.

The ordinary SFTP browser opens regular images through the bounded image
viewer and accepts regular binary files in the existing private external
editor workflow. External editing retains the 2 MiB limit and the checked
writeback and recovery flow. The built-in text editor and Linux elevated text
writeback retain their text validation. Transfer permission governs external
editing. The image viewer follows the managed editor-load rule.

Reviewed replacements recheck the target and protect rollback paths. SFTP
still lacks a portable operation that combines the expected-target comparison
and publication. Broader scheduled changed-file replacements and privileged
existing-target transfers remain gated on this limit.

The browser and Scheduler create ZIP, TAR, TAR.BZ2 and 7z archives on the SSH
server through the installed archive helper. The helper reviews the source
inventory, rechecks it at application and publishes to a new destination.
The command channel exchanges review and outcome data, and file content stays
on the server. This path is available under file-transfer denial when the route
and helper are allowed. Failure of the helper does not trigger client-side
archive creation. The Fedora archive helper suite passed 17 tests and Clippy.

On Unix the Scheduler can upload a single reviewed regular file of up to
256 MiB using native rsync over the pinned SSH route. The approval binds the
executable, source digest and exact destination. korTTY verifies the private
server stage over SFTP and publishes it with an atomic no-replace hard link.
The rsync download route remains available. Q1 tracks directory and multi-source
transfers, optional delete, per-target download trees and Windows rsync.

The Scheduler has typed Linux sudo actions to create a directory, change mode
or ownership, and copy one protected regular file to a new path. The review
binds the operation and saved SSH route, and the approval is device-local.
The fixed helper checks protected paths and verified file identities. Copy
keeps its payload on the server and publishes a new private file. Q2 tracks
privileged rename, delete, recursive copy, archiving and existing-target
transfers.

Native acceptance still covers interruption and restart, parallel-lane and
collision behavior, preference exchange with Java, image and external editor
actions, installed-helper operation under transfer denial, and actual sudo
authorization. The Step 2 and Step 3 source changes await package verification
under V7.

The Windows ARM64 workspace suite passed 3,328 tests with 54 ignored. The
Fedora archive suite, OpenSSH file-editing fixture, and native rsync upload
and download with the application bridge passed. See the
[verification record](target/verification/20261112-step3-file-operations.md).

### Interactive Swarm, reviewed AI providers and persistent indexes

Completed interactive Swarm model rounds can save their masked results in
Saved Chats. A reopened conversation can search stored turns and export
Markdown, text or PDF. Continuation rechecks the chosen saved SSH routes and
working directories. Connection groups can select targets and appear in the
stored target metadata. Swarm journal defaults persist in the profile. The
current round shows a summary usage estimate and links completed journals.

Standalone Agent Ask and Run can explicitly select a personal HTTPS profile
with endpoint trust and the additional Agent opt-in. Each request reviews the
masked text and exact destination. The route, profile, credential context and
live source are checked again before dispatch. The configured CLI has a Codex
read-only stdin preset. The MiniMax Global preset configures the native
OpenAI-compatible HTTPS transport and requires fresh endpoint trust.

Terminal Chat adds Tavily MCP, Bright Data MCP, Brave Search MCP, SearXNG MCP
and a configured LM Studio Toolpack. The separately approved search sends the
masked query with an allowlist of tools for one reviewed integration. Returned
excerpts require a separate model review. Per-profile request forecasts offer
Estimate, CL100K, O200K, P50K and R50K. The usage ledger records provider counts.

Explicitly reviewed knowledge sources reach 16 MiB for UTF-8 and 50 MiB for
PDF. The local snapshot stores up to 4,096 passages in 48 MiB. Older numeric
and reviewed source limits retain their original ceilings. Optional journal
semantic ranking caches a vector per reviewed card, keyed by the exact source
revision, masked card, journal location and embedding model. Lexical ranking
is available if semantic ranking fails or is disabled.

Remaining work and native verification are tracked in the
[porting plan](PORTING-PLAN.md) and the
[verification record](target/verification/20261112-step4-ai-workflows.md).

### Local terminal isolation and bounded parity audits

Local PTY startup selects the operating-system backend and reports its name
only for an attested session. macOS prepares a Seatbelt profile and verifies
the work area and protected paths before releasing the shell. Windows uses
Process Security Environments and ConPTY with a per-session Job Object.
The selected shell is never used as the trusted startup probe.

The pinned Java comparison records concrete Python bindings, Java private
snippet-library migration, effect-speed interchange, external skill
catalogues and policy schema differences. The managed policy parser now
normalizes the isolation, clipboard and editor enum values like Java.
See the [product audit](target/verification/20261112-step5-product-audits.md)
and [policy audit](target/verification/20261112-step5-policy-audit.md).
The [verification record](target/verification/20261112-step5-isolation.md)
describes the native Windows and Fedora checks and the Darwin cross-checks.

### Native Apple Silicon release acceptance

The signed ARM64 candidate started a native window and a real Seatbelt PTY
from a private profile and exited through the normal desktop path. Five
packaged SSH App Sandbox cases and the startup timeout case passed. Direct
and jump RSA, the packaged process worker's encrypted RSA key, and the
five native SSH wire checks also passed. The archive and nested Hardened
Runtime helper passed strict signature verification and dependency inspection.
The first bundle used ad hoc signing. The repaired snapshot passed the complete
standard workspace suite on Apple Silicon with 3,446 passed and zero failed.
Its Developer ID package passed strict verification, native desktop start
and exit, Seatbelt and packaged SSH isolation. Apple notarized the package,
and Gatekeeper accepted the app with its stapled ticket from the final ZIP.
The native macOS Intel suite and the external SSH review and dependency
remediation in V6 remain open.
[Native Mac suite](target/verification/20261112-macos-suite.md) · [Developer ID package](target/verification/20261112-macos-package.md) · [Notarization verification](target/verification/20261112-macos-notary.md) · [SSH audit](target/verification/20261112-step6-ssh-audit.md)

### Protected file actions, SFTP drag and larger knowledge indexes

The Scheduler can review and save typed Linux sudo rename and delete
operations on a pinned SSH route. Rename needs an unoccupied destination.
Recursive delete verifies up to 128 protected entries before the helper
accepts the commit token and rejects separate mounts. The client compresses
the fixed helper script to fit the existing SSH command limit. Fedora ARM64
filesystem and protocol checks ran inside a private user and mount namespace.
The installed sudo path over authenticated SSH still needs acceptance.

The SFTP workbench queues pane-to-pane transfers against the displayed
directory revision. Windows, macOS, and X11 can route desktop drops onto
the Server listing or a server folder row. A generated draft can create
a new file in a chosen canonical directory on its authenticated server
connection. Local draft replacement reviews the old and new content
digests and optionally creates a new `.bak` without replacing an existing
backup. It keeps the original recoverable if safe publication fails.
Native pointer gestures, live provider and SSH routing acceptance, Wayland
desktop drops, and desktop drag-out remain open.

Local knowledge indexes can review UTF-8 sources up to 50 MiB with bounded
streaming. The local snapshot can store 16,384 passages in 96 MiB, with
exact ranking above the HNSW graph limit of 8,192. Previous approval
bounds remain effective. The source count and Qdrant limits remain in
the porting plan. The Linux packager's optional `--linux-zip` adds a
deterministic portable ZIP alongside its regular TAR.GZ.

See the [verification record](target/verification/20261113-parity-next.md).

### Local SFTP drops, protected folder copy and Linux packages

Desktop drops in the SFTP Local pane copy files and folders through anchored
local filesystem handles. Copies choose a numbered name on a collision,
skip same-directory input, refuse a folder's own descendants, and can be
cancelled. No SSH worker is needed for this local route. Native desktop
gesture acceptance and the Wayland data-device path remain separate.

The AI draft can preselect the right compared local file and review its
original bytes, identity and optional backup. The comparison refreshes the
same pair on the original live connection after publication, subject to
current policy and saved-byte verification. Existing server-file replacement
remains open.

Scheduler protected folder copies stage a root-owned tree with private modes
and refuse links, mounted subtrees, changed sources, occupied destinations and
the source's own subtree. The optional native DEB and RPM package builders
use the current host's distribution tools and share the release staging with
the portable archives.

See the [verification record](target/verification/20261113-local-drop-and-copy.md).

### Privileged Scheduler archives

The Scheduler can save a passwordless Linux sudo approval for server-only
ZIP, TAR, TAR.BZ2, or 7z creation. The approval binds the exact archive
specification, saved route and host keys. Portable job backups omit the
device-local receipt. The exec-only SSH transport rechecks authority before
authentication, review and commit. The server validates the configured roots,
protected path components and mount boundaries, and returns a one-session
challenge. The helper publishes to a vacant name after rechecking the
inventory and configuration.

Installation uses a dedicated root-owned helper and command-specific sudoers
rule. Password-protected archives, vault-based sudo and local download are
separate acceptance and implementation items.

See the [verification record](target/verification/20261113-privileged-archive.md).

### Startup recovery, native desktop actions and larger knowledge stores

The startup recovery worker closes abandoned native interactive captures
independently of AI summary approvals. It verifies the effective root,
private metadata and existing writer lock before recovering the command count
and end time. Windows Quick Select can open local files in native PowerShell
and CMD panes, and the bundled macOS app has a separate status item for New
Window and Quit.

Knowledge stores admit up to 5,000 files with paged folder reviews. The local
and Qdrant snapshots admit up to 16,384 passages under the existing byte
bound. Earlier automatic discovery and Qdrant approvals retain their limits.
The expanded Qdrant receipt binds the covered source positions without
changing the legacy receipt or standing route encoding.

The SFTP AI comparison offers a review to replace the right server file
through the native Linux helper, optionally keeping a new `.bak`. The helper
checks the exact original and draft bytes, permissions and identity, claims
the output names without overwriting other entries, and reports recovery
after an uncertain result. The same comparison reloads the exact saved bytes.

See the [verification record](target/verification/20261113-journal-desktop-knowledge.md).

### macOS activity during terminal, Scheduler and AI work

Recent terminal I/O owns a 60-second native activity lease. Running Scheduler
jobs and active AI requests hold independent scopes through completion or
cancellation. The scopes permit system idle sleep. The separate manual sleep
preference still uses the existing IOKit assertion. The Apple Silicon app and
component passed strict Clippy as a cross-check. Native background observation
with the packaged app remains open.

See the [App Nap verification](target/verification/20261113-macos-app-nap/README.md).

### Incoming Wayland file drops in SFTP

The existing clipboard data device handles Copy offers from desktop clients.
The active SFTP window resolves the compositor position against its current
pane or scrolled folder row. Offer and window generations, current listings,
modal ownership and the existing transfer policy gate admission. The worker
reads a bounded `text/uri-list` asynchronously and confirms the offer after
the Local copy or Server upload queue accepts it. Local paths must have a
local file URI, and duplicate or unsuitable source entries cannot become
duplicate transfers. A pointer-only seat can register the data device.

On Fedora ARM64, a GTK4 source transferred two file URIs to an Iced
destination through Mutter 50.6 with XWayland disabled. Application tests
covered scrolled folder selection and stale native-window generations.
Full SFTP desktop interaction with a file manager and remote server, and
desktop drag sources on macOS remain open.

See the [Wayland verification](target/verification/20261113-wayland-sftp-ingress.md).

### Local SFTP drag-out on Wayland

The Local pane offers a drag grip for selected ordinary files and folders.
The grip matches the original compositor pointer serial and exposes a
bounded `text/uri-list` Copy offer through the clipboard worker. The regular
pane gesture still transfers between the SFTP panes. Fedora ARM64 verified
the source protocol with an independent GTK4 target, two files and their
contents on Mutter 50.6 with XWayland disabled. Application grip acceptance
and the other platform drag sources remain open.

See the [Wayland drag-out verification](target/verification/20261113-wayland-sftp-drag-out.md).

### Prepared SFTP Server drag-out on Wayland

The Server pane prepares the current selection on a separate channel of the
verified SSH connection. It checks regular files and resolved links, enforces
the Java selection and byte limits, and writes private temporary files under
the desktop runtime directory. The original listing and connection and the
effective file transfer rules govern the prepared selection and its native
offer. The separate Prepare action lets the later drag use a live pointer
press. Successful drags retain the temporary files for a short consumer
read period before cleanup.

Fedora ARM64 exercised the OpenSSH SFTP subsystem with regular files and
link targets, checked the byte cap, and verified private modes and cleanup.
See the [Server drag-out verification](target/verification/20261113-wayland-sftp-server-drag-out.md).

### Terminal Remote files drag-out on Wayland

The terminal's Remote files sidebar stages selected regular files through
the same verified SFTP download and native Copy source as the Server pane.
The current SSH session, sidebar generation, listing, folder and selection
must match when preparation finishes and when the user starts the drag.
The sidebar keeps its existing row navigation and transfer actions.
The drag grip uses the native pointer press after preparation and the
same short consumer read period for temporary files after the drop.

Fedora ARM64 passed the sidebar selection and shared OpenSSH staging
regressions. See the [sidebar drag-out verification](target/verification/20261113-wayland-sidebar-drag-out.md).

### SFTP and terminal Remote files drag-out on Windows

The Local and Server panes and the terminal Remote files sidebar can offer
native Windows Copy drags. OLE exposes the selected paths in Unicode and
checks the original source window, the live primary press and cancellation.
The remote controls use the same bounded verified SFTP preparation as Wayland.
They stage through an owner-only Windows directory, check the stage identity
before offering it and retain successfully copied paths for delayed reads.

The Windows ARM64 module tests verified Unicode path decoding through the
Shell and checked cancellation and ownership of the returned data. Full
product acceptance with Explorer and another independent drop target remains
open. The [verification record](target/verification/20261113-java-chat-environment/verification.md)
also covers application routing and staging.

### SFTP and terminal Remote files drag-out on X11

The Local and Server panes and the terminal Remote files sidebar share the
Linux file drag controls. On X11, the application keeps the existing
pointer gesture and offers `text/uri-list` through the Xdnd Copy protocol.
The worker observes the current target, waits for the release and matching
Copy response, and uses X-Resource 1.2 to check the client requesting paths.
X11 ClientMessage fields do not authenticate senders. Other privileged
clients on the same X server can observe or manipulate desktop state.

A native Fedora ARM64 Xvfb fixture exercised the original primary-button
grab, two exact file URIs, Copy completion, and rejection of pre-drop and
different-client requests. Full product acceptance with a desktop file
manager remains open. See the [X11 verification](target/verification/20261113-x11-file-drag/verification.md).

### Java profile backup migration

Import Backup detects Java AES-256 and legacy ZipCrypto archives. The preview
reports supported connection profiles, snippets and variables, custom themes,
project layouts, and any omitted members. Startup adds the reviewed data and
preserves existing entries at conflicts. The Windows ARM64 backup suite passed
57 tests and the backup dialog filter passed five. The migration fixture verifies
startup replay twice. Java-generated fixtures separately verify master password,
secret fields and temporary SSH key decryption. GnuPG backups encrypted to one supported recipient use the same
additive preview. The native application backup filter passed 59 tests,
including default and compressed GnuPG fixtures, wrong passphrases and modified
integrity protection.
The vault can seal an additive batch without changing the live file, then
publish it under the vault lock against an exact encrypted revision. It also
recognizes an already installed batch on retry. The native vault suite passed
ten tests with strict Clippy. The Java password review now uses this path for
stored SSH passwords and inline target and jump-host passwords. The preview
unlocks the Java credentials separately from the archive. Startup binds exact
new routes, preserves existing tunnel approvals and checks the vault and
connection catalogue revisions. An unstarted stale plan has an explicit
discard action in the startup error screen. An already committing plan can
replay the same sealed vault and references. The managed-key extension below
adds private key copies and their passphrases to this path.
The final Windows ARM64 application suite passed 3,007 tests with 57 ignored.
The recovery fixture covers stale catalogue refusal, exact discard before the
first write and replay after commit. Seven backup dialog tests cover both
preview outcomes. The startup dialog still needs interactive desktop acceptance.

See the [password ZIP verification](target/verification/java-profile-additive-20261113-ebd3bc91d56c.txt),
[GnuPG verification](target/verification/java-openpgp-20261113-585dfb7a4a3e.txt),
[vault verification](target/verification/20261113-vault-additions.log) and
[Windows password restore tests](target/verification/20261113-java-password-restore/windows-arm64-tests.txt).

### Java managed SSH key restore

The Java backup preview offers separate review of unencrypted managed private
keys or a combined review with the source master password and an unlocked
destination vault. Only exact archive-managed copies are eligible. The native
SSH parser checks each private key offline. The import seals eligible
passphrases and stored SSH passwords in one vault batch. It assigns fresh
native key IDs and binds active Java key references to exact new SSH routes,
including an independently supported jump-host password in the same profile.

Startup preflights the current vault, connection and key catalogues, and each
new managed path before committing. It preserves existing native records and
replays exact already-installed files. Keys without a vault use the same
startup path. Temporary keys, external key paths, public companions, host
pins, Scheduler credentials and additional Java profile stores remain outside
this import. Native desktop review and end-to-end use of imported credentials
against a live server remain acceptance cases.

On Windows ARM64, the final application suite passed 3,020 tests with zero
failures and 57 ignored. Strict application Clippy and formatting passed.
The native startup fixtures exercised the vault-free path, a protected key and
jump password in one connection, preservation of existing vault and key records,
refusal of stale catalogues and public companions, and replay with one key file
already installed. See the
[verification record](target/verification/20261113-java-managed-keys/verification.md).

### Java saved AI chats and credential environments

Java profile restore reads the same bounded JAXB chat records as the standalone
Saved Chats import. It retains the original UUID, conversation, and supported
Agent steps after applying the current chat redaction. Source profile and Agent
references remain display provenance. Startup validates the imported record
shape, checks the native chat capacity and installs new records by private
no-replace publication. A local chat with the same ID takes precedence.

The environment converter retains Java custom IDs, labels, and supported
built-in and custom tab colors. The same IDs already stored in imported vault
entries resolve through the restored catalogue. Existing definitions and colors
take precedence. Both datasets replay additively alongside ordinary profile
data and the prepared SSH credential transaction. Native desktop acceptance
and the remaining Java profile stores stay open. See the
[verification record](target/verification/20261113-java-chat-environment/verification.md).

### Java Swarm archives and SSH host keys

The Java profile backup preview recognizes Swarm conversations. Saved Chats
opens them as read-only archives with individual messages, unfinished prompts,
server outcomes, elapsed seconds and token counts. Imported records have stable
archive identities independent of local Swarm runs. The Java and native backup
paths preserve those records and existing local conversations.

The SSH host key review is a separate action in the Java backup dialog. It
decodes the Java Properties store, verifies key and fingerprint consistency
and quarantines conflicting aliases. Each new endpoint starts unselected.
The native TrustStore rechecks approved endpoint and key pairs under its lock
and preserves local pins, including concurrent additions. Queueing a Java
profile restore alone does not publish any host key.

The Windows ARM64 application suite passed 3,042 tests. The separate SSH
trust filter passed eight tests, including concurrent changes to the local
pins. The Swarm fixture passed Java and native backup restoration and the
Saved Chats open and delete path. See the
[verification record](target/verification/20261113-java-swarm-host-pins/verification.md).

### Read-only Journal Chat tabs

Open in AI Chat checks the selected journal location, card identity and saved
chat's reciprocal link. When the saved chat is unavailable, it opens the
journal's bounded transcript in a separate read-only tab. Selection and copying
use the active tab and current journal and clipboard policy. The existing
timeline expansion and citation navigation also remain available. The Windows
ARM64 application passed five Agent chat tests and the session journal filter
passed 270 tests with four ignored. The route fixture checks stale selection,
opening without a saved copy, editing refusal, clipboard policy and closing the
resulting tab. The combined Windows ARM64 application suite passed 3,001 tests
with 57 ignored, including the Java OpenPGP preview. The vault publication API
was verified separately after the application build.

See the [Agent chat tests](target/verification/20261113-journal-transcript/windows-journal-agent.log)
and [session journal tests](target/verification/20261113-journal-transcript/windows-session-journal.log).

## Desktop and application workflows

| Java reference area | Rust implementation at baseline | Verification and remaining acceptance |
| --- | --- | --- |
| [Main window][java-main], [menu and macOS Dock][java-menu], [appearance][java-appearance] | Partial. Native menu/action/connection bars, toolbar customization and overflow, macOS system menu, dashboard, palettes, hosted tools and detached windows. Menu entries disabled at baseline include Saved Chats, AI Agent, AI Planning, AI Swarm, AI Agent Design and Teamwork Settings. | Compare action order, icons, sizing, overflow, shortcuts, enablement and keyboard navigation on all four desktop environments. Repeat menu recovery and macOS Dock reopen with windows closed. Audit every pinned menu entry and its toolbar equivalent. |
| [Window settings][java-window], [terminal sessions][java-terminal], [projects][java-projects] | Partial. Multiple windows, nested splits, native drag, per-window chrome, saved geometry, drafts, project/session restore, and Java project exchange. | Historical macOS and Fedora drag evidence and Fedora Mutter size restoration for Settings and AI Manager. Run the window procedures below for every tool and supported window system. Compare modal ownership, cancellation, focus, and saved dimensions. |
| [Settings inventory][java-manifest], [enterprise policy][java-policy] | Partial. Global Settings has Font, Colors, Themes, Appearance, Terminal, Messaging, Video, Backup, Logging, Export, Window, Keyboard, and Editor and Snippet Editor appearance. Security and AI also have dedicated entry points. Audit Resources, Updates, Language, Translation, SFTP, and snippet-specific appearance profiles. | Walk the manifest's `settings.*` ownership groups. Record the Rust entry point, default, scope, persistence, and policy behavior for each setting. Restart after Save, test Cancel, and compare a fresh profile. Keep application text English. |
| [AI settings and profiles][java-ai], [local models][java-models], [RAG][java-rag], [AI skills and memory][java-manifest] | Partial. General, Profiles, Local Models, Knowledge Stores, AI Skills, AI Memory, and Local Preferences. External local endpoints, signed offline runtime installation, private GGUF import, managed text profiles and request leases, wizard, skills, memory files, and persistent embedding indexes. Search reveals existing fields. Managed text models support idle unload and on-demand reload. Online updates, MLX, and remaining editor and approval flows need parity checks. | Run `cargo test --locked -p kortty --bin kortty -- ai_manager local_ai`. Run the README release-signed smoke with the target's stable package. Then exercise the seven sections in both window and tab presentation using an enabled test profile. Resize the Skill Markdown editor, test preview, cancellation and dirty close, and restart with saved profiles and test retrieval. Compare local model import, start, stop, and restart with the pinned workflow. |
| [Snippet editor and analysis][java-snippets], [export settings][java-export] | Partial. Snippets, variables, protected archives, persisted single and folder PRE analyses with history, pins and source jumps, and PRE exports in HTML, Markdown and JSON. Native PDF, persisted analysis diagrams, reviewed application, POST reports, verification, and remaining branding/options are open at baseline. | Test with a fresh library containing two analysed snippets and one without analysis. Save, restart, and export without calling AI. Compare single, combined, and ZIP output, skipped entries, full code opt-in and branding. For PDF, inspect pagination, embedded diagram and bookmarks. For apply, reject one proposal, accept another after review, then verify resolved, still-open, and new findings in POST output. |
| [Editor and diagram workflows][java-snippets], [scheduler][java-jobs] | Partial. Native editor and seven bounded Mermaid families. Scheduler dependency scenes and local script flowcharts with source jumps. Audit structure navigation, diagram persistence, and remaining scheduler actions. | Run `cargo test --locked -p kortty-diagram` and `cargo test --locked -p kortty diagram_window`. Import each family, zoom and select a node, check the source line, and export SVG and PNG with the full graph beyond the viewport. Exercise rejection of directives, callbacks, and external resources. Verify one AI repair at most and the local flowchart fallback. In the editor, jump to a class, function, variable, and placeholder after an edit. |
| [Journal][java-journal], [terminal sessions][java-terminal], [recording and logging][java-manifest] | Partial. Journals, screenshots, PDF export, split and whole-tab recording including visible IBM cells, synchronized playback of interleaved panes, and Java JSONL recording import. Reciprocal Java export, video export, new terminal log recording and automatic retention are open. | Follow README journal, logging, and recording probes. Move a recorded live tab to another window and add a split. Inspect the playback and an exported excerpt. Repeat with real IBM hosts. In a fresh profile verify screenshot analysis is enabled as a preference. Verify an AI call still requires enabled AI, consent, approved endpoint and enterprise permission. Disable the preference and check restart persistence. |
| [Connections, SSH, SFTP, serial, vault and backup][java-manifest] | Partial. Saved SSH and serial connections, SSH imports/exports, host key pins, process workers, jump hosts, SFTP, fourteen forwarding streams per pane, serial character encodings, vault and encrypted backup. One new local file download can resume after reopening the SFTP window in the same application run. Persistent, upload and folder resume, the replacement race, serial library organization and interchange, remaining enterprise rules and remaining SSH auth and key algorithms are open. | Run the README OpenSSH, SFTP, vault, serial, and backup smoke tests. Interrupt and resume a download through the desktop. Verify changed host keys are rejected before authentication and unsupported required isolation fails before launch. Exercise actual serial break, modem lines, and flow control with a physical device. |
| [Session isolation][java-isolation], [enterprise policy][java-policy] | Partial. Separate SSH worker and platform isolation paths. Local PTY sandbox backends exist for Linux, macOS and Windows. | Run local-shell containment on native target systems and repeat the README packaged SSH sandbox tests on native Windows x64 and macOS Intel. Exercise denied file, network and subprocess access and successful approved traffic. Independent SSH security review is a separate release gate. Audit the target dependency trees against [the vendor patch](vendor/sunset/KORTTY-PATCH.md). |
| [Terminal effects, diagnostics, CLI and Control API][java-manifest] | Partial. Built-in terminal effects, session-process diagnostics, CLI and local Control API. Audit the remaining Java plugin and diagnostic capabilities. | Compare effect selection, rendering, cleanup and diagnostics in both apps. Exercise API consent, audit and policy in an isolated profile. Inventory any capabilities still tied to JAR/JMX in the reference and provide native equivalents. |
| Other [manifested workflows][java-manifest] | Audit. Reconcile shell integration, terminal notifications, highlighting, file browsing, hardening, AI assistant/tools, coding agents, and remaining menu or settings pages with actual Rust entry points. | Expand these rows to named actions as each comparison is made. Attach an executable test or an explicit prerequisite to each acceptance case. |

## Terminal profile inventory

The 19 stream identifiers come from [`TerminalProfile`](crates/terminal/src/lib.rs).
The two IBM families have device variants in [`kortty-ibm`](crates/ibm/src/lib.rs).
All 21 families still require the per-profile acceptance procedure below.

| Profile | Rust implementation at baseline | Existing evidence and acceptance gap |
| --- | --- | --- |
| Xterm | Present (`xterm-256color`) | [Pinned replay](crates/terminal/tests/fixtures/sithtermfx/README.md) and core protocol tests. Complete GUI and scheduler comparison. |
| DEC VT100 | Present (`vt100`) | Pinned replay, including documented DEL difference. Check display, keys and modes through GUI and scheduler. |
| DEC VT102 | Present (`vt102`) | `terminal` tests for editing and device identity. Add pinned wire and UI comparison. |
| DEC VT220 | Present (`vt220`) | `terminal` tests for conformance, C1, multinational GL/GR and selective erase. Add pinned wire and UI comparison. |
| DEC VT320 | Present (`vt320`) | `terminal` tests for identity, conformance and Latin-1. Add pinned wire and UI comparison. |
| DEC VT420 | Present (`vt420`) | `terminal` tests for margins and rectangular operations. Add pinned wire and UI comparison. |
| DEC VT520 | Present (`vt520`) | `terminal` tests for conformance, extent and selectable identity. Add pinned wire and UI comparison. |
| Sun CDE | Present (`dtterm`) | `stream_profiles` identity, color and keys. Add pinned stream and UI comparison. |
| Wyse 50 | Present (`wy50`) | [Pinned replay](crates/terminal/tests/fixtures/wyse/README.md) for addressing, editing, status and keys. Complete UI comparison. |
| Wyse 60 | Present (`wy60`) | Pinned replay for attributes, wrap, ID and keys. Complete UI comparison. |
| Wyse 160 | Present (`wy160`) | Pinned replay for columns, raw address bytes, color, ID and keys. README includes a desktop `$TERM` probe. Complete UI comparison. |
| TeleVideo 910 | Present (`tvi910`) | [Pinned Java output from synthetic input](crates/terminal/tests/fixtures/tvi/README.md) at all input splits. Complete UI comparison. |
| TeleVideo 920 | Present (`tvi920`) | Pinned Java output from synthetic input at all input splits. Complete UI comparison. |
| TeleVideo 925 | Present (`tvi925`) | Pinned Java output from synthetic input at all input splits. Complete UI comparison. |
| HP 2392 | Partial (`hp2392`) | [Pinned mode captures and HP manual cases](crates/terminal/tests/fixtures/hp/README.md) plus native block form PTY test. Verify additional hardware functions and a device or remote HP application. |
| HP 700/92 | Present (`hp700-92`) | Pinned mode captures and ANSI/native switching test. Complete wire and UI comparison and real HP verification. |
| SCO ANSI | Present (`scoansi`) | `stream_profiles` identity, color and keys. Add pinned stream and UI comparison. |
| DEC CTERM | Present (`cterm`) | `stream_profiles` identity, color and keys. Add pinned stream and UI comparison. |
| PETSCII | Present (`petscii`) | [Pinned Java stream and key replay](crates/terminal/tests/fixtures/petscii/README.md) for character modes, editing, colors, wrap, scroll and bell. Complete GUI and scheduler comparison. |
| IBM TN3270 | Present (`IBM-3278-2`, `IBM-3278-2-E`) | `kortty-ibm` screen tests and TCP loopback negotiation, traditional and enhanced framing, fields, AIDs and response sequences. Verify real IBM host interoperability. |
| IBM TN5250 | Present (`IBM-3179-2`, `IBM-3180-2`) | `kortty-ibm` screen tests and TCP loopback display negotiation, 24×80 and 27×132, Attention and System Request. Verify real IBM host interoperability. |

### Terminal acceptance procedure

1. Run `cargo test --locked -p kortty-terminal --test reference_replay --test terminal --test stream_profiles --test wyse --test tvi --test hp` and `cargo test --locked -p kortty-ibm`. Regenerate reference captures with the harnesses linked above. Add the missing pinned fixtures for each profile using the documented emulator and key APIs. Compare cells and styles, grid and pixel dimensions, cursor, modes, replies, event order, character sets, and keyboard bytes. Feed identical input whole, in chunks, and byte by byte. Record intentional differences with the relevant reference and a regression test.
2. For each stream profile, select it in the GUI, record the advertised terminal ID, feed the capture over a PTY or SSH, and send the corresponding keys. Compare the display and bytes with the reference. For scheduler-supported profiles, replay through a virtual terminal job and compare its frame and transcript with the same terminal core result. Exercise alternate screen, scrolling and reflow, search and selection, OSC 8, mouse protocols, typeahead, zoom, idle compression, and process continuity where supported.
3. Use one authorized test host per IBM family. Select each supported device model, check protection against editing, numeric input validation, editable fields, cursor navigation and AID keys, and record both directions of synthetic test traffic. Verify required plaintext opt-in before a direct dial and verify SSH forwarding where available. Record the server product and version along with the outcome. Loopback remains separate evidence.
4. Run the README Kitty probe over PTY and SSH. Extend it across direct and chunked RGB, RGBA and PNG, zlib, placement layers, moving tabs, scrolling, and reflow. Verify OSC 5522 MIME negotiation, chunking and paste events in the core. Verify disabled-by-default desktop access, consent, internal mode, enterprise policy, strict filtering and the 256 KiB UTF-8 limit in the app. Follow malformed, oversized and incomplete sequences with a valid command in the same session. Verify audible bell behavior on Windows and Linux with an actual audio device.

## Reproducible acceptance runs

Use one shared Cargo target and private profiles per run as required by
[AGENTS.md](AGENTS.md). Start with `cargo fmt --all --check`,
`cargo clippy --locked --workspace --all-targets -- -D warnings`, and
`cargo test --locked --workspace --all-targets`. On macOS and Linux wrap tests
with `bash scripts/with-scratch.sh`, adding `--zsh-tests` on macOS. On Windows
use a private run directory and restore `TEMP` and `TMP` in `finally`.
When launching the GUI, point `HOME` on macOS, `XDG_CONFIG_HOME` on Linux,
or `LOCALAPPDATA` on Windows to the private test directory for that process.

- [ ] **Pinned references.** In isolated source copies, run `mvn -pl sithtermfx-core,sithtermfx-ui -am test` with JDK 17 and `./gradlew terminalNavigationKeysSmoke terminalLinksSmoke terminalSplitTransparencySmoke multiExecSmoke mermaidRendererSmoke snippetDiagramZoomSmoke` in the Java application. Historical macOS evidence was 313 core and 49 UI Maven passes and six completed Gradle tasks. The link task skipped the focus assertion, which needs a focused rerun.
- [ ] **Live window transfer.** Repeat on macOS, Windows, native Wayland, and native X11. Open two windows, start output in a PTY (`echo $$` in a POSIX shell or `$PID` in PowerShell), add two nested splits, and focus the original pane. Move the tab by the menu and by native drag while output continues. Check PID, retained scrollback, pane focus and split ratios, and repeat with a cancelled drop. Verify a blocked broadcast target leaves a second pane responsive. Use the README platform-specific drag and ConPTY probes. Xwayland evidence is tracked separately from native X11 acceptance.
- [ ] **Geometry and ownership.** Open each settings, connection and tool dialog on a fresh small screen and reach every section and footer button with the keyboard. Resize, close and restart. Move from a large monitor to a small one and back at multiple scaling factors. The dialog should fit temporarily and recover its preferred dimensions. On Wayland observe compositor placement and test the Mutter auto-maximize case without changing desktop settings. Repeat with multiple owners, modal children, detached windows and hosted tabs. Cancel a close with unsaved edits and check both drafts and focus.
- [ ] **Platform packages and performance.** Use the [README package commands](README.md#native-packages), verify checksums, extract into the run directory and start on clean target systems. Record runtime dependencies and whether the process exits cleanly. Compare optimized Java and Rust on the same machine and workload. Require faster startup or lower idle memory with p95 input latency no worse. Exercise input, menus, selection and resize in a maximized macOS 5K window and run `terminal_row_damage` at Retina scale. Repeat native PTY/ConPTY and sandbox acceptance on the appropriate architectures.
- [ ] **Release evidence.** For each result record revision, package checksum where applicable, OS and architecture, session type, compositor, scaling, commands or UI steps, outcome, and limits. Attach the CI run and separate native acceptance results. Include the independent SSH security review, real IBM host checks, physical serial and HP checks, and unresolved prerequisite owners before closing release acceptance. Clean the run and build artifacts per [AGENTS.md](AGENTS.md).

[java-manifest]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/doc-manifest.yaml
[java-main]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/getting-started/main-window.md
[java-menu]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/reference/menu.md
[java-appearance]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/reference/settings/appearance.md
[java-window]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/reference/settings/window.md
[java-terminal]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/terminal.md
[java-projects]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/projects.md
[java-policy]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/reference/enterprise-policy.md
[java-ai]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/reference/settings/ai.md
[java-terminal-ai]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/ai-assistant.md#terminal-selections
[java-models]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/local-models.md
[java-rag]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/rag.md
[java-snippets]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/snippets.md
[java-export]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/reference/settings/export.md
[java-jobs]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/jobscheduler.md
[java-journal]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/session-journal.md
[java-isolation]: https://github.com/chardonnay/korTTY/blob/1ebbe724ee84/app-docs/site/docs/en/features/session-isolation.md
[sith-guide]: https://github.com/chardonnay/SithTermFX/blob/8b02de38b7c6396f1af2558b0b00477cfb9a2871/docs/integration-guide.adoc#L1068
[sith-changelog]: https://github.com/chardonnay/SithTermFX/blob/8b02de38b7c6396f1af2558b0b00477cfb9a2871/CHANGELOG.md
[handoff-ci]: https://github.com/chardonnay/korTTY_rust/actions/runs/42430876231
[head-ci]: https://github.com/chardonnay/korTTY_rust/actions/runs/42431985985
