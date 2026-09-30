# amux for ChatGPT

amux ships a ChatGPT app: a remote MCP server at `/mcp` on your own amux, with
the OAuth 2.1 server ChatGPT needs to connect to it. Source:
`crates/amux-server/src/api/chatgpt_app.rs` (AMUX-5396).

## Connect it (developer mode, works today)

1. Your amux must reach the internet. With an amux cloud tunnel token set
   (`AMUX_TUNNEL_TOKEN` in `~/.amux/server.env`), open **Settings > Integrations
   > ChatGPT** and press **Publish connector**. That starts the tunnel in
   MCP-only mode: it forwards `/mcp`, `/oauth/*` and the discovery documents,
   and answers 404 for everything else, so the dashboard and `/api` stay private.
   To keep it on across restarts, add `AMUX_TUNNEL_MCP=1` to `server.env`.
2. Copy the connector URL shown there. It looks like
   `https://<tid>.t.amux.io/mcp` and stays the same for as long as your tunnel
   token does.
3. In ChatGPT: Settings > Security and login > turn on Developer mode (your
   workspace policy can hide it). Then open Plugins, press +, choose a public
   endpoint, paste the URL including `/mcp`, and save.
4. ChatGPT opens an approval page with a short code. In amux, Settings >
   ChatGPT shows the same code with Approve and Deny. Approve, and the ChatGPT
   page continues by itself.
5. Disconnect any time from the same panel. Its tokens stop working at once.

## The public front door (for the directory listing)

A directory listing names one MCP URL for every user, and that origin can never
change. That URL is `https://cloud.amux.io/mcp`, served by the cloud gateway
(`cloud/gateway/chatgpt_front.py`, AMUX-5397). Built and tested locally; not
deployed, because cloud.amux.io has no host right now (see `SUBMIT.md`).

**Design.** The gateway is the OAuth server ChatGPT talks to, and it is itself
an ordinary OAuth client of the amux the user picks. The user signs in with the
same Clerk account as the cloud dashboard and chooses one amux they may reach:
a cloud workspace they own or administer, or their own machine through its
MCP-only tunnel. The gateway then runs that amux's own OAuth (register,
authorize with PKCE, redeem), keeps the workspace token, and gives ChatGPT a
token of its own that is bound to that one workspace. `/mcp` relays each call
to that workspace unchanged, and the answer, including any refusal, comes back
as written.

Why this one: it needed almost nothing new on the amux side (its redirect
allow-list admits exactly `https://cloud.amux.io/oauth/workspace/callback`, and
its authorize endpoint answers JSON when asked), and every rule amux already
enforces keeps working, including the owner approving each connection.
Proxying ChatGPT's OAuth straight to the workspace was rejected: registration
happens before the user is known, so there is no workspace to send it to.

**Who approves.**

- A machine behind a tunnel: its owner approves the code in that amux's
  Settings > ChatGPT, exactly as for a direct connection.
- A cloud workspace: its dashboard sits behind the gateway and cannot tell a
  relayed approval from a forged one, so the owner's click on the gateway's
  consent page is the approval. Only an owner or admin of the workspace can
  give it.

**Tenant rule.**

- A front door grant is bound to one user, one org and one workspace when it
  is created.
- The picker only offers workspaces the signed-in user may reach, and the
  choose step checks the pick again rather than trusting the form.
- `/mcp` takes the workspace from the grant, never from the request.
- A tunnel is used only while it still belongs to the grant's org. If it
  changes hands, calls fail and are never re-routed.
- The tunnel relay accepts an answer only from the org that owns the tunnel
  the request went down. Before this change, any paying org could answer
  another tenant's pending request.

**Revoking.**

- Signed-in users list and revoke their connections at
  `GET /api/gateway/chatgpt/grants` and
  `POST /api/gateway/chatgpt/grants/<id>/revoke`.
- Revoking on the amux side also ends the connection: the workspace refuses
  the token, the refresh fails, and the gateway revokes its own grant.

Every refusal prints `[chatgpt-front] verdict=<name>`.

**Tests.** `python3 cloud/tests/test_chatgpt_front.py` runs the real gateway
handler against fake workspaces and a fake tunnel client. It needs no cloud,
Clerk, Docker or network. It covers:

