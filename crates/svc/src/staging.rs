//! Swapping in a new worker binary without an elevated stop/copy/start dance.
//!
//! The service runs as `LOCAL SYSTEM` and its worker runs as SYSTEM too, so a
//! developer — or an updater — cannot replace `streambeam-host.exe` while it
//! runs: Windows locks a running image against overwrite, and a non-admin
//! cannot stop the service to release it. That friction is not cosmetic. It is
//! why deployed worker binaries silently drifted a month behind the tree
//! (PROGRESS, 2026-08-18), which cost a month of a broken feature nobody could
//! see was broken.
//!
//! So the update protocol is: **drop the new binary next to the old one as
//! `<name>.new.exe`**. The service notices it on its next tick, rotates the
//! files and restarts the worker. Windows permits *renaming* a running
//! executable even though it forbids overwriting one, which is what makes the
//! swap possible with no downtime window and no elevation.
//!
//! Rollback matters more than usual here: this service is what provides remote
//! sign-in, so a bad binary would take away the very access needed to fix it.
//! The previous binary is kept as `<name>.old.exe` and the supervisor restores
//! it if the new worker cannot stay alive.
//!
//! **Security note.** A service that executes a binary any non-admin can write
//! is a privilege-escalation path to SYSTEM. That is a property of the worker
//! *directory*, not of this mechanism — it is equally true of the plain
//! `streambeam-host.exe` the service already launches. A packaged install must
//! put the worker somewhere only administrators can write; this file adds no
//! new exposure to a directory that is already writable, and removes none from
//! one that isn't.

use std::path::{Path, PathBuf};

/// Where a new worker binary is staged for pickup.
pub fn staged_path(worker: &Path) -> PathBuf {
    suffixed(worker, "new")
}

/// Where the binary being replaced is kept, for rollback.
pub fn previous_path(worker: &Path) -> PathBuf {
    suffixed(worker, "old")
}

/// Where a binary that failed to run is parked. Deliberately *not* the staged
/// path: putting it back there would have the supervisor pick the same broken
/// build up again on its next tick, forever.
pub fn rejected_path(worker: &Path) -> PathBuf {
    suffixed(worker, "rejected")
}

fn suffixed(worker: &Path, tag: &str) -> PathBuf {
    let stem = worker
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match worker.extension() {
        Some(ext) => format!("{stem}.{tag}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{tag}"),
    };
    worker.with_file_name(name)
}

/// What a pickup attempt did.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Nothing staged — the overwhelmingly common case, so it must be cheap.
    Nothing,
    /// The staged binary is in place; the worker needs restarting to run it.
    Swapped,
    /// Something was staged but wasn't usable, and nothing was changed.
    Refused(String),
}

/// Rotate a staged binary into place, keeping the current one for rollback.
///
/// Deliberately conservative: any failure leaves the running configuration
/// exactly as it was, because a service that can't launch its worker is worse
/// than one running last week's build.
pub fn apply_staged(worker: &Path) -> Applied {
    let staged = staged_path(worker);
    if !staged.is_file() {
        return Applied::Nothing;
    }
    // A half-copied file is the likeliest way to get a broken worker: the
    // service polls, so it can easily look while a copy is still in flight.
    // Requiring a plausible image first rules out the empty and truncated
    // cases without pretending to be a virus scanner.
    if let Err(why) = looks_like_program(&staged) {
        return Applied::Refused(why);
    }
    let previous = previous_path(worker);
    let _ = std::fs::remove_file(&previous);
    if worker.exists()
        && let Err(e) = std::fs::rename(worker, &previous)
    {
        return Applied::Refused(format!("could not move the current worker aside: {e}"));
    }
    if let Err(e) = std::fs::rename(&staged, worker) {
        // Put it back, or the next tick finds no worker at all.
        let _ = std::fs::rename(&previous, worker);
        return Applied::Refused(format!("could not move the staged worker into place: {e}"));
    }
    Applied::Swapped
}

/// Restore the binary that [`apply_staged`] replaced. Used when the new worker
/// won't stay running.
pub fn roll_back(worker: &Path) -> Result<(), String> {
    let previous = previous_path(worker);
    if !previous.is_file() {
        return Err("no previous worker binary to roll back to".into());
    }
    // The failed binary is kept rather than deleted — whoever staged it will
    // want to look at it, and silently destroying someone's build is a poor
    // way to report a failure — but parked under a name the supervisor won't
    // pick up again.
    let _ = std::fs::remove_file(rejected_path(worker));
    let _ = std::fs::rename(worker, rejected_path(worker));
    std::fs::rename(&previous, worker).map_err(|e| format!("restoring the previous worker: {e}"))
}

