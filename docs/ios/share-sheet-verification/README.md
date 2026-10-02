# iOS share sheet: owner feedback verified on the simulator

Run 2026-10-02 07:52 local, iPhone 17 Pro Max simulator, build from origin/main
0efc2fcf plus commit "fix(ios share): a send to a stopped worker reports Sent…",
against the live local amux server. Test:
`ShareSheetUITests.testOwnerFeedbackChecklist` (passed in 121.9 s). Sends went
only to two throwaway workers (empty send allow-list, standing orders off,
deleted afterwards); no message from them reached another worker.

| # | Feedback | Result | Screenshot |
|---|---|---|---|
| A1 | Choose which worker to send to | pass | a1-a2-a9-layout.png, a1-a3-two-selected.png |
| A2 | Active workers only by default | pass: "26 active, 24 running · 20 paused hidden" | a1-a2-a9-layout.png |
| A3 | Select more than one worker | pass: "To zz-share-sim-a, zz-share-sim-b" | a1-a3-two-selected.png |
| A4 | Send and Cancel never go away | pass: Cancel present while sending | a6-sending.png |
| A5 | Keyboard covers nothing | pass with the note focused (search focused: testTheExtensionLoadsWorkersFromTheAppGroup) | a5-note-keyboard-up.png |
| A6 | Send shows an indicator | pass: "Sending to 2 workers…" then "Sent" | a6-sending.png, a6-sent.png |
| A7 | Workers load fast | pass: first worker row 1.88 s after tapping amux | a7-first-paint.png |
| A8 | Sort by last shared (default), most shared, activity, name; filter by group | pass: all four sorts; group amux -> 2 | a8-*.png |
| A9 | Search on top, Filter and Sort under it, note at the bottom | pass | a1-a2-a9-layout.png |
| A10 | Workers with Chat on are listed | pass: amux and mixpeek-override listed | a10-chat-enabled-listed.png |

Delivery was read back from the server: both throwaways received the message.
Raw notes from the run: checklist.txt.
