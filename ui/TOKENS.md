# Powder UI design tokens

`ui/src/tokens.css` is the **single source of truth** for Powder's shadcn /
Base UI replatform. Powder owns this restrained operational palette and every
component contract directly.

The prior uniform-style constraints are retired by this layer:

- radius becomes a **scale** (`--radius-0` is one option, not a law); and
- type becomes a **scale** (hierarchy, spacing, and typography are the bar).

Every value lives in `tokens.css` once, as a Tailwind 4 `@theme` token — a CSS
custom property. The token namespaces follow Tailwind 4 so the shadcn
primitives can be themed with generated utilities (`bg-surface`, `text-ink`,
`border-line`, `text-sm`, `rounded-md`, …) **and** every token is also a plain
CSS custom property for direct `var()` use in hand-authored CSS.

## The contract every migrated view must follow

1. **Consume tokens, never literals.** A migrated view references tokens via
   `var(--…)`. It carries **no raw hex or px literals** — the literal values
   live in `tokens.css`, once. `em`/`rem`/`%` may appear where they are
   genuinely relative (line-heights, content measures), but a color, a font
   size, a spacing value, a radius, a shadow, or a touch target is a token
   reference or nothing.
2. **Name roles, not hues.** Use the semantic color roles (`--color-surface`,
   `--color-ink`, `--color-muted`, `--color-line`, `--color-accent`,
   `--color-danger`, …). A migrated view never says which hex a color is; it
   says what the color *means*. The role re-resolves per scheme.
3. **Use the scales.** Spacing from `--pw-space-*`, type from `--text-*`,
   radius from `--radius-*`, weight from `--pw-weight-*`, focus from
   `--pw-focus-*`, motion from `--pw-ease`/`--pw-quick`/`--pw-soft`.
4. **One focus ring.** Interactive primitives draw the focus ring from
   `--pw-focus-*`; no ad-hoc outline colors or widths.
5. **Dark/light is automatic.** Do not branch on mode in view code. The role
   tokens re-resolve under `:root.dark` / `[data-pw-mode='dark']` and the OS
   `prefers-color-scheme: dark` fallback. The Powder-owned `pw-mode`
   localStorage toggle pins an explicit preference.

## Token reference

### Type scale (`--text-*`) — hierarchy is the bar

| token | size | use |
|---|---|---|
| `--text-xs` | 12px | micro chrome: counts, kbd hints, attachment chips |
| `--text-sm` | 13px | chrome: labels, captions, button text, meta |
| `--text-base` | 14px | body: inputs, inline body copy |
| `--text-md` | 15px | subheading / emphasized body |
| `--text-lg` | 16px | panel + section heading |
| `--text-xl` | 18px | page heading *(board-view migration card)* |
| `--text-2xl` | 22px | hero / board title *(board-view migration card)* |

`--font-sans` (Geist) and `--font-mono` (Geist Mono) name the families
`index.html` loads via Google Fonts.

### Radii scale (`--radius-*`) — radius is a value, not a law

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
The comic-ops 800 black is retired; 600 carries headings.

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
`powder-board.css`) is the reference implementation: every value in its rules is
a token reference, it exercises the type scale (a `--text-lg` panel title over
`--text-sm` labels and `--text-base` inputs), the spacing scale, the role
colors, the focus ring, and dark/light parity — all while staying within the
still-enforced `radius-0` / `<=16px` invariants so the law gate holds while the
rest of the board is unmigrated. The board-view migration card retires those
invariants board-wide and draws `--text-xl`/`--text-2xl` and `--radius-md`/`lg`
from this same layer.