/// Cheap sanity check that a staged file is a program at all.
fn looks_like_program(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("unreadable staged worker: {e}"))?;
    if bytes.len() < 64 {
        return Err(format!(
            "staged worker is only {} bytes — a truncated or in-flight copy",
            bytes.len()
        ));
    }
    // Every Windows executable still starts with the DOS stub's "MZ".
    if &bytes[..2] != b"MZ" {
        return Err("staged worker is not a Windows executable (no MZ header)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal bytes that pass `looks_like_program`.
    fn program(tag: u8) -> Vec<u8> {
        let mut v = b"MZ".to_vec();
        v.extend(std::iter::repeat_n(tag, 100));
        v
    }

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sb-staging-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_the_staged_and_previous_files_beside_the_worker() {
        // Built with `join` rather than written as a literal Windows path:
        // backslashes are not separators on the machine this test usually runs
        // on, so a literal would make `file_name()` the whole string.
        let worker = Path::new("sb").join("streambeam-host.exe");
        let worker = worker.as_path();
        assert_eq!(
            staged_path(worker).file_name().unwrap(),
            "streambeam-host.new.exe"
        );
        assert_eq!(
            previous_path(worker).file_name().unwrap(),
            "streambeam-host.old.exe"
        );
        // Same directory, or the swap would cross a volume boundary and the
        // rename could stop being atomic.
        assert_eq!(staged_path(worker).parent(), worker.parent());
    }

    #[test]
    fn handles_a_worker_path_without_an_extension() {
        let worker = Path::new("/opt/sb/streambeam-host");
        assert_eq!(
            staged_path(worker).file_name().unwrap(),
            "streambeam-host.new"
        );
    }

    #[test]
    fn does_nothing_when_nothing_is_staged() {
        let dir = tmpdir("nothing");
        let worker = dir.join("streambeam-host.exe");
        std::fs::write(&worker, program(1)).unwrap();
        assert_eq!(apply_staged(&worker), Applied::Nothing);
        assert_eq!(std::fs::read(&worker).unwrap(), program(1));
    }

    #[test]
    fn swaps_the_staged_binary_in_and_keeps_the_old_one() {
        let dir = tmpdir("swap");
        let worker = dir.join("streambeam-host.exe");
        std::fs::write(&worker, program(1)).unwrap();
        std::fs::write(staged_path(&worker), program(2)).unwrap();

        assert_eq!(apply_staged(&worker), Applied::Swapped);
        assert_eq!(std::fs::read(&worker).unwrap(), program(2));
        assert_eq!(std::fs::read(previous_path(&worker)).unwrap(), program(1));
        // Consumed, so the next tick doesn't swap forever.
        assert!(!staged_path(&worker).exists());
    }

    #[test]
    fn refuses_a_truncated_copy_and_changes_nothing() {
        // The service polls, so it will sometimes look at a file that is still
        // being written. Swapping that in would brick the worker.
        let dir = tmpdir("truncated");
        let worker = dir.join("streambeam-host.exe");
        std::fs::write(&worker, program(1)).unwrap();
        std::fs::write(staged_path(&worker), b"MZ").unwrap();

        assert!(matches!(apply_staged(&worker), Applied::Refused(_)));
        assert_eq!(std::fs::read(&worker).unwrap(), program(1));
        assert!(
            staged_path(&worker).exists(),
            "the staged file is left alone"
        );
    }

    #[test]
    fn refuses_something_that_is_not_a_program() {
        let dir = tmpdir("notaprogram");
        let worker = dir.join("streambeam-host.exe");
        std::fs::write(&worker, program(1)).unwrap();
        std::fs::write(staged_path(&worker), vec![b'x'; 200]).unwrap();

        assert!(matches!(apply_staged(&worker), Applied::Refused(_)));
        assert_eq!(std::fs::read(&worker).unwrap(), program(1));
    }

    #[test]
    fn rolls_back_to_the_binary_that_was_replaced() {
        // The failure this exists for: the new worker won't start, and the
        // service it belongs to is the only way in to fix it.
        let dir = tmpdir("rollback");
        let worker = dir.join("streambeam-host.exe");
        std::fs::write(&worker, program(1)).unwrap();
        std::fs::write(staged_path(&worker), program(2)).unwrap();
        assert_eq!(apply_staged(&worker), Applied::Swapped);

        roll_back(&worker).unwrap();
        assert_eq!(std::fs::read(&worker).unwrap(), program(1));
        // The bad build is preserved for inspection, not deleted.
        assert_eq!(std::fs::read(rejected_path(&worker)).unwrap(), program(2));
        // And crucially not left where it would be picked up again: a rollback
        // that re-stages the binary it just rejected is an infinite loop.
        assert!(!staged_path(&worker).exists());
        assert_eq!(apply_staged(&worker), Applied::Nothing);
    }

    #[test]
    fn a_second_update_replaces_the_stale_rollback_copy() {
        let dir = tmpdir("twice");
        let worker = dir.join("streambeam-host.exe");
        std::fs::write(&worker, program(1)).unwrap();
        std::fs::write(staged_path(&worker), program(2)).unwrap();
        assert_eq!(apply_staged(&worker), Applied::Swapped);
        std::fs::write(staged_path(&worker), program(3)).unwrap();
        assert_eq!(apply_staged(&worker), Applied::Swapped);

        assert_eq!(std::fs::read(&worker).unwrap(), program(3));
        assert_eq!(std::fs::read(previous_path(&worker)).unwrap(), program(2));
    }

    #[test]
    fn rolling_back_without_a_previous_binary_is_an_error_not_a_panic() {
        let dir = tmpdir("norollback");
        let worker = dir.join("streambeam-host.exe");
        std::fs::write(&worker, program(1)).unwrap();
        assert!(roll_back(&worker).is_err());
        assert_eq!(std::fs::read(&worker).unwrap(), program(1));
    }
}
