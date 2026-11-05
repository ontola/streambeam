# Store submission guide

What it takes to get StreamBeam into each store, which parts are already in the
repo, and which parts only the publisher can do (accounts, payments, legal
answers). Build, signing and upload mechanics live in [RELEASING.md](RELEASING.md)
and [DESKTOP_RELEASES.md](DESKTOP_RELEASES.md); listing texts live in
[`distribution/store-listings.json`](../distribution/store-listings.json).

Status on 2026-11-05:

| Store | Account | App record | Signed build | Upload path | Listing/forms |
|---|---|---|---|---|---|
| Apple App Store (iOS) | Argu B.V., team `Q9WPWRTU7G` | ✅ app 6825263933 | ✅ IPA 0.1.0 (4) | ⛔ needs App Store Connect API `.p8` | draft text; privacy/age/screenshots open |
| Google Play | ⛔ none | ⛔ | ⛔ no upload key | script ready, needs service account | draft text; declarations open |
| Microsoft Store | ⛔ none | ⛔ | MSIX job ready, needs identity | manual upload in Partner Center | draft text |
| Mac App Store | not planned: app is not sandboxed. macOS ships as notarized Developer ID ZIP |
| Linux (Flathub) | not planned: Linux build is an experimental shell |

Shared prerequisites, all stores:

- Privacy policy: `https://streambeam.eu/privacy/` (source `site/privacy/index.html`).
  Must be deployed before any submission. Re-read it whenever data handling changes
  (clipboard, internet relay, crash reporting).
- Support contact: `support@streambeam.eu` is used on the privacy page and listings.
  The mailbox or forward must exist before submitting.
- Screenshots of the real release build on each platform (no mockups).

## Google Play

### Publisher steps (cannot be automated)

1. **Create a Play Console developer account** at
   <https://play.google.com/console/signup> as an **organization** (Argu B.V.),
   one-time USD 25. Use the D-U-N-S number already used for Apple. An organization
   account skips the rule that new *personal* accounts must run a closed test with
   12 testers for 14 days before production. Identity verification takes days.
2. **Create the upload key** on your Mac: `scripts/create-android-upload-key.sh`.
   Save the `.p12`, password and alias in Proton Pass, then set the four
   `STREAMBEAM_KEY*` secrets and the `STREAMBEAM_UPLOAD_CERT_SHA256` variable it prints.
3. **Create the app** in Play Console: name `StreamBeam` (fall back to
   `StreamBeam Remote Desktop` to match Apple), default language en-US, App, Free.
   Package name is fixed by the first upload: `io.streambeam.streambeam`.
   Keep **Play App Signing** on (Google generates the app-signing key).
4. **First AAB by hand**: the API cannot create the first release. Run
   *Signed store candidates and uploads* with platform `android`,
   `upload_store: false`, build number 3 or higher, download the AAB and upload
   it to an **Internal testing** release in Play Console.
5. **Service account for CI uploads**: Google Cloud project, enable the Google Play
   Android Developer API, create a service account with a JSON key, invite its
   email in Play Console (Users and permissions) with release rights for this
   app. Store the JSON as secret `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON`. From then on
   `upload_store: true` creates internal drafts.
6. **App Links**: copy the *App signing key* SHA-256 from Play Console
   (Test and release, App integrity) into `site/.well-known/assetlinks.json`,
   replacing the `REPLACE_WITH_…` entry, and deploy the site.

### App content forms (suggested answers, publisher confirms)

- **Privacy policy**: `https://streambeam.eu/privacy/`.
- **Ads**: no ads.
- **App access**: all functionality needs a second device running StreamBeam.
  Give reviewers instructions plus the demo video; there is no login.
- **Target audience**: 18+ (avoids Families policy; the app is a general tool).
- **Content rating (IARC)**: utility, no user-generated content shared publicly,
  no violence etc. Answer "yes" only to "users can interact/exchange content"
  if the questionnaire counts device-to-device screen sharing; that usually
  stays Everyone/PEGI 3.
- **Data safety**: no data collected or shared by StreamBeam itself. Data flows
  only device-to-device on the LAN, end-to-end encrypted, not to the developer;
  Google's guidance excludes that from "collection". Disclose ML Kit if Google
  still lists its diagnostics as collected by the SDK: "App info and performance:
  diagnostics, collected, not shared, not optional, purpose analytics" is the
  conservative answer. Encrypted in transit: yes. Deletion request: data is
  deleted by uninstalling.
