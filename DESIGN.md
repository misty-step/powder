# Powder DESIGN.md

This file is the product's public-site brand contract. Keep it short and exact:
agents and humans should be able to update `site/` from this file without
inventing a second design system.

## Brand Voice

- Plain-spoken, concrete, and operator-facing.
- Lead with the user outcome, then the proof.
- Avoid marketing fog, mascot language, and decorative claims.
- Treat Powder as infrastructure people can run, not a hosted task-tool brand.

## Pitch One-Liner

`Powder helps operators and agent teams share a self-hosted work ledger without losing claims, context, or proof in chat.`

## Lucide Mark

- Icon: `snowflake`
- Reason: reused from the Powder board because it is already the product mark
  operators see in the live Kanban face.
- Rule: the mark is an inline Lucide SVG inside `.ae-app-mark`. No bespoke
  marks, logo images, emoji marks, or colored wordmarks.

## Palette Hooks

Only steer brand tokens here. Do not add a second palette.

```css
:root {
  --ae-accent: #2643d0;
  --ae-accent-dark: #8c9eff;
  --product-signal: #2c6e62;
  --product-signal-dark: #54bba4;
}
```

If the product needs extra categorical hues, name them as project tokens and
spend them on content, never filled pills:

```css
:root {
  --product-signal: #2c6e62;
  --product-signal-dark: #54bba4;
}
```

## Screenshot Inventory

| File                                      | Surface       | State                                  | Caption                                      |
| ----------------------------------------- | ------------- | -------------------------------------- | -------------------------------------------- |
| `site/assets/screenshots/01-overview.png` | Board desktop | Demo instance with representative data | Live board showing backlog, ready, and done. |
| `site/assets/screenshots/02-narrow.png`   | Board narrow  | Same demo instance at mobile width     | Narrow board proving the real responsive UI. |
| `site/assets/screenshots/03-edge.png`     | Board desktop | Blocked/non-happy card visible         | Edge state with blocked work and blockers.   |

## Footer Links

- Misty Step: `https://mistystep.io`
- GitHub: `https://github.com/misty-step/powder`
- Weave: omitted; Powder is a Misty Step fleet product, not a Weave-family
  product surface.

## Release Notes Rule

`site/changelog.html` is user-facing. Write entries as product outcomes, not
commit logs. Each entry needs a date, a version or release label, and one or two
plain-language bullets.
