# Chat delegate: read-only background jobs for a worker's Chat tab

Card AMUX-5432. Owner request, 2026-10-01 10:00 and 10:02.

## Why

Ethan asked the `social-activities` Chat "what is the dress code for the
brownstone today?". The terminal worker could have fetched the event page in
one step. The Chat could not: its tools are narrower than the worker's
(WebFetch was denied), so it told Ethan to look it up himself.

His rules for the fix:

- Chat stays a Q&A surface. Ethan can still talk to the worker terminal.
- Chat may do real work through the worker's own coding agent, read-only.
- It must not interfere with the worker, whether the worker is busy or idle.
- It must be model agnostic: claude, codex, gemini, whatever the worker runs.
- When a question only the worker's terminal can answer comes up, the Chat
  follows a ladder, cheapest and least intrusive first (10:02).

## What a delegate is

A delegate is a short-lived JOB: one non-interactive run of the parent
worker's own provider CLI, read-only, in its own process group, answering one
question. It is not a ninth primitive and not a worker: it has no tmux
session, no board, no fleet row, and it is gone when it answers.

It is composed from existing primitives (CLAUDE.md "Primitives"):

| Need | Primitive used |
|---|---|
| who it works for, its tools, model, scope env, vault | the parent **worker**'s env and **environment** layers |
| the job's prompt, output and status | **filesystem** (`~/.amux/chat-state/delegates/<job>/`) |
| the answer reaching the Chat | **messages** (a queued chat message, origin `delegate`) |
| what it is allowed to read | the worker's own provider permissions, narrowed |

## The job spec

```json
{
  "id": "dg-01K…",
  "worker": "social-activities",
  "chat": "social-activities@chat",
  "provider": "claude",
  "mode": "fork | fresh",
  "prompt": "what is the dress code for the brownstone today?",
  "cwd": "<the worker's CC_DIR, the LIVE checkout>",
  "read_only": true,
  "timeout_s": 300,
  "max_usd": 1.0,
  "wait_until": 1790863800
}
```

The server turns the spec into the provider's own non-interactive command
(`provider_command`, unit tested per provider). The prompt goes in on stdin,
never argv. Vault secrets reach the process environment the way they reach a
worker launch, never argv.

## The escalation ladder the Chat follows

1. **Read what the terminal already produced.** The Chat itself, with the amux
   CLI: `amux peek <worker>` (pane history), `amux info`, `amux board ls`,
   `amux get` (any read-only API path: messages, history, email inbox,
   calendar), its last reply and recent commits. Most "what is it doing, why,
   how far" questions end here.
2. **Uncommitted work, read-only.** A delegate runs with its working directory
   set to the worker's LIVE checkout, so it sees in-progress edits. It cannot
   write there (see Isolation) and `GIT_OPTIONAL_LOCKS=0` stops `git status`
   or `git diff` from refreshing the shared index.
