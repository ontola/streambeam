//! Start the bundled [VIIPER](https://github.com/Alia5/VIIPER) server when the host boots.
//!
//! Gamepad injection talks to `127.0.0.1:3242`. If nothing is listening yet, we try to
//! launch `viiper.exe server` from the same directory as `streambeam-host.exe`.

#[cfg(windows)]
mod imp {
    use std::net::ToSocketAddrs;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use viiper_client::client::ViiperClient;

    const API_ADDR: &str = "127.0.0.1:3242";

    pub struct ViiperGuard {
        child: Option<Child>,
    }

    impl ViiperGuard {
        pub fn ensure() -> Self {
            if api_reachable() {
                tracing::info!("VIIPER server already running");
                return Self { child: None };
            }
            let Some(exe) = locate_viiper_exe() else {
                tracing::warn!(
                    "VIIPER binary not found next to streambeam-host — gamepad injection \
                     requires bundled viiper.exe + usbip-win2 (Windows installer, M8)"
                );
                return Self { child: None };
            };
            match Command::new(&exe)
                .arg("server")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    tracing::info!("started VIIPER server ({})", exe.display());
                    for _ in 0..20 {
                        if api_reachable() {
                            return Self { child: Some(child) };
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    tracing::warn!("VIIPER server started but API not reachable yet");
                    Self { child: Some(child) }
                }
                Err(e) => {
                    tracing::warn!("failed to start VIIPER server: {e}");
                    Self { child: None }
                }
            }
        }
    }

    impl Drop for ViiperGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
                tracing::info!("stopped bundled VIIPER server");
            }
        }
    }

    fn api_reachable() -> bool {
        let Ok(addr) = API_ADDR.to_socket_addrs().and_then(|mut i| {
            i.next()
                .ok_or_else(|| std::io::Error::other("no VIIPER API address"))
        }) else {
            return false;
        };
        ViiperClient::new(addr).ping().is_ok()
    }

    fn locate_viiper_exe() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("STREAMBEAM_VIIPER_EXE") {
            let p = PathBuf::from(path);
            if p.is_file() {
                return Some(p);
            }
        }
        let host = std::env::current_exe().ok()?;
        let dir = host.parent()?;
        for name in ["viiper.exe", "VIIPER.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }
}

#[cfg(windows)]
pub use imp::ViiperGuard;

#[cfg(not(windows))]
pub struct ViiperGuard;

#[cfg(not(windows))]
impl ViiperGuard {
    pub fn ensure() -> Self {
        Self
    }
}
