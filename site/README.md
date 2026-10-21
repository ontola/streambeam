# streambeam.eu

Static site. No build step — deploy this directory as-is to any static host
(Cloudflare Pages, Netlify, GitHub Pages with a custom domain, nginx…).

## Layout

- `index.html` — landing page.
- `view/` — experimental video-only WebTransport/WebCodecs viewer; requires the
  separate native bridge, not linked as a production install option. See
  [WEB.md](../docs/WEB.md).
- `session/index.html` — target of the QR/deep link `https://streambeam.eu/session/#sbm1:…`.
  The pairing code lives in the **URL fragment** on purpose: fragments are never
  sent to the server, so the one-time token can't end up in access logs. The page
  is a fallback for phones without the app (or without verified App Links); with
  the app installed, Android opens it directly and this page is never shown.
- `.well-known/assetlinks.json` — Android App Links verification. Must be served
  at `https://streambeam.eu/.well-known/assetlinks.json` with content type
  `application/json` and **no redirect**.

## Release checklist

1. The first fingerprint in `assetlinks.json` is the shared Android **debug**
   keystore — fine for development, replace-or-keep consciously.
2. Before shipping a signed APK/AAB, add the release key fingerprint
   (replace the `REPLACE_WITH_…` entry):
   `keytool -list -v -keystore <release.keystore> -alias <alias> | grep SHA256`
   (or `cd app/android && ./gradlew signingReport`).
3. Verify after deploy:
   `adb shell pm verify-app-links --re-verify io.streambeam.streambeam`
   then `adb shell pm get-app-links io.streambeam.streambeam` should show
   `verified` for `streambeam.eu`.
