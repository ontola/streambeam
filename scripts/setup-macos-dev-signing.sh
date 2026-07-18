#!/usr/bin/env bash
set -euo pipefail

identity="StreamBeam Local Development"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
local_config="$repo_root/app/macos/Runner/Configs/LocalSigning.xcconfig"

if ! security find-identity -v -p codesigning | grep -Fq "\"$identity\""; then
  cert_dir="$(mktemp -d)"
  trap 'rm -rf "$cert_dir"' EXIT
  import_password="streambeam-local-import"

  openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
    -subj "/CN=$identity/O=StreamBeam Development" \
    -addext "basicConstraints=critical,CA:FALSE" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=codeSigning" \
    -keyout "$cert_dir/key.pem" -out "$cert_dir/cert.pem"
  openssl pkcs12 -export -legacy \
    -inkey "$cert_dir/key.pem" -in "$cert_dir/cert.pem" \
    -name "$identity" -passout "pass:$import_password" \
    -out "$cert_dir/identity.p12"
  security import "$cert_dir/identity.p12" \
    -k "$HOME/Library/Keychains/login.keychain-db" \
    -P "$import_password" -T /usr/bin/codesign
  security add-trusted-cert -r trustRoot -p codeSign \
    -k "$HOME/Library/Keychains/login.keychain-db" "$cert_dir/cert.pem"
fi

printf 'STREAMBEAM_CODE_SIGN_IDENTITY = %s\n' "$identity" > "$local_config"
echo "Configured stable macOS development signing: $identity"
