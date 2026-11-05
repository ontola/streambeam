# Releases and store delivery

The first downloadable release is `v0.1.0` (Android version code 2). These are
LAN-streaming previews, not the completed M8/v1 milestone. Ordinary CI now builds
Android ARM64, Windows x64, macOS Apple Silicon/Intel, an experimental Linux x64
shell, and a complete unsigned iOS app. Browser fixture tests cover the optional
video/audio/control bridge separately. See [DESKTOP_RELEASES.md](DESKTOP_RELEASES.md),
[IOS.md](IOS.md) and [WEB.md](WEB.md). Build artifacts do not establish production
signing, actual install/upgrade behavior, physical input, cameras or live latency.

Per-store accounts, forms and publisher steps: [STORE_SUBMISSION.md](STORE_SUBMISSION.md).

## Current store status — 2026-10-21

The Apple bundle identifier and App Store Connect app record have been created:

| Field | Registered value |
|---|---|
| App Store name | **StreamBeam Remote Desktop**; `StreamBeam` was unavailable |
| Bundle ID | `io.streambeam.streambeam` |
| SKU | `streambeam-ios` |
| Apple app ID | `6825263933` |
| Publisher / team | Argu B.V. / `Q9WPWRTU7G` |
| Store version / state | iOS **0.1.0**, **Prepare for Submission** |
| Account record | [App Store Connect](https://appstoreconnect.apple.com/apps/6825263933/distribution) |

A local App Store distribution IPA **0.1.0 (3)** was exported on 2026-10-21.
Strict code-signature verification, the embedded distribution profile and the
Rust framework privacy manifest were checked in the exported artifact. The
dedicated Apple Distribution certificate and matching App Store profile are
validated, and all three signing secrets are configured in this repository.
This is a signed export; no upload, Apple processing, TestFlight availability or
public App Store release is verified.

The [hosted signed build 4](https://github.com/joepio/streambeam/actions/runs/39608208468)
passed on merge commit `6dc295c6c902356d945e4f84be6670bacd5bac5d`. The downloaded
IPA **0.1.0 (4)** independently passed strict signature, exact profile/certificate,
production entitlement, bundle/team and Rust privacy-manifest checks. Its SHA-256
is `d84ac566b377e3ddb819ba34357751a3055be82d2965ba85dfe4f03e6757459d`.
CI removed its temporary signing material and skipped the upload step. The app record's
marketing version is now **0.1.0**, matching source **0.1.0+2**, and its draft
description/keywords are saved with manual release selected.

The App Store Connect API-key download failed in the in-app browser. The first
unused key was revoked with user approval; its replacement is not yet backed up
or configured for CI: its `.p8` private file has not been recovered. Do not claim
upload access until that file is saved and authenticated. A private credential
inventory is ready. On 2026-10-21 the publisher confirmed saving the Apple-compatible
P12, its password, the distribution profile and the inventory in Proton Pass.
This is user-confirmed storage; a restore test has not been performed. The API
private key is still missing and is not included in that backup.

Google Play registration/upload and Microsoft Store identity/submission are
still unverified. Keep their draft listings separate from Apple's registered
name and record. No public store installation is available from this work yet.

## Verified build and device status — 2026-10-21

All ten hosted checks passed on exact commit
`5a99a2799340a1460f2d63e7119d7c02b93ea50b`, subsequently merged through PR #3:
[ordinary CI](https://github.com/joepio/streambeam/actions/runs/39605216105),
[iOS preview](https://github.com/joepio/streambeam/actions/runs/39605216489), and
[browser preview](https://github.com/joepio/streambeam/actions/runs/39605216141).
This includes Windows x64, both Mac architectures, Linux's experimental shell,
Android release/scanner checks, unsigned iOS/native decoder tests, Rust checks,
Flutter analysis/tests, and real fixture media/input through the browser bridge.

The exact commit is installed on the Mac with its existing signing identity and
macOS privacy permissions preserved. A real Mac/Mancave session displayed
nonblank **3840 × 2160 AV1** video. **120 fps is the selected target, not a
measured delivered frame rate.** The matching Windows artifact is now installed
on Mancave after an explicitly approved process-only execution-policy exception.
Persistent AllSigned, the supervisor, configuration, host identity and pairings
are unchanged; a rollback copy is retained. Worker/app/Rust DLL hashes match the
artifact and the supervisor confirmed the updated worker stayed running. A real
updated-Mac to updated-Windows 4K AV1 stream passed, and Stop returned the Mac to
Ready with MANCAVE still Saved. Remote-input and subjective-audio acceptance were
not part of this deployment check.

The final Android artifact has not been installed on a phone because ADB is
offline. Xiaomi's last verified in-place preview update was `604ce0e`, with the
existing preview signer. Permanent Android signing and Google Play access remain
pending. Browser CI proves the development bridge against the fixture; it does
not establish native-host browser sharing or a deployed public viewer.

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
The original OpenSSL 3 PKCS12 export could not be imported by Apple's keychain.
Re-exporting the **same key and certificate** with explicit 3DES encryption and
a SHA-1 MAC produced an Apple-compatible P12; the corrected encrypted secret
is installed. This changes the export container, not the app's signing identity.
Preserve this compatibility when renewing/exporting the credential.

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

- App Store distribution IPA 0.1.0 (3) exported successfully; strict signature,
  profile and embedded Rust privacy-manifest verification passed.
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
physical iPhone/iPad. Local and hosted signed exports are verified; Apple processing and
TestFlight availability remain separate checks. The
registered app record above resolves the old registration blocker; it does not resolve these acceptance requirements.

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

## Credential recovery

GitHub Actions secrets cannot serve as a recoverable backup. Keep the dedicated
Apple distribution P12 and its password, the App Store profile, API private key
with key/issuer IDs, and any Android upload keystore/password/alias in the
publisher's password manager. On 2026-10-21 the publisher confirmed saving the
Apple-compatible P12, its password, the profile and the private inventory in
Proton Pass. The missing API private key must still be added; recovery from the
vault has not been tested.
Verify recovery of the saved items before removing local originals or treating
GitHub-secret installation as completion of credential custody.
Never commit keys or include them in diagnostic artifacts. Certificate/profile
renewal is separate from replacing an installed app's signing identity.
