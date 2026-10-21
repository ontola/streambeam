#!/usr/bin/env python3
"""Check release boundaries without contacting a store or reading credentials."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('upload', Path(__file__).with_name('upload-store.py'))
upload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upload)

class PlayUploadTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.bundle = Path(self.directory.name) / 'app.aab'
        self.bundle.write_bytes(b'candidate bytes')
        self.accepted = {'versionCode': 3, 'sha256': hashlib.sha256(self.bundle.read_bytes()).hexdigest()}
        self.calls = []

    def run_upload(self, uploaded=None, releases=None, fail_validate=False):
        def api(method, url, data=None, token=None, binary=False):
            self.calls.append((method, url, data))
            if url.endswith('/edits'):
                return {'id': 'edit-123'}
            if '/bundles?' in url:
                self.assertTrue(binary)
                self.assertEqual(data, self.bundle.read_bytes())
                return self.accepted if uploaded is None else uploaded
            if method == 'GET':
                return {'releases': releases or []}
            if url.endswith(':validate') and fail_validate:
                raise RuntimeError('validation failed')
            return {}
        with patch.object(upload, 'google_token', return_value='test-token'), patch.object(upload, 'request', side_effect=api):
            upload.play_draft(self.bundle, 3)

    def test_upload_is_draft_and_preserves_current_release(self):
        current = {'status': 'completed', 'versionCodes': ['2']}
        self.run_upload(releases=[current])
        put = next(call for call in self.calls if call[0] == 'PUT')
        self.assertTrue(put[1].endswith('/tracks/internal'))
        self.assertEqual(put[2]['releases'], [current, {'name': 'StreamBeam 3', 'versionCodes': ['3'], 'status': 'draft'}])
        self.assertIn(':commit?changesNotSentForReview=true&changesInReviewBehavior=ERROR_IF_IN_REVIEW', self.calls[-1][1])
        self.assertFalse(any(call[0] == 'DELETE' for call in self.calls))

    def test_wrong_version_rolls_back_without_track_change(self):
        with self.assertRaisesRegex(RuntimeError, 'version differs'):
            self.run_upload(uploaded={**self.accepted, 'versionCode': 4})
        self.assertEqual(self.calls[-1][0], 'DELETE')
        self.assertFalse(any(call[0] == 'PUT' for call in self.calls))

    def test_wrong_digest_rolls_back(self):
        with self.assertRaisesRegex(RuntimeError, 'digest differs'):
            self.run_upload(uploaded={**self.accepted, 'sha256': '0' * 64})
        self.assertEqual(self.calls[-1][0], 'DELETE')

    def test_pending_release_is_not_overwritten(self):
        with self.assertRaisesRegex(RuntimeError, 'already exists'):
            self.run_upload(releases=[{'status': 'draft'}])
        self.assertFalse(any(call[0] == 'PUT' for call in self.calls))
        self.assertEqual(self.calls[-1][0], 'DELETE')

    def test_validation_failure_cannot_commit(self):
        with self.assertRaisesRegex(RuntimeError, 'validation failed'):
            self.run_upload(fail_validate=True)
        self.assertFalse(any(':commit' in call[1] for call in self.calls))
        self.assertEqual(self.calls[-1][0], 'DELETE')

    def test_untrusted_token_endpoint_rejected(self):
        with patch.dict(upload.os.environ, {'GOOGLE_PLAY_SERVICE_ACCOUNT_JSON': '{"type":"service_account","token_uri":"https://invalid.example"}'}):
            with self.assertRaisesRegex(ValueError, 'official OAuth'):
                upload.google_token()

class AppleUploadTests(unittest.TestCase):
    def test_key_removed_when_validation_fails(self):
        locations = []
        def command(args, env, check):
            key = Path(env['API_PRIVATE_KEYS_DIR']) / 'AuthKey_TEST123.p8'
            locations.append(key)
            self.assertEqual(key.read_text(), 'test private key')
            self.assertEqual(key.stat().st_mode & 0o777, 0o600)
            raise RuntimeError('validation failed')
        with patch.dict(upload.os.environ, {'APP_STORE_CONNECT_KEY_ID': 'TEST123',
            'APP_STORE_CONNECT_ISSUER_ID': '12345678-1234-1234-1234-123456789abc',
            'APP_STORE_CONNECT_PRIVATE_KEY': 'test private key'}), patch.object(upload.subprocess, 'run', side_effect=command):
            with self.assertRaisesRegex(RuntimeError, 'validation failed'):
                upload.testflight(Path('app.ipa'))
        self.assertEqual(len(locations), 1)
        self.assertFalse(locations[0].exists())

if __name__ == '__main__':
    unittest.main()
