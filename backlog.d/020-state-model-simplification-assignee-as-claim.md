# State-model simplification: assignee-as-claim, three board states

Priority: P1 | Status: backlog | Type: Epic

## Goal
Collapse Powder's board state model to the minimum the product needs. The
board has exactly three states — Ready, In Progress, Done — full stop.
Assignee presence on a card IS the claim; there is no separate `claimed`
status. Needing input is still "in progress" — there is no separate
`awaiting-input` status; the input request arrives via the Bridge or
elsewhere, not through a distinct board column. Review is not a board state
either: the implementer-to-reviewer handoff is a reassignment (the ticket's
assignee changes to a reviewer agent), the reviewer posts findings as
comments and reassigns back to the implementer, that loop repeats until the
work is solid, and the reviewer merges when done. Tickets themselves are
agent scratchpads — work notes, hypotheses, and context that doesn't belong
in code or docs — so a successor agent can pick up without repeating
mistakes. Bare minimum necessary, maximize simplicity: slash and burn while
keeping essential capabilities.

## Oracle
- [ ] The board exposes exactly three states — Ready, In Progress, Done —
      with no `claimed`, `awaiting-input`, or `review` status anywhere in the
      schema, API, CLI, or MCP surface.
- [ ] A card's claim is derived from assignee presence (assignee set =
      claimed/in-progress; assignee absent = ready), not from a separate
      status field.
- [ ] A card needing input renders as In Progress on the board; the input
      request is visible through the Bridge (or another answer-loop surface)
      without introducing a fourth board state.
- [ ] Reviewer handoff is implemented as a reassignment (ticket assignee
      changes to a reviewer agent), with reviewer findings recorded as
      comments and reassignment back to the implementer; no `review` status
      exists anywhere.
- [ ] The 150 live cards on the current deployed instance migrate losslessly
      — every claimed/awaiting-input/review card maps to an equivalent
      in-progress+assignee(+comment) representation with no data loss.
- [ ] The UI exposes exactly two views: Board (3 columns) and Backlog
      (single-column list); clicking a card opens a side sheet/panel — not a
      modal, not a tab.

## Children
- Schema/state migration — map `claimed` and `awaiting-input` into
  `in-progress`+assignee; map `review` into `in-progress`+assignee(reviewer);
  write and run the lossless migration against the live 150-card instance.
- API surface updates — collapse the status enum fleet-wide (powder-core,
  powder-store, powder-api, powder-cli, powder-mcp) to Ready/In
  Progress/Done; assignee becomes the claim signal everywhere claim was
  previously checked.
- Answer-loop rework — spec the seam by which an in-progress card's "needs
  input" state becomes Bridge-visible without a dedicated board state (event,
  flag, or comment convention the Bridge reads).
- Board UI rebuild — three-column board + single-column backlog list +
  side-sheet card detail, per the winning r2 aesthetic-kit design.
- Docs — update VISION.md, README.md, and any state-model references to the
  three-state model; remove `claimed`/`awaiting-input`/`review` from all
  documentation.

## Notes
Operator directive, verbal, 2026-07-02: "Board = Ready / In Progress / Done
ONLY." Kill `claimed` as a status — assignee presence IS the claim. Kill
`awaiting-input` as a status — needing input = still in progress (input
arrives via the Bridge or elsewhere). Review is NOT a state: implementer to
reviewer handoff = reassigning the ticket to a reviewer agent; reviewer posts
findings as comments and reassigns back; loop until solid; reviewer merges;
done. Tickets are agent scratchpads (work notes, hypotheses, context that
doesn't belong in code/docs — so a successor agent picks up without
repeating mistakes). UI = exactly two views: Board (3 cols) + Backlog
(single-column list, click into a side sheet/panel — not modal, not tab).
"Bare minimum necessary, maximize simplicity, slash and burn while
maintaining essential capabilities." The current live instance has 150 cards
using the old states — migration must be lossless.
