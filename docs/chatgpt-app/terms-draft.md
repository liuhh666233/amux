# DRAFT: amux Terms of Service

Status: draft for Ethan's review (AMUX-5397). Not published. When approved,
it goes to `site/terms/index.html` in the same layout as `site/privacy/`, and
the footer links to it. Written to match the privacy page's plain style; it is
not legal advice and should be read by whoever reviews our contracts.

Last updated: [date of publication]

## Overview

These terms cover the amux open-source software, the amux.io website, the
managed service at cloud.amux.io, the amux iOS app, and the amux app for
ChatGPT. amux is built by Mixpeek, Inc. ("we"). By using any of them you agree
to these terms.

## The open-source software

amux is released under the MIT License. The license, not these terms, governs
your use, copying, modification and distribution of the source code. When you
run amux yourself, you run it on your own machine and under your own control.

## cloud.amux.io

- **Accounts.** You sign in through our identity provider. You are responsible
  for what happens under your account and for keeping your sign-in secure.
- **Plans and billing.** Paid plans are billed through Stripe at the price
  shown when you subscribe. You can cancel at any time; access continues to
  the end of the paid period. Trial limits (days and spend) are shown when the
  trial starts.
- **Your content.** You keep all rights to the code, prompts, messages and
  other content you put into amux. We process it only to run the service for
  you.
- **Acceptable use.** Do not use amux to break the law, to attack or probe
  systems you are not authorized to test, to send spam, or to get around
  another service's usage limits or terms. We may suspend an account that
  does, and we will tell you why.
- **Third-party AI providers.** Workers run on AI models from providers such
  as Anthropic and OpenAI. Their terms apply to your use of their models.

## The amux app for ChatGPT

- The app connects ChatGPT to an amux you choose: your own machine, through
  its tunnel, or your cloud workspace. You approve each connection, and you
  can disconnect it at any time from amux or from cloud.amux.io.
- While connected, ChatGPT can read your workers and board cards, message
  workers and update cards, within the permissions you approved. What the
  tools return is sent to OpenAI as part of your ChatGPT conversation and is
  handled under OpenAI's terms and privacy policy.
- Requests pass through cloud.amux.io to reach your amux. We keep a log of
  connections and tool calls for security and debugging. We do not keep the
  content of tool results.

## Availability and changes

We work to keep cloud.amux.io available but do not promise it will be
uninterrupted. We may change or discontinue features. For a change that
materially affects paid users, we will give notice by email before it takes
effect.

## Disclaimer and liability

The software and services are provided "as is", without warranties of any
kind. AI workers can make mistakes, including in code they write or commands
they run; review their work before you rely on it. To the extent the law
allows, our total liability for any claim relating to the services is limited
to the amount you paid us in the 12 months before the claim.

## Ending your use

You can stop using amux and delete your cloud account at any time by emailing
support@amux.io. We may end access for a breach of these terms. The privacy
policy describes what happens to your data.

## Changes to these terms

We may update these terms. Changes are posted on this page with a new date,
and material changes are emailed to cloud.amux.io users.

## Contact

Questions about these terms? Email support@amux.io.

---

## Suggested addition to the privacy page (also a draft)

Add a section after "cloud.amux.io (managed cloud)":

> **amux app for ChatGPT.** If you connect amux to ChatGPT, the results of the
> tools ChatGPT calls (worker lists, worker output, board cards) are sent to
> OpenAI as part of your conversation and are handled under OpenAI's privacy
> policy. The connection passes through cloud.amux.io, which records when you
> connected, disconnected and called a tool, but not what the tools returned.
> You can disconnect at any time, and your amux stops accepting the
> connection at once.
