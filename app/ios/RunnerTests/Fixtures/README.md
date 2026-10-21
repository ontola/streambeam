These fixtures are extracted without re-encoding from
`crates/devtools/assets/test.h264`, the shared 640x360 test clip:

- `config.h264`: Annex-B SPS and PPS NAL units (types 7 and 8).
- `keyframe.h264`: the first Annex-B IDR NAL unit (type 5).

The native iOS test verifies that VideoToolbox produces a non-uniform BGRA
pixel buffer and notifies the Flutter texture registry. It also verifies
clean teardown and actionable failure for malformed codec configuration.
Run `scripts/test-ios-native.sh` with an installed iOS simulator runtime.
This does not replace physical LAN pairing, audio and input acceptance.
