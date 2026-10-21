# Releases and store delivery

The first downloadable release is `v0.1.0` (Android version code 2). These are
LAN-streaming previews, not the completed M8/v1 milestone. Ordinary CI now builds
Android ARM64, Windows x64, macOS Apple Silicon/Intel, an experimental Linux x64
shell, and a complete unsigned iOS app. Browser fixture tests cover the optional
video bridge separately. See [DESKTOP_RELEASES.md](DESKTOP_RELEASES.md),
[IOS.md](IOS.md) and [WEB.md](WEB.md). Build artifacts do not establish production
signing, actual install/upgrade behavior, physical input, cameras or live latency.

## Current store status — 2026-10-21

The Apple bundle identifier and App Store Connect app record have been created:

| Field | Registered value |
|---|---|
| App Store name | **StreamBeam Remote Desktop**; `StreamBeam` was unavailable |
| Bundle ID | `io.streambeam.streambeam` |
| SKU | `streambeam-ios` |
| Apple app ID | `6825263933` |
| Publisher / team | Argu B.V. / `Q9WPWRTU7G` |
| Store version / state | iOS **1.0**, **Prepare for Submission** |
| Account record | [App Store Connect](https://appstoreconnect.apple.com/apps/6825263933/distribution) |

This is registration, not an App Store release or TestFlight build. There is no
verified signed IPA or upload yet. Apple signing/API credential setup is
underway following user authorization; completing that setup still requires
verification. The app source currently builds **0.1.0+2**: align the App Store
marketing version with the intended binary version before selecting a build.
Do not assume the record's initial 1.0 means the application has reached v1.

Google Play registration/upload and Microsoft Store identity/submission are
still unverified. Keep their draft listings separate from Apple's registered
name and record. No public store installation is available from this work yet.

## Before publishing

1. Update `app/pubspec.yaml`: match the tag's version and increase the build
   number, even when a preview's semantic version is lower than an old local
   development build's `1.0.0`. Android upgrades use the build number.
2. Commit the release notes and changes. Push `main` and wait for every CI job:
   Rust formatting, Rust tests/Clippy, Windows/macOS/Linux app bundles, Flutter
   analysis/tests, the minified Android release with scanner verification, the
   unsigned iOS build/native simulator tests, and browser-preview checks.
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


## Signed store candidates

The **Signed store candidates and uploads** workflow builds downloadable artifacts
on explicit dispatch. With `upload_store: true`, it validates/uploads an IPA to
TestFlight or uploads a verified AAB to a Google Play internal **draft**. It never
submits a public release, invites testers, or implies device acceptance. With the
default `upload_store: false`, it only builds artifacts.
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

### iOS signing and remaining acceptance

The iOS job expects `IOS_DISTRIBUTION_P12_BASE64`,
`IOS_DISTRIBUTION_P12_PASSWORD` and `IOS_APP_STORE_PROFILE_BASE64`.
The profile must be an unexpired App Store distribution profile for the
exact bundle ID; development, ad hoc and enterprise profiles are rejected.
Signing uses a temporary CI keychain and an ignored Runner-only xcconfig,
then removes the signing material. Exported IPAs remain artifacts by default;
explicit upload is described below. Never include private credentials in source,
logs or the listing metadata.

**The iOS viewer now builds and has native video/audio integration.** The
previous missing Rust sink and native decoder channel have been implemented.
Runner/Pods target iOS 15; builds set `CMAKE_POLICY_VERSION_MINIMUM=3.5` for
vendored Opus. Bonjour discovery, camera/local-network permission descriptions,
QR pairing, saved hosts, touch/keyboard controls and foreground lifecycle are
wired. The viewer advertises SDR H.264; it does not host the iPhone/iPad screen.

Local validation on 2026-10-21:

- Complete unsigned device release built successfully (29.3 MB).
- Full Flutter analysis and 54 tests passed for the iOS implementation; later
  integrated discovery work increased the shared suite to 59 tests.
- Three Rust audio tests passed, including PCM conversion and silence after stop.
- Three iPhone simulator tests passed through `scripts/test-ios-native.sh`,
  with an inspected successful xcresult: non-uniform H.264 pixels, texture
  notification/teardown, audio-engine startup/stop and malformed-config errors.
- A full macOS ARM64 release and six existing native decoder regressions passed
  with the shared Apple code, including AV1, HDR10 and MetalFX. The installed
  Mac app and its signing identity were not changed by these tests.

The `iOS preview` workflow builds the unsigned device app, archives it with its
SHA-256, and runs the native simulator tests on ordinary pushes/PRs. Those
artifacts do not install on a physical iPhone without appropriate signing.
Check hosted results for the exact commit; local results are not hosted CI proof.

Before public submission, verify installation, Local Network/camera consent,
first pairing, reconnection, moving video, audible A/V and remote input on a
physical iPhone/iPad. Signed export, Apple processing and TestFlight availability
must each be checked separately. The registered app record above resolves the
old registration blocker; it does not resolve these acceptance requirements.

Android upload-key setup and a successful signed AAB also remain unverified.
The unsigned App Bundle rejection remains an ordinary CI check.

## Store uploads and listing assets (2026-10-21)

`scripts/upload-store.py` adds explicit upload paths to the candidate workflow:

- Play: `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON` must be a Google service account with
  access to this app and its test releases. The app record and initial Play
  setup must already exist. The script checks Google's version code and SHA-256
  against the candidate, preserves the current completed internal release,
  refuses to overwrite pending drafts/rollouts or cancel a review, validates
  and commits an internal draft. It does not release the draft to testers.
- Apple: `APP_STORE_CONNECT_KEY_ID`, `APP_STORE_CONNECT_ISSUER_ID` and
  `APP_STORE_CONNECT_PRIVATE_KEY` provide upload access for the existing app
  record. The private key exists only in a temporary directory for validation
  and upload. The documented `--use-old-altool` compatibility mode supports the
  pinned macOS runner and current local Xcode. Successful upload must still be
  followed by checking Apple processing and TestFlight availability.

Seven mocked boundary tests cover digest/version rejection, preservation of
existing releases, refusal of pending edits, cleanup and credential endpoints.
They run in ordinary CI, without credentials. They are not live store proof.

Draft descriptions and the remaining submission fields live in
`distribution/store-listings.json`. Android's permission help now discloses
remote taps/typing and local focused-text access before the user agrees to open
Accessibility settings. A Play declaration and demonstration video remain
required; the dialog change alone does not establish policy approval.

Desktop signed/notarized and Windows MSIX delivery are documented in
[DESKTOP_RELEASES.md](DESKTOP_RELEASES.md).

References: [Google edits](https://developers.google.com/android-publisher/api-ref/rest/v3/edits/commit),
[Apple uploads](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds/),
[Accessibility declaration](https://support.google.com/googleplay/android-developer/answer/10964491).