- **Foreground service declaration** (`FOREGROUND_SERVICE_MEDIA_PROJECTION`):
  type *Media projection*, use case "the user shares their screen with their own
  paired computer or phone". Needs a short video of starting screen sharing.
- **Accessibility API declaration** (`RemoteTouchService`): StreamBeam is *not*
  an accessibility tool (`isAccessibilityTool` stays false). Purpose: let a paired
  device the user approved control this phone remotely (taps, swipes, typing),
  as TeamViewer/AnyDesk-style remote support. Point to the in-app prominent
  disclosure shown before opening Accessibility settings. Needs a video showing
  that disclosure, the consent, enabling the service and a remote tap.
  This is the declaration most likely to be rejected; ship without remote
  control first if review stalls (watching/sharing works without it).
- **Government / financial / health / news**: not applicable.

### Release path

Internal testing → closed testing (optional) → production with a staged
rollout. Preview APKs on GitHub use a different signer, so a phone with a
GitHub preview cannot upgrade to the Play build in place; uninstall first and
pair again.

## Apple App Store (iOS/iPadOS)

### Publisher steps

1. **App Store Connect API key** (Users and Access, Integrations, Team Keys,
   role App Manager). Download the `.p8` once, save it with key ID and issuer ID in
   Proton Pass, then set `APP_STORE_CONNECT_KEY_ID`, `APP_STORE_CONNECT_ISSUER_ID`,
   `APP_STORE_CONNECT_PRIVATE_KEY`. The same key can serve macOS notarization
   (`APPLE_NOTARY_*`). Without it, upload IPAs with Transporter by hand instead.
2. **Physical device check** via TestFlight: install, Local Network and camera
   consent, pairing, moving video, audio, touch input.
3. **App Privacy** questionnaire: *Data Not Collected*. Device-to-device LAN
   traffic that never reaches the developer is not "collected" under Apple's
   definition.
4. **Age rating**: all "None" → 4+.
5. **Encryption**: `ITSAppUsesNonExemptEncryption` is now `false` in
   `Info.plist`: the app only uses standard TLS 1.3/QUIC (rustls) to protect
   user data, which falls under the mass-market exemption, so App Store Connect
   stops asking per build. Confirm this answer as publisher; France distribution
   needs no extra declaration for exempt use.
6. **Review notes**: the app needs a computer running StreamBeam on the same
   network. Provide a demo video and offer a reachable test host or a call.
   Apple rejects apps it cannot exercise (guideline 2.1), so this matters.
7. Screenshots: 6.9" iPhone and 13" iPad are required sizes.
8. Submit version 0.1.0 for review, manual release.

## Microsoft Store (Windows)

1. **Partner Center account** at <https://partner.microsoft.com/dashboard>,
   company account for Argu B.V., signed in with a Microsoft Entra ID work account.
   Company registration is free since May 2026 (it used to be USD 99), see
   [Microsoft's announcement](https://blogs.windows.com/windowsdeveloper/2026/05/07/publish-to-microsoft-store-as-a-company-now-with-free-registration-and-faster-onboarding/).
   Business verification can take days.
2. **Reserve the name** `StreamBeam` (Apps and games, New product, MSIX).
3. Copy Product identity values into repository variables
   `WINDOWS_STORE_IDENTITY_NAME`, `WINDOWS_STORE_PUBLISHER`,
   `WINDOWS_STORE_PUBLISHER_DISPLAY_NAME`.
4. Run *Desktop distribution candidates* with `windows-store` and an unused
   version like `0.1.3.0`, run the Windows App Certification Kit on the MSIX,
   then upload it in a submission.
5. Capability justification: the package declares `runFullTrust` (Flutter desktop
   apps need it). Explain: "desktop streaming host that captures the screen and
   injects input for the user's own paired devices".
6. Privacy policy URL, age rating (IARC questionnaire, utility), screenshots,
   category Productivity/Utilities.

The MSIX leaves out the SYSTEM secure-desktop service (see DESKTOP_RELEASES.md).

## What the repo still needs before public launch

- Real screenshots per platform, and the two Play declaration videos.
- Physical acceptance on iPhone/iPad and on a clean Windows PC.
- Deployed `site/privacy/` and a working support address.
- Play app-signing fingerprint in `assetlinks.json`.
