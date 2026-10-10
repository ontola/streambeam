#!/usr/bin/env python3
"""Check a minified Android release retained ML Kit's reflective constructors.

Run after `cd app && flutter build apk --release`. Flutter/widget tests cannot
catch R8 deleting manifest-discovered component constructors.
"""
import re
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[1]
folder = Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'app/build/app/outputs/mapping/release'
mapping = (folder / 'mapping.txt').read_text()
usage = (folder / 'usage.txt').read_text()
registrars = [
    'com.google.mlkit.common.internal.CommonComponentRegistrar',
    'com.google.mlkit.vision.common.internal.VisionCommonRegistrar',
    'com.google.mlkit.vision.barcode.internal.BarcodeRegistrar',
]
errors = []
for name in registrars:
    blocks = re.findall(r'^' + re.escape(name) + r':\n((?:[ \t].*\n)*)', usage, re.M)
    if any('void <init>()' in block for block in blocks):
        errors.append(f'{name}: no-argument constructor removed by R8')
    kept = re.search(r'^' + re.escape(name) + ' -> ' + re.escape(name) + r':\n((?:[ \t#].*\n)*)', mapping, re.M)
    if kept is None or 'void <init>()' not in kept[1]:
        errors.append(f'{name}: original class name and constructor not retained')
if errors:
    sys.exit('\n'.join(errors))
print('PASS: all three ML Kit component registrars retain their reflective constructors')
