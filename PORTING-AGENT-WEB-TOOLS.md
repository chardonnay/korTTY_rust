# Agent web-tool offering: remaining implementation contract

Reviewed on 2026-11-25. This Mac has verified the original Java offer, the
native single-turn transport and interactive Run review against a loopback
model. The next desktop check covers Setup and Pause/Resume. Additional Agent
routes, Scheduler approval scopes, the General preference and configured
services remain open. [Porting priorities](PORTING-PLAN.md#next-milestones-in-priority-order)

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
Private runs, Swarm, final advice and unsupported transports do not enable it.
The selected profile must already permit the direct Tavily integration.

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
was interrupted and its desktop fixture recorded zero model requests. Setup
and Pause/Resume still need desktop acceptance. [Setup candidate checks](target/verification/20261125-agent-setup-paused-notes.md)

## Current native gaps

AI Manager General has native counterparts for 23 of 30 Java controls,
including six nonsecret Internet defaults. The seven outstanding controls are
the global Tavily key, Bright Data token and Brave key, their three Clear
actions and the always-offer checkbox. Java migration and managed-policy
checks remain open for those global settings. [General field inventory](target/verification/20261125-searxng-global-reconciliation.json)

Standalone Run can offer tools on an eligible direct Tavily profile. Ask,
planning/triage, Swarm and Scheduler need route-specific eligibility and
approval scopes for model-proposed calls. The existing user-initiated
search/page actions and approved single-query Scheduler lookup each retain
their own authority.

## Required implementation and acceptance

1. **Verify the Setup candidate on this Mac.** Complete its application suite
   and normal, compact and minimum-size Setup checks. Pause and resume both an
   edited proposal and fetched results awaiting review. Match each restored
   review to its call ID and verify the wire sequence through approval or
   cancellation. [Candidate and desktop steps](PORTING-PLAN.md#next-verification-on-this-mac)
2. **Extend interactive offering to the remaining Java routes.** Compare Ask,
   planning/triage and Swarm with the original-service cases. Determine
   eligibility from request kind and original task. Keep private contexts,
   terminal output and ordinary non-Agent calls within their existing scopes.
   Reuse Run's editable query or exact-page review, masked-excerpt approval
   and per-call identity. Exercise cancellation, stale authority, ordered
   errors, aggregate limits, repair and usage on each newly enabled route.
3. **Define the approval for Scheduler model-driven rounds.** Bind allowed
   queries/pages, target, provider and per-firing budgets before enabling
   dispatch. Exercise recurring jobs and revocation after a profile, mask,
   vault or policy change. Preserve the existing one-query grant for its
   reviewed lookup.
4. **Wire the saved always-offer preference to eligible routes.** Default it
   to off, expose it in General / Internet search and invalidate old authority
   when it changes. Verify Save, Cancel, restart, policy locking and Java
   migration into a profile with local choices. Count this control after both
   the preference and its runtime behavior pass.
5. **Verify configured services and platform behavior.** Compare new routes
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
