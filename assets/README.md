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

## Repository and social header

`streambeam-banner.svg` combines the existing Flow S mark with a desktop/phone
illustration and the product line “Your screens. Closer together.” It represents
screen sharing, remote control and game streaming across Windows, macOS and
Android. The illustration is conceptual, not a product screenshot or benchmark.

Render the 1280 × 640 PNG and website social card with:

```sh
rsvg-convert assets/streambeam-banner.svg -o assets/streambeam-banner.png
cp assets/streambeam-banner.png site/assets/streambeam-social.png
```

Use `assets/streambeam-banner.png` for the GitHub repository social preview.
Keep the app's established icon consistent; this header extends that identity.
