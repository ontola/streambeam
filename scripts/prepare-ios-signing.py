#!/usr/bin/env python3
"""Install an App Store profile and certificate in an ephemeral CI keychain."""
import base64
import datetime
import os
import plistlib
import secrets
import subprocess
from pathlib import Path

os.umask(0o077)
out = Path(os.environ["RUNNER_TEMP"]) / "streambeam-signing"
out.mkdir(exist_ok=True)
assert os.environ.get("GITHUB_ACTIONS") == "true", "Use only on an ephemeral GitHub Actions runner"
p12 = out / "distribution.p12"
profile = out / "distribution.mobileprovision"
p12.write_bytes(base64.b64decode(os.environ["IOS_DISTRIBUTION_P12_BASE64"], validate=True))
profile.write_bytes(base64.b64decode(os.environ["IOS_APP_STORE_PROFILE_BASE64"], validate=True))
data = plistlib.loads(subprocess.check_output(["security", "cms", "-D", "-i", str(profile)]))
bundle = "io.streambeam.streambeam"
team = data["TeamIdentifier"][0]
assert data["Entitlements"]["application-identifier"] == f"{team}.{bundle}", "Wrong bundle profile"
assert not data["Entitlements"].get("get-task-allow"), "Development profile is not permitted"
assert not data.get("ProvisionedDevices") and not data.get("ProvisionsAllDevices"), "App Store profile required"
assert data["ExpirationDate"] > datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None), "Expired profile"
assert all(c not in data["Name"] for c in "\r\n"), "Invalid profile name"
keychain = out / "signing.keychain-db"
password = secrets.token_urlsafe(32)
def run(*args):
    result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if result.returncode:
        raise RuntimeError(f"{args[0]} {args[1]} failed (details withheld to protect signing material)")
run("security", "create-keychain", "-p", password, str(keychain))
run("security", "set-keychain-settings", "-lut", "3600", str(keychain))
run("security", "unlock-keychain", "-p", password, str(keychain))
run("security", "import", str(p12), "-k", str(keychain), "-P",
    os.environ["IOS_DISTRIBUTION_P12_PASSWORD"], "-T", "/usr/bin/codesign")
run("security", "set-key-partition-list", "-S", "apple-tool:,apple:", "-s", "-k", password, str(keychain))
keychains = subprocess.check_output(["security", "list-keychains", "-d", "user"], text=True)
existing = [line.strip().strip('"') for line in keychains.splitlines()]
run("security", "list-keychains", "-d", "user", "-s", str(keychain), *existing)
profiles = Path.home() / "Library/MobileDevice/Provisioning Profiles"
profiles.mkdir(parents=True, exist_ok=True)
destination = profiles / (data["UUID"] + ".mobileprovision")
destination.write_bytes(profile.read_bytes())
(out / "profile-path").write_text(str(destination))
Path("app/ios/Flutter/Signing.xcconfig").write_text(
    f"CODE_SIGN_STYLE = Manual\nDEVELOPMENT_TEAM = {team}\n"
    f"CODE_SIGN_IDENTITY = Apple Distribution\nPROVISIONING_PROFILE_SPECIFIER = {data['Name']}\n")
(out / "ExportOptions.plist").write_bytes(plistlib.dumps({
    "method": "app-store-connect", "teamID": team, "signingStyle": "manual",
    "signingCertificate": "Apple Distribution", "provisioningProfiles": {bundle: data["Name"]},
    "uploadSymbols": True,
}))
print("Installed App Store signing profile for " + bundle)
