//! StreamBeam host service (`streambeam-svc`) — docs/SECURE_DESKTOP.md.
//!
//! Runs as `LOCAL SYSTEM`, auto-start. Its job is to keep a **host worker**
//! process alive in the *active console session* so streaming survives logout
//! and — in a later phase — can follow the secure (lock/login/UAC) desktop
//! that an in-session app can never reach. The worker is the existing
//! `streambeam-host.exe` headless binary; the service only supervises it and
//! re-launches it into whatever session is currently at the console.
//!
//! This crate is purely additive: installing/starting it is an explicit
//! opt-in, and the normal in-app host keeps working untouched until a user
//! chooses the service.
//!
//! CLI (all Windows-only; non-Windows builds are a stub so the workspace
//! still type-checks on macOS/WSL):
//!   streambeam-svc install [--worker <path>]   register with the SCM
//!   streambeam-svc uninstall                   remove it
//!   streambeam-svc start | stop                control it
//!   streambeam-svc run                          SCM entry point (not for humans)

#[cfg(windows)]
mod service_win;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    let result = match cmd {
        "install" => {
            // Optional explicit worker path; default is streambeam-host.exe
            // next to this binary.
            let worker = args
                .iter()
                .position(|a| a == "--worker")
                .and_then(|i| args.get(i + 1))
                .cloned();
            service_win::install(worker)
        }
        "uninstall" => service_win::uninstall(),
        "start" => service_win::start(),
        "stop" => service_win::stop(),
        // One-shot combinations the app's toggle invokes elevated (a single
        // UAC prompt each), so the user never runs install+start by hand.
        "enable" => {
            let worker = args
                .iter()
                .position(|a| a == "--worker")
                .and_then(|i| args.get(i + 1))
                .cloned();
            // Runs elevated-as-the-user, so it can record the user's data dir
            // for the SYSTEM worker to share (identity + pairings).
            service_win::record_worker_data_dir_pub();
            service_win::install(worker).and_then(|()| service_win::start())
        }
        "disable" => service_win::disable(),
        // Non-elevated status query (the app polls this to render the toggle);
        // prints one word to stdout.
        "status" => {
            println!("{}", service_win::status_word());
            Ok(())
        }
        "run" => service_win::run_from_scm(),
        other => {
            eprintln!(
                "streambeam-svc: unknown command {other:?}\n\
                 usage: streambeam-svc <install|uninstall|start|stop|run> [--worker <path>]"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("streambeam-svc {cmd} failed: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("streambeam-svc is Windows-only");
    std::process::exit(1);
}
