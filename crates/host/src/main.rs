//! Host binary — Windows capture/encode in M2. Mock/demo on other platforms.

fn main() {
    eprintln!("streambeam-host: Windows capture pipeline not wired yet (M2).");
    eprintln!("Use fakehost + playclient for the macOS dev loop.");
    #[cfg(not(windows))]
    {
        use streambeam_host::{mock::MockEncoder, VideoEncoder};
        let mut enc = MockEncoder::bundled();
        let first = enc.encode_next().unwrap().expect("frame");
        eprintln!(
            "mock encoder OK — first frame {} bytes, keyframe={}",
            first.data.len(),
            first.keyframe
        );
    }
}
