#!/usr/bin/env python3
"""Upload an explicit candidate to TestFlight or a Google Play internal draft.

No production release, review submission, tester invitations or account setup.
Google uses a scoped Android Publisher service account; Apple uses an ASC key.
Secrets are read from the environment, never arguments or logs.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

PACKAGE = 'io.streambeam.streambeam'
API = 'https://androidpublisher.googleapis.com/androidpublisher/v3/applications/' + PACKAGE
UPLOAD = 'https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications/' + PACKAGE
TOKEN = 'https://oauth2.googleapis.com/token'


def request(method, url, data=None, token=None, binary=False):
    headers = {'Content-Type': 'application/octet-stream' if binary else 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    if data is not None and not binary:
        data = json.dumps(data).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=240) as response:
            body = response.read()
    except urllib.error.HTTPError as error:
        # Do not echo request/response bodies containing account or key details.
        raise RuntimeError(f'Store API request failed (HTTP {error.code})') from None
    return json.loads(body) if body else {}


def b64(value):
    return base64.urlsafe_b64encode(value).rstrip(b'=').decode()


def google_token():
    account = json.loads(os.environ['GOOGLE_PLAY_SERVICE_ACCOUNT_JSON'])
    if account.get('type') != 'service_account' or account.get('token_uri') != TOKEN:
        raise ValueError('Expected a Google service account with the official OAuth endpoint')
    now = int(time.time())
    header = b64(json.dumps({'alg': 'RS256', 'typ': 'JWT'}).encode())
    claims = b64(json.dumps({'iss': account['client_email'],
        'scope': 'https://www.googleapis.com/auth/androidpublisher',
        'aud': TOKEN, 'iat': now, 'exp': now + 3600}).encode())
    content = (header + '.' + claims).encode()
    with tempfile.TemporaryDirectory(prefix='streambeam-play-') as directory:
        key = Path(directory) / 'key.pem'
        key.write_text(account['private_key'])
        key.chmod(0o600)
        result = subprocess.run(['openssl', 'dgst', '-sha256', '-sign', str(key)],
            input=content, capture_output=True)
        if result.returncode:
            raise RuntimeError('Unable to sign Google OAuth assertion')
    body = urllib.parse.urlencode({'grant_type': 'urn:ietf:params:oauth:grant-type:jwt-bearer',
        'assertion': content.decode() + '.' + b64(result.stdout)}).encode()
    req = urllib.request.Request(TOKEN, data=body,
        headers={'Content-Type': 'application/x-www-form-urlencoded'})
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            return json.load(response)['access_token']
    except urllib.error.HTTPError as error:
        raise RuntimeError(f'Google authentication failed (HTTP {error.code})') from None


def play_draft(bundle, build_number):
    data = bundle.read_bytes()
    token = google_token()
    edit_id = request('POST', API + '/edits', {}, token)['id']
    if not re.fullmatch(r'[A-Za-z0-9_-]+', edit_id):
        raise RuntimeError('Invalid edit identifier returned by Google')
    edit = API + '/edits/' + edit_id
    committed = False
    try:
        uploaded = request('POST', UPLOAD + '/edits/' + edit_id + '/bundles?uploadType=media',
            data, token, binary=True)
        if str(uploaded['versionCode']) != str(build_number):
            raise RuntimeError('Uploaded bundle version differs from the requested build')
        if uploaded.get('sha256', '').lower() != hashlib.sha256(data).hexdigest():
            raise RuntimeError('Google bundle digest differs from the local candidate')
        # Refuse to replace another pending internal release silently.
        track = request('GET', edit + '/tracks/internal', token=token)
        if any(r.get('status') != 'completed' for r in track.get('releases', [])):
            raise RuntimeError('An internal draft or rollout already exists; resolve it in Play Console first')
        releases = track.get('releases', []) + [{'name': 'StreamBeam ' + str(build_number),
            'versionCodes': [str(build_number)], 'status': 'draft'}]
        request('PUT', edit + '/tracks/internal', {'track': 'internal', 'releases': releases}, token)
        request('POST', edit + ':validate', token=token)
        request('POST', edit + ':commit?changesNotSentForReview=true&changesInReviewBehavior=ERROR_IF_IN_REVIEW', token=token)
        committed = True
    finally:
        if not committed:
            try:
                request('DELETE', edit, token=token)
            except Exception:
                print('The uncommitted Play edit could not be deleted; inspect Play Console.')
    print(f'Uploaded verified build {build_number} to a Play internal draft. Not released to testers.')


def testflight(ipa):
    key_id = os.environ['APP_STORE_CONNECT_KEY_ID']
    issuer = os.environ['APP_STORE_CONNECT_ISSUER_ID']
    if not re.fullmatch(r'[A-Za-z0-9]+', key_id):
        raise ValueError('Invalid App Store Connect key ID')
    if not re.fullmatch(r'[A-Fa-f0-9-]{36}', issuer):
        raise ValueError('Invalid App Store Connect issuer ID')
    with tempfile.TemporaryDirectory(prefix='streambeam-asc-') as directory:
        key = Path(directory) / ('AuthKey_' + key_id + '.p8')
        key.write_text(os.environ['APP_STORE_CONNECT_PRIVATE_KEY'])
        key.chmod(0o600)
        env = os.environ.copy()
        env['API_PRIVATE_KEYS_DIR'] = directory
        subprocess.run(['xcrun', 'altool', '--use-old-altool', '--validate-app', '-f', str(ipa), '-t', 'ios',
            '--apiKey', key_id, '--apiIssuer', issuer], env=env, check=True)
        subprocess.run(['xcrun', 'altool', '--use-old-altool', '--upload-app', '-f', str(ipa), '-t', 'ios',
            '--apiKey', key_id, '--apiIssuer', issuer], env=env, check=True)
    print('Apple accepted the upload. Processing and TestFlight availability still need verification.')


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination', choices=['play-draft', 'testflight'])
    parser.add_argument('artifact', type=Path)
    parser.add_argument('--build-number', type=int)
    args = parser.parse_args()
    if args.destination == 'play-draft':
        if not args.build_number or not 2 < args.build_number <= 2100000000:
            parser.error('Play requires an unused build number greater than 2')
        if args.artifact.suffix != '.aab':
            parser.error('Play requires an .aab')
        play_draft(args.artifact, args.build_number)
    else:
        if args.artifact.suffix != '.ipa':
            parser.error('TestFlight requires an .ipa')
        testflight(args.artifact)


if __name__ == '__main__':
    main()
