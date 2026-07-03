pub mod audio;
pub mod pipe_player;

// Legacy module names — re-export pipe player.
pub use pipe_player::{AudioPlayer, VideoPlayer};
