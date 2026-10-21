#!/usr/bin/env python3
"""Import Developer ID credentials only in an ephemeral Actions keychain."""
import base64
import os
from pathlib import Path
import secrets
import subprocess

assert os.environ.get("GITHUB_ACTIONS") == "true", "Use only on an ephemeral GitHub Actions runner"
os.umask(0o077)
out = Path(os.environ["RUNNER_TEMP"]) / "streambeam-macos-signing"
out.mkdir(exist_ok=False)
identity = os.environ["MACOS_SIGN_IDENTITY"]
assert identity.startswith("Developer ID Application: "), "Developer ID Application identity required"
assert not any(c in identity for c in "\r\n"), "Invalid signing identity"
p12 = out / "distribution.p12"
p12.write_bytes(base64.b64decode(os.environ["MACOS_DEVELOPER_ID_P12_BASE64"], validate=True))
key = out / "AuthKey.p8"
key.write_bytes(base64.b64decode(os.environ["APPLE_NOTARY_KEY_BASE64"], validate=True))
keychain = out / "signing.keychain-db"
password = secrets.token_urlsafe(32)

def run(*args):
    result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if result.returncode:
        raise RuntimeError(f"{args[0]} {args[1]} failed (signing details withheld)")

run("security", "create-keychain", "-p", password, str(keychain))
run("security", "set-keychain-settings", "-lut", "7200", str(keychain))
run("security", "unlock-keychain", "-p", password, str(keychain))
run("security", "import", str(p12), "-k", str(keychain), "-P",
    os.environ["MACOS_DEVELOPER_ID_P12_PASSWORD"], "-T", "/usr/bin/codesign")
run("security", "set-key-partition-list", "-S", "apple-tool:,apple:", "-s", "-k", password, str(keychain))
keychains = subprocess.check_output(["security", "list-keychains", "-d", "user"], text=True)
existing = [line.strip().strip('"') for line in keychains.splitlines()]
run("security", "list-keychains", "-d", "user", "-s", str(keychain), *existing)
print("Imported Developer ID signing credentials into the temporary CI keychain")
