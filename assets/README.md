# StreamBeam identity

The selected **Flow S** uses two flat bands: indigo `#6366F1` and cyan `#22D3EE`,
on navy `#0F172A`. The shape is rebuilt as a vector from the selected concept B.

- `streambeam-mark.svg`: editable transparent vector master.
- `streambeam-mark-mono.svg`: generated single-color version using `currentColor`.
- `streambeam-logo.svg` / `.png`: generated rounded app icon.

Run `python3 scripts/generate-brand-icons.py` from the repository root after
editing the master (requires `rsvg-convert` and ImageMagick `magick`). It exports
the Flutter asset, website logo, Android legacy/adaptive icons, Apple asset
catalogs, web icons, Windows application icon and tray icon. Android adaptive
artwork is inset for launcher masks; iOS icons have an opaque square background.

Exploration boards and the original generation prompts are in
`design/logo-variants-2026-10-11/`.
