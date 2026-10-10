# Android preview releases

The first downloadable release is `v0.1.0` (Android version code 2). These are
LAN-streaming previews, not the completed M8/v1 milestone. Android ARM64 is the
packaged target; Windows and macOS host/app-bridge compilation are checked separately. CI cannot
validate GPU capture, physical controllers, phone cameras or live latency.

## Before publishing

1. Update `app/pubspec.yaml`: match the tag's version and increase the build
   number, even when a preview's semantic version is lower than an old local
   development build's `1.0.0`. Android upgrades use the build number.
2. Commit the release notes and changes. Push `main` and wait for every CI job:
   Rust formatting, Rust tests/Clippy, Windows/macOS host and app bridges, Flutter
   analysis/tests, and the minified Android release with scanner verification.
3. Build from that exact clean commit with the pinned Flutter 3.44.7 and Rust
   toolchain. Run `flutter pub get --enforce-lockfile` in `app`, then
   `flutter build apk --release --target-platform android-arm64` (allow pub to
   refresh generated plugin registration after tests). Run
   `python3 scripts/check-android-scanner.py` from the repository root.
4. Verify the APK version, ARM64 native libraries, signature and SHA-256 before
   attaching it. Tag only the tested commit; tag CI verifies the version match.
5. Publish a GitHub prerelease with `StreamBeam-<version>-android-arm64.apk`,
   `SHA256SUMS`, and notes describing what was actually tested.

## Signing and upgrades

Preview downloads use the existing local Android development signing identity
so they update the builds already installed on the test phones without deleting
pairings. Keep that private keystore backed up outside the repository. Do not
upload it to GitHub or commit it. This is a preview signing arrangement, not a
Play Store production signing setup.

GitHub Actions creates a fresh development signer. Its `android-arm64-ci`
artifact proves the release build and shrinker checks, but is **not** the
published update and will not replace an installed preview. Publish the locally
signed APK only after comparing its certificate with the previous preview.

Before a public/store launch, choose a dedicated long-term production signing
identity and update `site/.well-known/assetlinks.json` as described in
`site/README.md`. Changing signer requires an explicit migration plan.
