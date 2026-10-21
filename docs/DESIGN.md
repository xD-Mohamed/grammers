# Documentation design

Use a neutral black canvas with white text and soft white glass edges on
navigation buttons. The glass treatment is explicitly requested by the
maintainer; body content stays flat and readable.

- Background: `oklch(0.08 0 0)`.
- Surfaces: `oklch(0.13 0 0)` and translucent white at 3-6%.
- Primary text: `oklch(0.96 0 0)`; secondary text: `oklch(0.76 0 0)`.
- No chromatic accent, animated backgrounds, external fonts, or heavy framework.
- System sans for prose, system monospace for code.
- Desktop: persistent left navigation, readable article width, compact header.
- Mobile: collapsible navigation, single-column content, horizontally scrollable code.
- Links and copy buttons have clear hover/focus states; motion is limited to
  short interaction feedback and disabled under reduced-motion preferences.
- README uses native GitHub Markdown/HTML and local SVG link buttons; GitHub
  Pages provides the richer layout. No unsupported CSS is promised in README.
