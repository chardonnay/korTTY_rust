# Agent web-tool offering: remaining implementation contract

Reviewed on 2026-11-25. This Mac has verified the original Java offer, the
native single-turn transport and interactive Ask, Run, Plan and triage review
against a loopback model. Desktop checks also cover compact Setup,
Pause/Resume and per-member Swarm reviews. Scheduler has direct-tool approval
for guided Agent, iterative Swarm and guided read-only Swarm. The General
preference and configured services remain open.
[Porting priorities](PORTING-PLAN.md#next-milestones-in-priority-order)

## Verified original behavior

Original korTTY Java v2.26.0 defaults `aiAgentAlwaysOfferWebTools` to false. Its
General / Internet checkbox controls model-side tool offering for the direct
Tavily integration. The service factory passes the preference to Tavily tool
instances, including those supplied to embedded llama.cpp and MLX delegates.
MCP configuration follows its own integration path.

The original OpenAI-compatible service was exercised against a loopback model
endpoint in 26 cases. Tavily search and extraction were replaced by in-process
fixture methods; no real Tavily HTTP request or configured model was tested.

| Situation | Verified result |
| --- | --- |
| Agent task without web signals, preference off | No tools offered. |
| Same task, preference on | Both `web_search` and `web_extract` offered. |
| Task mentions current information, documentation or an HTTP URL | Tools offered with either preference value. |
| URL appears only in context after the Agent task line | It does not enable tools when the preference is off. |
| Ordinary non-Agent local prompt, preference on | No tools offered. |
| Native-style JSON containing only a local goal | The Java text heuristic does not identify it as an Agent task. |
| Vision prompt or missing direct-tool integration | No direct web tools offered. |
| Search and extraction requested together | Both execute and their results return as tool messages before the final answer. |
| Five tool calls in one response | Only the first three execute. |
| Repeated tool requests | Two executable rounds, followed by a final request without tools and failed-call metadata for the limit. |
| Unsupported tool name | Error tool result and unsuccessful metadata; no fixture Tavily execution. |

The Agent marker is case-insensitive and handles CR/LF line endings. The
original also considers an empty `User task:` line eligible when the preference
is on; native code should use its explicit request kind and validated goal,
rather than infer authority from a line that untrusted context can supply.

## Native single-turn transport

`Client::complete_agent_turn_before_guarded` can return either text or bounded,
typed web-tool proposals. An explicit request carries assistant/tool history
and chooses whether to offer the two web tools. It uses the existing transport,
lease, model-resolution, compatibility-retry and usage machinery. Ordinary Agent
calls use the same boundary without a tool offer.

Tool histories require complete, ordered call/result identity matches and unique
call IDs. Argument objects and identifiers are bounded; history is capped at
96 KiB and the encoded model request at 256 KiB. At most three returned calls
are retained, with an explicit omitted count. A final request can carry a group
of round-limit error results while withholding further tools. This history
capacity does not itself implement or authorize the two-round execution loop.

The path checks Internet policy, the caller's guard before dispatch and after
the reply, current model credentials and the leased transport. Truncated calls
and credential echoes fail rather than becoming altered proposals. Ordinary
nonstreamed text replies reject actual unsolicited calls while preserving empty
tool arrays; the existing strict stream path remains unchanged.

Twelve new tests passed with actual loopback HTTP requests. The retained wire
matches the original tool names/argument schemas, selection and JSON mode,
assistant/tool role sequence, ordered result IDs and final tool omission.
No Tavily provider was dispatched. These are transport checks, not end-to-end
Agent web execution or installed-runtime acceptance. Other native wire protocols
receive an explicit unsupported response for this direct-tool API.

## Interactive Run orchestration

The Run decision request can offer the two direct tools when the first line
of its original task matches the reference's default signal-word or URL rules.
Eligibility uses the typed goal and the Run request kind.
Private runs, final advice and unsupported transports do not enable it. The
selected profile must already permit the direct Tavily integration.

Each proposal arrives over the existing model progress channel. Its query or
exact page URL uses the existing editable review before provider dispatch; its
masked excerpts require a separate action before they return to the model.
The result records the reviewed input, source context or declined-tool error
under the original call ID. Unsupported requests and the round limit produce
explicit errors without dispatching a provider. The loop permits two executable
rounds of at most three calls and then a final request without tools. Per-result,
aggregate context, history and request limits bound the continuation.

Review serials and provider-request tickets are distinct from the worker epoch.
Late actions and results therefore cannot adopt a subsequent tool review. Model
authority, provider configuration/credentials, vault revision and masking are
checked before dispatch and publication, and approved result guards remain live
through later model requests. Cancellation, expiry or a closed review aborts the
worker rather than supplying invented evidence. Model usage is aggregated across
rounds and compatibility retries; schema repair retains the tool history and
its execution budget.

The suite for this revision passed 4,471 application tests, including nine
new regressions, with 203 ignored. Native desktop checks covered edited search,
exact-page review, both excerpt approvals, a declined page and cancellation in
normal and compact windows. Five retained
loopback model requests verify ordered results and no continuation after cancel.
Provider results came from an explicitly enabled debug fixture. Real Tavily
and installed-runtime acceptance remain open. This run also exposed clipped
Setup actions at 800 × 500, particularly when a finished run could be exported.
The subsequent worktree change puts the Setup body in a scroll area and keeps
the action row visible in all 18 geometry cases. Its full application suite
was interrupted. The merged PR #38 tree later passed 5,145 workspace tests
and live Setup checks at normal, compact and minimum content sizes on this
Mac. Compact Pause/Resume restored both an edited query and loaded excerpts
while model continuation stayed gated by the review. [Desktop and wire evidence](PORTING-PARITY.md#november-25-agent-setup-pause-and-resume-on-macos)

The November 26 minimum-size run found that declining the final tool while
paused sent a follow-up model request. Paused reviews now hold tool decisions
until Resume. The provider may finish a lookup already in flight, and the run
can still be cancelled. [Native follow-up](PORTING-PARITY.md#november-26-minimum-agent-layout-and-paused-tool-decline)

## Current native gaps

AI Manager General has native counterparts for all 30 inventoried Java
controls. The Tavily, Bright Data and Brave fields and their Clear actions
use the encrypted native vault. Tavily and Bright Data use an unlocked saved
credential in their reviewed provider routes. Brave requests select the
configured LM Studio plugin, as in Java v2.26.0. Adoption of encrypted Java
provider keys and acceptance against configured services remain open.
[Native credential acceptance](PORTING-PARITY.md#november-26-global-web-provider-credentials) · [Prior field inventory](target/verification/20261125-searxng-global-reconciliation.json) · [Saved preference](PORTING-PARITY.md#november-26-saved-agent-web-tool-offering-preference)

Standalone Ask, Run, Plan Questions/Options/Report, Run triage and interactive
Swarm members can offer tools on eligible direct Tavily profiles. Guided
scheduled Agent, iterative Swarm and guided read-only Swarm require an explicit
saved opt-in for direct tools. The existing approved lookup workflow and the
interactive user-initiated search/page actions each retain their own authority.

Java Ask sets `queryOnly` and uses the shared decision prompt with the
`User task:` marker. Native Ask keeps its advisory answer format and the
attachment checks before provider and model dispatch. This Mac verified the
edited search, exact page URL, separate excerpt approvals, ordinary Ask without
tool definitions and cancellation with a tool proposal held. [Java decision path](https://github.com/chardonnay/korTTY/blob/b235d91f20356f8a9b33ad5398550a043cc21ed2/src/main/java/de/kortty/core/TerminalAgentService.java#L1738-L1766) · [Native Ask acceptance](PORTING-PARITY.md#november-25-reviewed-web-tools-in-native-agent-ask)

Planning and Run triage use the original task for eligibility. Plan review
revalidates the current route before provider dispatch and model continuation.
Triage returns the approach choice, which leads to a separate Run review.
[Native planning and triage acceptance](PORTING-PARITY.md#november-25-reviewed-web-tools-in-native-planning-and-triage)

Java Swarm sends the original query through the shared Agent service for each
target. Native Swarm offers tools in member advisory and command-review
requests, with a separate model request, query or URL review and excerpt
approval for each member. Direct Tavily HTTP operations have a bounded worker
pool independent of model workers waiting on tool results.
[Java member request](https://github.com/chardonnay/korTTY/blob/b235d91f20356f8a9b33ad5398550a043cc21ed2/src/main/java/de/kortty/core/swarm/SwarmOrchestrator.java#L346-L389) · [Native Swarm acceptance](PORTING-PARITY.md#november-25-reviewed-web-tools-for-interactive-swarm)

## Required implementation and acceptance

1. **Scheduler model-tool revocation accepted on macOS.** Guided Agent and iterative
   Swarm bind individual web requests to the current target and firing.
   Existing grants V1–V5 default to their earlier lookup scope. Guided
   Agent has calendar and profile revocation evidence on this Mac, and
   iterative Swarm has a two-target desktop run with real OpenSSH probes.
   Guided read-only Swarm uses the same per-call approvals and checks the
   target and advisory authority again before model continuation. On this Mac,
   disabling Internet access and changing masking rules closed pending reviews
   with zero model continuations and recorded each target's block reason.
   The guided Agent and iterative Swarm also closed their pending tool reviews
   after Internet-policy or masking changes. The journal identified the cause,
   and neither change let the model continue. After policy restoration a fresh
   Swarm run accepted one target's excerpts and kept the other target's decline separate.
   [Guided Agent and iterative Swarm evidence](PORTING-PARITY.md#november-25-direct-model-web-tool-approval-in-jobscheduler) · [Read-only Swarm evidence](PORTING-PARITY.md#november-25-reviewed-web-tools-for-scheduled-read-only-swarm) · [Read-only revocation follow-up](PORTING-PARITY.md#november-25-scheduled-read-only-swarm-revocation-reasons)
   [Guided Agent and iterative Swarm revocation follow-up](PORTING-PARITY.md#november-26-guided-agent-and-iterative-swarm-web-revocation)
2. **Saved always-offer preference accepted.** General / Internet access
   defaults to off and offers tools for eligible direct Tavily requests when
   enabled. The saved revision invalidates an older review even after
   restoring the previous value. Java migration fills unset settings and
   preserves an explicit native choice. The desktop verified search,
   discard, Save, restart, policy locking and the changed model offer on the
   same local task.
   [Saved preference and desktop model evidence](PORTING-PARITY.md#november-26-saved-agent-web-tool-offering-preference)
3. **Verify configured services and platform behavior.** Compare new routes
   with the retained Java wire cases, then use a configured direct Tavily
   integration and supported OpenAI-compatible and signed GGUF/MLX delegate
   routes. Record actual provider dispatch and model continuation, including
   denial or cancellation. Run macOS cases on this desktop. Windows tests
   resume once the user makes the reinstalled desktop available.

## Evidence

- [Original-service requests and results](target/verification/20261125-agent-web-offer-original-wire.json)
- [Pinned Java and current native source hashes](target/verification/20261125-agent-web-offer-reference.json)
- [Desktop reference observations](target/verification/20261125-agent-web-offer-gui-observations.json)
- [Report and limitations](target/verification/20261125-agent-web-offer-notes.md)
- [Evidence checksums](target/verification/20261125-agent-web-offer-evidence.sha256)
- [Native single-turn checks](target/verification/20261125-web-tool-turns-notes.md)
- [Native/original wire comparison](target/verification/20261125-web-tool-turns-original-comparison.json)
- [Interactive Run checks and limitations](target/verification/20261125-web-rounds-notes.md)
- [Desktop model requests](target/verification/20261125-web-rounds-gui-model-wire.json)
