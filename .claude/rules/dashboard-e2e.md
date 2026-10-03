---
description: E2E testing for dashboard UI changes affecting multiple workers
globs: ["crates/amux-dashboard/static/**"]
---

When editing dashboard UI that touches worker cards, the send composer, peek
overlays, or any control that renders per-session:

- **Test every state combination, not just the happy path.** A control driven by
  two booleans has four states; test all four. The send-split button is
  `{send,queue} x {active,idle}` and the bug was in one quadrant.
- **Test across workers with different statuses.** Active, idle, waiting,
  paused, stopped, unattributed. Clicking one card's control must not bleed
  into another card's visual state.
- **Test after a re-render cycle.** Trigger `_fetchAndRender()` or wait for an
  SSE update, then re-check. A fix that survives the initial render but breaks
  on refresh is worse than no fix.
- **Verify text AND class agree.** A button that says "Send" but carries
  `mode-queue` (or vice versa) is the exact bug class this rule exists to
  prevent. Check both, not just one.
- **Test the peek overlay separately from card-level controls.** They share
  logic but draw from different DOM roots (`#peek-overlay` vs `.card`). A fix
  to one does not prove the other.
- **A global mode must LOOK global.** If a toggle is per-user (not per-session),
  every card must show the same label after the toggle. A helper function that
  conditions on session status (e.g., returning "Send" for idle workers even in
  queue mode) creates visual inconsistency that looks broken. Keep per-session
  delivery semantics in tooltips, not labels.
- **SSE re-renders can overwrite your sync.** `_syncComposerPending` updates
  buttons, but an SSE event can trigger `render()` which rebuilds cards from
  the template. If the template's inline evaluation uses session-specific data
  that changed between the sync and the render, one card can disagree with the
  rest. After any edit to the send-split or composer, verify with a live
  dashboard where sessions are actively changing status.
