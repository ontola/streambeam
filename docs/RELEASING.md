# Android preview releases

The first downloadable release is `v0.1.0` (Android version code 2). These are
LAN-streaming previews, not the completed M8/v1 milestone. Android ARM64 is the
packaged mobile target; desktop bundles are described in [DESKTOP_RELEASES.md](DESKTOP_RELEASES.md). CI cannot
validate GPU capture, physical controllers, phone cameras or live latency.

## Before publishing

1. Update `app/pubspec.yaml`: match the tag's version and increase the build
   number, even when a preview's semantic version is lower than an old local
   development build's `1.0.0`. Android upgrades use the build number.
2. Commit the release notes and changes. Push `main` and wait for every CI job:
   Rust formatting, Rust tests/Clippy, Windows/macOS/Linux app bundles, Flutter
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


## Store candidates (2026-10-20)

The **Signed store candidates** workflow builds downloadable artifacts on
explicit dispatch. It does not upload to Apple/Google, create store listings,
or imply that either platform has passed device acceptance.
Choose an unused increasing build number greater than the existing Android
preview's version code 2. Use the same bundle/package ID:
`io.streambeam.streambeam`.

### Android signing

Configure these repository secrets before dispatching the Android job:

- `STREAMBEAM_KEYSTORE_BASE64`: base64 PKCS12 upload keystore.
- `STREAMBEAM_KEYSTORE_PASSWORD`: keystore password.
- `STREAMBEAM_KEY_ALIAS`: alias inside the keystore.
- `STREAMBEAM_KEY_PASSWORD`: private key password.

Set repository variable `STREAMBEAM_UPLOAD_CERT_SHA256` to that key's public
SHA-256 certificate fingerprint. The job builds an ARM64 AAB and verifies its
JAR signature, pinned certificate, non-debug signer and exact native ABI
payload before uploading the artifact. Secrets are not exposed on PR builds.
An unsigned release bundle or partial signing configuration fails explicitly;
APK previews can still use their existing development identity.

The upload key is separate from the **app signing key** used by Google Play
to sign installed APKs. Add the Play app signing certificate, not merely the
upload certificate, to the hosted `assetlinks.json` before claiming App Links
work. Existing debug-signed previews cannot be updated in place by a
different Play signer. Do not uninstall previews or discard pairing data as
part of store setup; establish the migration separately.

Official reference:
[Android app signing](https://developer.android.com/studio/publish/app-signing).

### iOS signing and current blocker

The iOS job expects `IOS_DISTRIBUTION_P12_BASE64`,
`IOS_DISTRIBUTION_P12_PASSWORD` and `IOS_APP_STORE_PROFILE_BASE64`.
The profile must be an unexpired App Store distribution profile for the
exact bundle ID; development, ad hoc and enterprise profiles are rejected.
Signing uses a temporary CI keychain and an ignored Runner-only xcconfig,
then removes the signing material. Exported IPAs remain artifacts, with no
automatic upload or submission.

**iOS is not currently buildable or ready for TestFlight.** An actual device
build on 2026-10-20 exposed:

1. Xcode 27 rejects the old iOS 13 deployment target. Runner/Pods now target 15.
2. The vendored Opus CMake project requires
   `CMAKE_POLICY_VERSION_MINIMUM=3.5` with CMake 4; the workflow sets it.
3. `app/rust/src/audio_play.rs` has no iOS `sink`, so Rust compilation fails.
   The iOS Runner also has no `streambeam/video_desktop` channel or native
   decoder; resolving compilation alone would not provide a functioning app.

Implement and test iOS video/audio, camera/local-network permissions and
pairing/input on a real device before distributing a build. The signing job
is prepared but its signed build/export has not been exercised.

App Store Connect still requires an authenticated account and an app record
before an upload. See
[Apple's app record setup](https://developer.apple.com/help/app-store-connect/create-an-app-record/add-a-new-app).
Neither store registration nor an upload was completed in this session.
No permanent Android upload key was generated: the approval request for
creating it and saving it in `joepio/streambeam` Actions secrets is pending.

Validation for this preparation: 122 Rust tests passed (one manual soak
ignored), full Flutter analysis and all 53 Flutter tests passed, Rust formatting
and Clippy passed with existing warnings, optimized Mac build and signed
installation passed, Android ARM64 release APK and all three ML Kit registrar
checks passed. The unsigned App Bundle rejection is exercised in CI.
A signed AAB/IPA and hosted CI have not yet been verified.
