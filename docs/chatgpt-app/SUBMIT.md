# Submitting amux to the ChatGPT app directory

What is built and what only Ethan can do. Tracking card: AMUX-5397.

## Built and working

- The MCP server and OAuth 2.1 server on every amux (`api/chatgpt_app.rs`),
  tested end to end.
- MCP-only tunnel mode, so ChatGPT can reach one amux without exposing its
  dashboard.
- The public front door at `https://cloud.amux.io/mcp`
  (`cloud/gateway/chatgpt_front.py`): one fixed origin for the listing that
  routes each user to their own amux. Design and tests are in `README.md`.
  Tested locally with `python3 cloud/tests/test_chatgpt_front.py`; not
  deployed.
- The deploy workflow now ships `chatgpt_front.py` with `gateway.py`.
- Listing copy, tool list and 5 positive and 3 negative test cases in
  `README.md`.
- A terms of service draft in `terms-draft.md`, not published.

## What is blocking the listing

cloud.amux.io has no host. Its VM (`amux-dev`, GCP mixpeek-inference-463103,
us-central1-a) was deleted on 2026-09-21, after a final snapshot
(`amux-dev-final-2026-09-22`). The front door runs inside the cloud gateway, so
the listing cannot go live until the gateway runs somewhere again. Nothing was
restored, because that spends money.

## Ethan-only steps, in order

1. **Host for cloud.amux.io (money).** Restore the VM from
   `amux-dev-final-2026-09-22`, or pick another host for the gateway. Once it
   runs, a normal push deploys the front door (`deploy-cloud.yml`). Then check
   `https://cloud.amux.io/.well-known/oauth-authorization-server` names
   `https://cloud.amux.io` as its issuer.
2. **Terms of service.** Review `terms-draft.md` and publish it at
   `amux.io/terms/`. The privacy page says "no data leaves your machine" for
   self-hosted amux. With the ChatGPT app connected, tool results go to OpenAI
   and pass through cloud.amux.io, so the privacy page needs a ChatGPT section
   too. The suggested text is at the end of `terms-draft.md`.
3. **Business verification** in the OpenAI Platform organization that will own
   the listing.
4. **The submission.**
   - In the OpenAI plugin portal choose "With MCP" and enter
     `https://cloud.amux.io/mcp`.
   - Verify the domain and let the portal scan the tools.
   - Paste the listing copy and test cases from `README.md`.
   - Supply a reviewer login: a cloud.amux.io account that owns a workspace,
     so it can approve its own connection on the consent page.
   - Submit. The origin cannot change after this.

Until the listing is live, anyone can use the app in ChatGPT developer mode
with their own tunnel URL (the steps are in `README.md`).
