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

## Fleet Lock

- Lock: operator lock-in 2026-07-07, `misty-step-936`.
- Homepage h1: `A work board built for agents.`
- Layout: Split. The copy block is left-aligned, vertically centered, and sits
  directly on the page with no panel.
- Hero image: `site/assets/hero.jpg`, copied from the staged
  `powder-hero.jpg` production asset generated with `gpt-image-1` in the Misty
  Step fresco language.
- Image treatment: whole-viewport background image, `background-size: cover`,
  `background-position: center`, opacity `0.35`.
- Homepage content: hero only, no scroll. The CTA text is `Get started` and it
  links to `get-started.html`.
- Header nav: `features`, `get started`, `changelog`, `github`.
- Footer: mode toggle on the left; on the right, `a Misty Step project` with
  `Misty Step` linked to `https://mistystep.io` and an inline GitHub glyph
  linked to `https://github.com/misty-step/powder`.

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

## Footer Contract

- Left: the shared `mode.js` dark/light toggle button.
- Right: `a Misty Step project`, where `Misty Step` links to
  `https://mistystep.io`, followed by the inline GitHub glyph linking to
  `https://github.com/misty-step/powder`.
- No bare URLs, email line, copyright line, or Weave-family footer links.

## Release Notes Rule

`site/changelog.html` is user-facing. Write entries as product outcomes, not
commit logs. Each entry needs a date, a version or release label, and one or two
plain-language bullets.