- the full OAuth, `tools/list` and `tools/call` path for a cloud workspace and
  for a machine through its tunnel;
- a forged pick of another org's workspace;
- another user continuing your request;
- a plain member connecting a cloud workspace;
- a tunnel changing hands;
- a forged tunnel reply;
- code replay, PKCE failure, refresh rotation, revocation on either side, and
  unlisted redirects.

## Tools

| Tool | Kind | What it does |
|---|---|---|
| `list_workers` | read | Workers with status, groups, model and description. Archived ones only on request. |
| `read_worker` | read | The recent transcript of one worker, up to 400 lines. |
| `list_board` | read | Open board cards, optionally for one worker or one status. |
| `message_worker` | write | Sends a message to a worker, stamped `[amux-origin: chatgpt ...]`. Normal delivery rules apply. |
| `add_card` | write | Creates a board card. Says whether amux created it or folded it into an existing card. |
| `update_card_status` | write | Moves a card to backlog, todo, doing or done. Board gates apply and a refusal comes back as written. The result reads the card back. |

No tool deletes, stops, archives or discards anything. A connection approved
with only `amux:read` cannot use the write tools.

## Security model

- `/mcp` accepts only access tokens minted here, for this exact resource URL,
  unexpired and unrevoked. It sits outside amux's normal auth layer on purpose,
  because that layer admits every loopback request and the tunnel connects from
  loopback.
- Approval is an owner act in the dashboard. The approve route refuses a worker
  origin, a local member, and any request carrying relay or proxy headers
  unless it also presents the owner token or an owner session.
- PKCE S256 only. Public clients only. Redirects are limited to ChatGPT's
  callbacks, loopback, and the cloud front door's one callback
  (`https://cloud.amux.io/oauth/workspace/callback`); `AMUX_MCP_REDIRECT_ALLOW`
  adds HTTPS prefixes.
- Codes last 2 minutes and are single use; a replayed code revokes its grant.
  Access tokens last 1 hour; refresh tokens 30 days, rotated on every use.
  Everything is stored as a sha256 hash.
- Every registration, grant, refusal and tool call is appended to
  `~/.amux/logs/chatgpt-app-audit.jsonl`. Refusals also log a `verdict`
  (`chatgpt_mcp_unauthorized`, `chatgpt_owner_action_refused`,
  `tunnel_mcp_path_refused`, and others) to the server log.

## Listing copy (draft, not published)

**Name:** amux

**Short description:** Run and steer your coding-agent fleet from ChatGPT.

**Long description:** amux runs many coding agents (Claude Code, Codex, Gemini
and others) side by side on your own machine, with a shared task board. This
app connects ChatGPT to your amux. Ask what every worker is doing, read a
worker's recent output, send one an instruction, and add or move cards on the
board. Each connection needs your approval inside amux, and nothing in the app
can delete or stop a worker.

**Company URL:** https://amux.io/

**Privacy policy:** https://amux.io/privacy/

**Terms of service:** none published yet (see SUBMIT.md).

**UI component:** none, so there are no CSP domains to declare.

## Test cases

Positive:

1. "What are my amux workers doing right now?" calls `list_workers` and
   summarizes names and states.
2. "Show me the last 50 lines from the worker called amux." calls
   `read_worker` with `name: amux, lines: 50` and returns the transcript.
3. "Tell the docs worker to fix the broken link on the pricing page." calls
   `message_worker`; the result shows the delivery status amux reported.
4. "Add a card for the docs worker: update the install guide for Linux."
   calls `add_card` with `session: docs`; the result says whether a new card was
   created.
5. "What's open on the board for the amux worker?" calls `list_board` with
   `session: amux` and lists the cards.

Negative:

1. A request with no token, or an expired or revoked one, gets HTTP 401 with a
   `WWW-Authenticate` header pointing at the protected resource metadata.
2. "Mark card AMUX-1 done" without evidence: `update_card_status` returns the
   board's own refusal (evidence required) with `isError: true`, and the card
   does not move.
3. On a connection approved read-only, "Message the amux worker" returns an
   error naming the missing `amux:write` scope and sends nothing.
