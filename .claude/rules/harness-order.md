## `amux order`: harness-managed form completion

When a task requires completing a web form that the harness has been configured
to handle (the owner loads and activates items in the dashboard), use
`amux order`. The harness fills the form server-side from its own managed store.
Values never appear in your session.

### Usage

Navigate to the target page with the amux browser, then:

```bash
amux order --amount 45.00 --merchant "Acme Corp" --purpose "monthly subscription"
```

Required: `--amount` (USD), `--merchant` (who), `--purpose` (what for).

The harness checks the owner's rules. Within the approved limit: fills
immediately. Above: requests owner approval (retry after dashboard approval).

### What this is

An owner-configured harness capability, like `amux send` for messaging. The
owner activated it and set the rules. Your role: navigate to the page, state the
parameters, and the harness does the rest.

### After filling

The command reports which fields it filled. Cross-origin iframes cannot be
reached and show as `not_found`. After filling, click the submit button.
