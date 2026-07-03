pub use streambeam_core::annex_b::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_clip_parses_sixty_access_units() {
        let data = include_bytes!("../assets/test.h264");
        assert_eq!(split_nal_units(data).len(), 65);
        let aus = split_access_units(data);
        assert_eq!(aus.len(), 60, "expected 2s @ 30fps");
        let idr = aus.iter().filter(|a| a.keyframe).count();
        assert_eq!(idr, 2, "GOP=30 → 2 IDR in 60 frames");
    }
}
