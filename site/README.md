# streambeam.eu

Static site, no framework. Hosted on **Netlify**, configured by
[`netlify.toml`](../netlify.toml) in the repo root: the "build" copies `site/`
to `_public/` minus `view/` and this README, and sets the headers below.

## Netlify setup (one time)

1. Netlify → *Add new site* → *Import an existing project* → GitHub →
   `ontola/streambeam`. Netlify reads `netlify.toml`; leave the build fields empty.
2. *Domain management* → add `streambeam.eu` (primary) and `www.streambeam.eu`.
3. DNS at the registrar, either point the nameservers to Netlify DNS
   (simplest), or set `streambeam.eu ALIAS/ANAME apex-loadbalancer.netlify.com`
   (or `A 75.2.60.5` if the registrar has no ALIAS) and
   `www CNAME <site-name>.netlify.app`.
4. Wait for the Let's Encrypt certificate, then enable *Force HTTPS*.
5. Check: `curl -sI https://streambeam.eu/.well-known/assetlinks.json` returns
   `200` with `content-type: application/json` and no redirect.

Deploys happen on every push to `main` that touches `site/` or
`netlify.toml`; pull requests get a deploy preview.

## Layout

- `index.html` — landing page.
- `404.html` — not-found page (Netlify serves it automatically).
- `view/` — experimental WebTransport/WebCodecs viewer with opt-in desktop control; requires the
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
