#!/usr/bin/env python3
"""Export every StreamBeam icon from assets/streambeam-mark.svg.

Requires rsvg-convert (librsvg) and magick (ImageMagick). The mark is the editable
vector master; platform PNG/ICO files and the framed SVG are generated outputs.
"""
import json
import re
import shutil
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
mark = (ROOT / 'assets/streambeam-mark.svg').read_text()
body = re.search(r'<svg[^>]*>(.*)</svg>', mark, re.S)[1]


def svg(background=False, rounded=True, scale=1):
    bg = f'<rect width="512" height="512" rx="{112 if rounded else 0}" fill="#0F172A"/>' if background else ''
    shape = body if scale == 1 else f'<g transform="translate(256 256) scale({scale}) translate(-256 -256)">{body}</g>'
    return '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" fill="none" role="img" aria-label="StreamBeam logo">' + bg + shape + '</svg>\n'


framed = svg(background=True)
for file in ['assets/streambeam-logo.svg', 'app/assets/streambeam-logo.svg', 'site/assets/streambeam-logo.svg']:
    dest = ROOT / file
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(framed)
(ROOT / 'assets/streambeam-mark-mono.svg').write_text(
    mark.replace('#6366F1', 'currentColor').replace('#22D3EE', 'currentColor'))

jobs = []
def export(file, size, source=framed):
    jobs.append((ROOT / file, size, source))

for density, base in [('mdpi', 48), ('hdpi', 72), ('xhdpi', 96), ('xxhdpi', 144), ('xxxhdpi', 192)]:
    folder = f'app/android/app/src/main/res/mipmap-{density}'
    export(f'{folder}/ic_launcher.png', base)
    export(f'{folder}/ic_launcher_foreground.png', round(base * 108 / 48), svg(scale=.70))

for platform in ['macos', 'ios']:
    folder = f'app/{platform}/Runner/Assets.xcassets/AppIcon.appiconset'
    manifest = json.loads((ROOT / folder / 'Contents.json').read_text())
    for image in {image['filename']: image for image in manifest['images']}.values():
        size = round(float(image['size'].split('x')[0]) * float(image['scale'].rstrip('x')))
        export(f"{folder}/{image['filename']}", size, framed if platform == 'macos' else svg(background=True, rounded=False))

for size in [192, 512]:
    export(f'app/web/icons/Icon-{size}.png', size)
    export(f'app/web/icons/Icon-maskable-{size}.png', size, svg(background=True, rounded=False, scale=.85))
export('app/web/favicon.png', 32)
export('assets/streambeam-logo.png', 512)
for name, size in [('Square44x44Logo', 44), ('Square150x150Logo', 150), ('StoreLogo', 50)]:
    export(f'packaging/windows/Assets/{name}.png', size)

with tempfile.TemporaryDirectory(prefix='streambeam-brand-') as temporary:
    tmp = Path(temporary)
    def render(job):
        dest, size, source = job
        dest.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(['rsvg-convert', '-w', str(size), '-h', str(size), '-o', str(dest)], input=source.encode(), check=True)
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(render, jobs))
    icon_pngs = []
    for size in [16, 24, 32, 48, 64, 128, 256]:
        file = tmp / f'icon-{size}.png'
        render((file, size, framed))
        icon_pngs.append(str(file))
    ico = ROOT / 'app/windows/runner/resources/app_icon.ico'
    subprocess.run(['magick', *icon_pngs, str(ico)], check=True)
    shutil.copyfile(ico, ROOT / 'app/assets/tray_icon.ico')
print(f'Generated {len(jobs)} PNGs, platform ICOs and shared SVGs from the S mark.')
