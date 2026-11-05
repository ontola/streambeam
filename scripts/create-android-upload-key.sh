#!/usr/bin/env bash
# Create the long-term Google Play *upload* key for StreamBeam and print the
# values to store in the password manager and GitHub secrets.
#
# Run on your own machine, never in CI. Nothing here is committed or uploaded.
# Google Play App Signing keeps the real app-signing key; this key only proves
# uploads come from you, and Google can reset it if it is ever lost.
#
#   scripts/create-android-upload-key.sh [output-dir]
set -euo pipefail

out_dir="${1:-$HOME/streambeam-play-upload}"
alias_name="streambeam-upload"
keystore="$out_dir/streambeam-upload.p12"

command -v keytool >/dev/null || { echo "keytool not found (install a JDK, e.g. via Android Studio)" >&2; exit 1; }
[[ -e "$keystore" ]] && { echo "$keystore already exists; refusing to overwrite" >&2; exit 1; }

mkdir -p "$out_dir"
chmod 700 "$out_dir"
password="$(openssl rand -base64 30 | tr -d '/+=' | cut -c1-32)"

# PKCS12 uses one password for store and key.
keytool -genkeypair -v \
  -storetype PKCS12 -keystore "$keystore" \
  -alias "$alias_name" -keyalg RSA -keysize 4096 -validity 10000 \
  -storepass "$password" -keypass "$password" \
  -dname "CN=StreamBeam upload key, O=Argu B.V., C=NL" >&2
chmod 600 "$keystore"

sha256="$(keytool -list -v -keystore "$keystore" -storepass "$password" -alias "$alias_name" \
  | awk -F': ' '/SHA256:/ {print $2; exit}')"
base64 < "$keystore" | tr -d '\n' > "$keystore.base64"
chmod 600 "$keystore.base64"

cat >&2 <<INFO

Created $keystore

1. Save in Proton Pass (one item, "StreamBeam Play upload key"):
   - attach $keystore
   - password: (printed below)
   - alias:    $alias_name
2. Add GitHub Actions secrets on the repository:
   STREAMBEAM_KEYSTORE_BASE64   = contents of $keystore.base64
   STREAMBEAM_KEYSTORE_PASSWORD = the password
   STREAMBEAM_KEY_PASSWORD      = the same password
   STREAMBEAM_KEY_ALIAS         = $alias_name
   and repository variable
   STREAMBEAM_UPLOAD_CERT_SHA256 = $sha256
3. Delete $keystore.base64 afterwards; keep the .p12 until the vault restore is tested.

INFO
printf 'password: %s\n' "$password" >&2
printf 'upload cert SHA-256: %s\n' "$sha256" >&2
