mod api;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. This line may not be accurate, and you can change it according to your needs. */
mod input;
mod playback;
mod store;

#[cfg(any(target_os = "windows", target_os = "android", target_os = "macos"))]
mod host;

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "macos")]
mod macos;
