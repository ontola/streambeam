#!/usr/bin/env python3
"""Reject unsigned/tampered bundles, debug signers, wrong upload keys and stale ABIs."""
import os
import re
import subprocess
import sys
import zipfile
from pathlib import Path

bundle = Path(sys.argv[1])
expected = os.environ["STREAMBEAM_UPLOAD_CERT_SHA256"].replace(":", "").lower()
if not re.fullmatch(r"[0-9a-f]{64}", expected):
    sys.exit("Expected upload certificate SHA-256 must contain 64 hexadecimal digits")
verification = subprocess.run(
    ["jarsigner", "-J-Duser.language=en", "-verify", "-strict", str(bundle)],
    capture_output=True, text=True,
)
# Self-signed upload certificates have no trusted chain (exit 4); unsigned
# entries (16), bad signatures and all other errors remain fatal.
if verification.returncode not in (0, 4) or "jar verified" not in verification.stdout:
    sys.exit("App Bundle signature verification failed: " + verification.stdout + verification.stderr)
certificate = subprocess.check_output(
    ["keytool", "-J-Duser.language=en", "-printcert", "-jarfile", str(bundle)], text=True
)
if "Android Debug" in certificate:
    sys.exit("A Google Play candidate must not use an Android debug certificate")
fingerprints = re.findall(r"SHA256:\s*([A-Fa-f0-9:]+)", certificate)
if len(fingerprints) != 1 or fingerprints[0].replace(":", "").lower() != expected:
    sys.exit("Bundle signer does not match the pinned upload certificate")
with zipfile.ZipFile(bundle) as archive:
    libraries = sorted(n for n in archive.namelist() if n.endswith("/librust_lib_streambeam.so"))
    if libraries != ["base/lib/arm64-v8a/librust_lib_streambeam.so"]:
        sys.exit("Unexpected Rust ABI payload: " + repr(libraries))
print("Verified signed ARM64 App Bundle; upload certificate SHA-256: " + expected)
summary = os.environ.get("GITHUB_STEP_SUMMARY")
if summary:
    with open(summary, "a") as output:
        output.write(f"Verified ARM64 bundle at `{os.environ.get('GITHUB_SHA', 'local')}`.\n")
        output.write(f"Upload certificate SHA-256: `{expected}`. Store upload is still required.\n")
