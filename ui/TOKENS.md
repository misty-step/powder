# Powder UI design tokens

`ui/src/tokens.css` is the **single source of truth** for Powder's shadcn /
Base UI replatform. Powder owns this restrained operational palette and every
component contract directly.

The design system uses independent radius and type scales:

- radius is a **scale** (`--radius-0` is one option); and
- type is a **scale** (hierarchy, spacing, and typography are the bar).

Every value lives in `tokens.css` once, as a Tailwind 4 `@theme` token — a CSS
custom property. The token namespaces follow Tailwind 4 so the shadcn
primitives can be themed with generated utilities (`bg-surface`, `text-ink`,
`border-line`, `text-sm`, `rounded-md`, …) **and** every token is also a plain
CSS custom property for direct `var()` use in hand-authored CSS.

## Current contract

Tokenized board surfaces use semantic roles, named component values, touch-target
minimums, focus, motion, and dark/light re-resolution from this file. New rules
should adopt a named token whenever one exists. Existing board rules still contain
literal type, spacing, and border values; that is current tokenization debt governed
by later tone-token work.

1. **Consume tokens for adopted roles.** A tokenized surface references values via
   `var(--…)`; raw colors, touch targets, focus rings, and named component values
   belong in `tokens.css` rather than view rules. Relative `em`/`rem`/`%` values
   remain valid where they express content measures or line-height.
2. **Name roles, not hues.** Use semantic color roles (`--color-surface`,
   `--color-ink`, `--color-muted`, `--color-line`, `--color-accent`,
   `--color-danger`, …). A component says what a color means; the role re-resolves
   per scheme.
3. **Use the scales where adopted.** Tokenized rules draw spacing from
   `--pw-space-*`, type from `--text-*`, radius from `--radius-*`, weight from
   `--pw-weight-*`, focus from `--pw-focus-*`, and motion from
   `--pw-ease`/`--pw-quick`/`--pw-soft`.
4. **One focus ring.** Tokenized interactive primitives draw the focus ring from
   `--pw-focus-*`; they do not add ad-hoc outline colors or widths.
5. **Dark/light is automatic.** Do not branch on mode in view code. Role tokens
   re-resolve under `:root.dark` / `[data-pw-mode='dark']` and the OS
   `prefers-color-scheme: dark` fallback; the `pw-mode` localStorage toggle
   pins an explicit preference.

## Token reference

### Type scale (`--text-*`) — hierarchy is the bar

| token | size | use |
|---|---|---|
| `--text-xs` | 12px | micro chrome: counts, kbd hints, attachment chips |
| `--text-sm` | 13px | chrome: labels, captions, button text, meta |
| `--text-base` | 14px | body: inputs, inline body copy |
| `--text-md` | 15px | subheading / emphasized body |
| `--text-lg` | 16px | panel + section heading |
| `--text-xl` | 18px | page heading |
| `--text-2xl` | 22px | hero / board title |

`--font-sans` (Geist) and `--font-mono` (Geist Mono) name the families
`index.html` loads via Google Fonts.

### Radii scale (`--radius-*`) — semantic shape options

`--radius-0` (0), `--radius-sm` (4px), `--radius-md` (6px), `--radius-lg` (8px).
The reference surface uses `--radius-0` (the calm choice); `sm`/`md`/`lg`
complete the scale for denser or more tactile Powder surfaces.

### Color roles (`--color-*`) — semantic, single source

Light defaults (in `@theme`); dark re-resolves under `:root.dark` /
`[data-pw-mode='dark']` and the OS fallback.

| token | role |
|---|---|
| `--color-surface` | page ground |
| `--color-surface-raised` | panels, quick-add, popovers |
| `--color-surface-sunken` | wells, inputs at rest |
| `--color-ink` | primary text + hairline ink |
| `--color-muted` | secondary text, labels |
| `--color-faint` | tertiary text, placeholders |
| `--color-line` | hairlines, dividers |
| `--color-accent` | the single accent (calmer indigo) |
| `--color-accent-ink` | text on an accent ground |
| `--color-danger` | destructive / error |
| `--color-ok` | success |
| `--color-warn` | warning |

### Spacing scale (`--pw-space-*`) — 4px base

`--pw-space-0` … `--pw-space-8` = `0, 2px, 4px, 8px, 12px, 16px, 24px, 32px, 48px`.

### Weights (`--pw-weight-*`)

`--pw-weight-regular` (400), `--pw-weight-medium` (500), `--pw-weight-semibold` (600).
The restrained weight scale uses 600 for headings.

### Focus (`--pw-focus-*`)

`--pw-focus-width` (2px), `--pw-focus-offset` (2px), `--pw-focus-color`
(`var(--color-ink)`). One ring contract for every interactive primitive.

### Motion (`--pw-ease`, `--pw-quick`, `--pw-soft`)

Feedback, never decoration. `--pw-quick` 160ms, `--pw-soft` 240ms,
`--pw-ease` `cubic-bezier(0.23, 1, 0.32, 1)`.

### Measure + elevation

`--pw-measure-form` (32rem) for form widths; `--pw-shadow-1` (subtle) and
`--pw-shadow-pop` (popovers); `--pw-touch-target` (44px, the WCAG 2.5.5 floor).

## Reference implementation

The **quick-add panel** (`#quick-add-panel`, `.pw-quick-add*` in
`powder-board.css`) demonstrates the current contract: semantic role colors,
tokenized touch targets, the shared focus ring, dark/light parity, and named
component values. Its typography and spacing use the defined scales where those
tokens are adopted. Existing board rules still contain literal type, spacing, and
border values; that current debt is governed by later tone-token work.