3. **The agent's working context.** Mode `fork`: on claude the delegate
   resumes the worker's conversation with `--fork-session
   --no-session-persistence`, a separate process on a copy of the context that
   is never written back. The worker's session file stays byte-identical.
   Providers with no fork (codex, gemini) get mode `fresh` seeded with the
   worker's recent pane history and last reply. Either way the answer is
   labelled as the forked or reconstructed view, never as the live agent.
4. **Only the live agent will do.** The Chat says so and offers the owner two
   explicit choices: ask the worker directly, or queue the question for the
   worker's next idle turn boundary through the existing message queue. Never
   automatic, never mid-turn.

## The four questions

### 1. Where do the permissions come from?

From the parent worker, narrowed to reading. The delegate gets the worker's
provider, model, scope environment and vault secrets, so it can reach what the
worker reaches. Writes are taken away three ways, so no single layer has to be
trusted:

| Layer | What it stops | Applies to |
|---|---|---|
| Provider CLI read-only mode | edit and write tools, shell writes where the CLI supports it | per provider, see table |
| OS sandbox (`sandbox-exec`, macOS) | any file write under the worker's checkout and its git directory | every provider |
| amux API guard | any non-GET request from identity `<worker>@delegate` | every provider |

Per provider:

| Provider | Non-interactive mode | Read-only, enforced by the CLI | Enforced by amux | Fork of the worker conversation |
|---|---|---|---|---|
| claude | `-p --output-format stream-json` | `--permission-mode dontAsk` with an allowlist (Read, Grep, Glob, WebFetch, WebSearch, amux read verbs), `--disallowedTools Edit Write MultiEdit NotebookEdit`, `--max-budget-usd` | OS sandbox, API guard, timeout | yes: `--resume <id> --fork-session --no-session-persistence` |
| codex | `exec --json` | `--sandbox read-only`, `approval_policy="never"` | OS sandbox, API guard, timeout, budget by tokens unavailable | no: fresh, seeded |
| gemini | `--output-format json` | `--approval-mode default` (prompts auto-deny headless) | OS sandbox, API guard, timeout | no: fresh, seeded |
| other | none | none | refused (`chat_delegate_refused`, `provider_unsupported`) | n/a |

MCP servers can write (a mail tool can send), so they are not allowed by
default. `AMUX_CHAT_DELEGATE_TOOLS` (scoped env) adds tool patterns for a
worker whose owner wants a specific read tool.

Where no OS sandbox exists (Linux without one), claude and codex still run,
on their own CLI enforcement plus the API guard; gemini is refused with
`no_read_only_enforcement`, since its headless mode cannot be shown to deny
writes on its own.

### 2. How does the result get back to the Chat?

`amux delegate-job run --worker <w> --wait 120 --stdin` from the Chat's own
turn. Inside the wait, the answer prints to the Chat as the command's output
and the Chat answers the owner in the same turn. If the job outlives the
wait, the command says so and returns; when the job finishes, the server
queues the result into the Chat as a message (origin `delegate`, a header
with job id, provider, mode, duration and cost), which starts a Chat turn
that relays it. A result a waiting caller already read is never re-queued.

### 3. How does it avoid the worker's live edits?

It never writes. It does not send keys or messages to the worker's pane, does
not use the steering queue, cannot mutate the board (API guard), and runs
under its own identity `<worker>@delegate`. Reads of the live checkout are
safe because nothing in the job can write: the OS sandbox denies writes under
the checkout and git directory, and `GIT_OPTIONAL_LOCKS=0` keeps read-only git
commands from touching the index. A committed-HEAD snapshot was the first
idea; it answered from stale code, so the live read-only view replaced it.

### 4. Cost and record

- Timeout `AMUX_CHAT_DELEGATE_TIMEOUT_S` (default 300) kills the job's whole
  process group.
- Budget `AMUX_CHAT_DELEGATE_MAX_USD` (default 1.00), enforced by
  `--max-budget-usd` on claude; measured cost is reported for every provider
  that reports it.
- Record: the job's own directory (spec, output, result), a line in the Chat
  with job id, provider, mode, duration, cost and outcome, and log verdicts
  `chat_delegate_started / finished / refused / timeout / killed /
  reattached`. No board card per job: it links to its Chat turn instead.

## Failure modes

| Failure | What happens |
|---|---|
| server restarts mid-job | the job runs detached, like a Chat turn (61a92bfc); boot re-attaches and delivers once |
| provider CLI missing or broken | job ends `failed` with the CLI's stderr in the Chat line |
| worker has no conversation to fork | mode falls back to `fresh` and says so |
| job exceeds timeout or budget | process group killed, outcome `timeout` or `budget`, partial output kept |
| job tries to write | denied by the CLI, the sandbox, or the API guard; logged |

## Ethos check

- **Rule 1, does capability reach the model:** every Chat gets the verb and
  the ladder in its standing prompt, with no opt-in.
- **Rule 3, a truthful path:** a question the Chat cannot answer now has a
  path other than "look it up yourself", and the one it truly cannot answer
  ends in an explicit owner choice.
- **Rule 5, accumulate or discriminate:** jobs are files that go away; the
  record is a Chat line and a log verdict, not a card per question.
- **Rule 8, the human's decision:** the delegate never writes the owner's data
  and never steers the worker; asking or queueing the live agent stays his.
