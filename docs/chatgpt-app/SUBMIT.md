# Submitting amux to the ChatGPT app directory

What is built and what only Ethan can do. Tracking card: AMUX-5397.

## Built and working

- The MCP server and OAuth 2.1 server (`api/chatgpt_app.rs`), tested end to end.
- MCP-only tunnel mode, so one amux can be reached by ChatGPT without
  exposing its dashboard.
- Listing copy, tool list and 5 positive and 3 negative test cases in
  `README.md`.

## The blocker for a PUBLIC listing

A directory listing has one fixed MCP URL that every ChatGPT user connects to,
and OpenAI does not allow that origin to change later. Today each amux has its
own URL (`https://<tid>.t.amux.io/mcp`), which is right for developer mode and
wrong for the directory. A public listing needs a shared origin such as
`https://cloud.amux.io/mcp` whose OAuth signs the user in (Clerk) and routes
their requests to their own amux. That is a change to cloud.amux.io
production, so it waits for a decision.

## Ethan-only steps, in order

1. **Decide the public origin.** Approve building `cloud.amux.io/mcp` as the
   multi-tenant front door (gateway work, no new paid infrastructure), or keep
   amux as a developer-mode app only.
2. **Terms of service.** amux.io has a privacy page but no terms page. Decide
   whether to publish one; the portal may ask for it.
3. **Business verification** in the OpenAI Platform organization that will own
   the listing.
4. **The submission click.** In the OpenAI plugin portal choose "With MCP",
   enter the production `/mcp` URL, verify the domain, let it scan the tools,
   paste the listing copy and test cases from `README.md`, supply a reviewer
   login (an amux account whose connection you pre-approve), and submit.

Until step 1 is decided, anyone can use the app in ChatGPT developer mode with
the steps in `README.md`.
