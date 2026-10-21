#!/usr/bin/env python3
"""Sign a COPY of a built macOS app, notarize it, staple and verify delivery.

Never modifies an installed app or the source build's signing identity.
Credentials are passed by environment; secret values are not printed.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--app", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
identity = os.environ["MACOS_SIGN_IDENTITY"]
if not identity.startswith("Developer ID Application: "):
    parser.error("MACOS_SIGN_IDENTITY must be a Developer ID Application identity")
source = args.app.resolve()
output = args.output.resolve()
if not (source / "Contents/Info.plist").is_file():
    parser.error("--app must be a built application bundle")
if output.exists() or output == source or source in output.parents:
    parser.error("--output must be a new directory outside the source bundle")
key = Path(os.environ["APPLE_NOTARY_KEY_PATH"]).resolve()
if not key.is_file():
    parser.error("APPLE_NOTARY_KEY_PATH must be an App Store Connect API private key file")
output.mkdir(parents=True)
app = output / "StreamBeam.app"
subprocess.run(["ditto", str(source), str(app)], check=True)

def run(*command):
    subprocess.run(command, check=True)

def sign(path, entitlements=False):
    command = ["codesign", "--force", "--timestamp", "--options", "runtime", "--sign", identity]
    if entitlements:
        command += ["--entitlements", str(Path(__file__).resolve().parents[1] / "app/macos/Runner/Release.entitlements")]
    run(*command, str(path))

# Sign leaf Mach-O files first, then each enclosing framework/bundle, and the
# outer app last. --deep is for verification, never a shortcut for signing.
magic = {b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca"}
for path in sorted(app.rglob("*"), key=lambda p: len(p.parts), reverse=True):
    if path.is_symlink():
        continue
    if path.is_file():
        with path.open("rb") as handle:
            is_macho = handle.read(4) in magic
        if is_macho:
            sign(path)
    elif path.suffix in {".framework", ".app", ".xpc", ".bundle"}:
        # Resource-only bundles have no executable and need no code signature.
        if (path / "Contents/MacOS").exists() or path.suffix == ".framework":
            sign(path)
sign(app, entitlements=True)
run("codesign", "--verify", "--deep", "--strict", str(app))
archive = output / "StreamBeam.zip"
run("ditto", "-c", "-k", "--keepParent", str(app), str(archive))
command = ["xcrun", "notarytool", "submit", str(archive), "--key", str(key),
           "--key-id", os.environ["APPLE_NOTARY_KEY_ID"], "--issuer", os.environ["APPLE_NOTARY_ISSUER_ID"],
           "--wait", "--timeout", "30m", "--output-format", "json"]
result = subprocess.run(command, capture_output=True, text=True)
if result.returncode:
    raise RuntimeError("Notarization request failed; check API key access and Apple service status")
receipt = json.loads(result.stdout)
(output / "notarization.json").write_text(json.dumps(receipt, indent=2) + "\n")
if receipt.get("status") != "Accepted":
    raise RuntimeError(f"Notarization was not accepted (submission {receipt.get('id', 'unknown')})")
run("xcrun", "stapler", "staple", str(app))
run("xcrun", "stapler", "validate", str(app))
run("codesign", "--verify", "--deep", "--strict", str(app))
run("spctl", "--assess", "--type", "execute", "--verbose=2", str(app))
archive.unlink()
run("ditto", "-c", "-k", "--keepParent", str(app), str(archive))
print(f"Signed, notarized and stapled release: {archive}")
