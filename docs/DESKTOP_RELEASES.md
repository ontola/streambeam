# Desktop distribution

Ordinary CI builds complete Windows x64, macOS arm64/Intel and experimental
Linux x64 app bundles. CI Windows ZIPs are unsigned, macOS ZIPs use ad-hoc
signatures, and Linux has no native capture/video/audio/input implementation.
A successful build is not a real-device streaming test or Store certification.

## Windows

`scripts/package-windows.ps1` archives the complete Flutter release directory,
adds `streambeam-host.exe` and `streambeam-svc.exe`, and includes the Visual C++
redistributable runtime. This avoids shipping an EXE that depends on files left
behind on the build runner. The ZIP must be extracted before launch. On Windows, the workflows set
`CARGOKIT_BUILD_ROOT` to a short directory in the runner's temporary directory:
nested Opus/MSBuild tracking filenames otherwise exceed Windows' path limit.
Local long-path builds can set the same variable to a short absolute directory,
unique to that checkout. The final Flutter output bundle remains unchanged.

The manually dispatched **Desktop distribution candidates** workflow supports
`windows-store`. Set these repository variables to the exact values shown by
Partner Center under Product management / Product identity:

- `WINDOWS_STORE_IDENTITY_NAME` — Package/Identity/Name
- `WINDOWS_STORE_PUBLISHER` — Package/Identity/Publisher (including `CN=`)
- `WINDOWS_STORE_PUBLISHER_DISPLAY_NAME` — Package/Properties/PublisherDisplayName

Supply an unused four-part `windows_version` such as `0.1.3.0`. The last part
must be zero for Store submissions. No identity values are guessed. The job
creates a validated MSIX with MakeAppx; Microsoft signs it on certification.
It is **not** a signed sideload installer and is not automatically submitted.

The MSIX omits the optional SYSTEM secure-desktop service and worker. The
existing app hides the service toggle when its helper is absent. Normal desktop
sharing/viewing remain part of the app. Packaging the SYSTEM service requires
a separate service declaration, update/uninstall lifecycle, elevation review
and real-machine testing; copying the ZIP's service into MSIX would not prove
that those work. Use the direct ZIP for that capability during development.

Before submitting, run the Windows App Certification Kit, install the package
through an appropriate test signing/Store flight, and test discovery, pairing,
streaming, keyboard/mouse control and upgrade/uninstall on a clean Windows PC.
A Partner Center product, screenshots/listing, declarations and certification
are still separate requirements.

Primary references: [Store package requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/package-version-numbering),
[MSIX deployment considerations](https://learn.microsoft.com/en-us/windows/msix/packaging-tool/know-your-installer),
[Flutter Windows distribution](https://docs.flutter.dev/deployment/windows).

## macOS direct distribution

The `macos` choice in **Desktop distribution candidates** builds both Apple
Silicon and Intel release apps, signs a separate copy with Developer ID,
submits it for notarization, checks Apple's accepted status, staples the ticket,
and verifies both the signature and Gatekeeper assessment before uploading ZIPs.
The local installed app and its existing privacy permissions are never changed
by these scripts. This is direct distribution; the current app is not sandboxed
and this workflow is not a Mac App Store submission.

Set repository variable `MACOS_SIGN_IDENTITY` to the full Developer ID
Application certificate name. Set these repository secrets:

- `MACOS_DEVELOPER_ID_P12_BASE64` — certificate plus private key export
- `MACOS_DEVELOPER_ID_P12_PASSWORD` — export password
- `APPLE_NOTARY_KEY_BASE64` — App Store Connect API `.p8` key, base64 encoded
- `APPLE_NOTARY_KEY_ID` and `APPLE_NOTARY_ISSUER_ID` — matching API identifiers

Only import credentials into the isolated temporary keychain on an ephemeral
GitHub runner. Cleanup always removes that keychain, certificate and API key.
Do not commit exports or log their values. The production key is distinct from
the Apple Development identity used by the current local installation: changing
that local identity can invalidate saved macOS Screen Recording/Accessibility
permissions. Keep development installs stable.

Before public distribution, test a downloaded, quarantined ZIP on another Mac:
launch, Screen Recording/Accessibility consent, first pairing, playback and
hosting. Successful notarization alone does not verify those experiences.

Primary references: [Apple packaging](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution),
[notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## Validation boundary

The desktop workflow and scripts are prepared, not a claim of Store acceptance
or successful notarization. Account identities, signing secrets, hosted build
results, signed output checks and hardware acceptance must each be recorded
separately in PROGRESS.md as they become available.
