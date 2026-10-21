//! Release held input on stream loss without releasing a newer stream's keys.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use streambeam_proto::input::InputEvent;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Held {
    Key(u16),
    Button(u8),
}
impl Held {
    fn release(self) -> InputEvent {
        match self {
            Self::Key(vk) => InputEvent::Key { vk, down: false },
            Self::Button(button) => InputEvent::MouseButton {
                button,
                down: false,
            },
        }
    }
}

struct Sessions<F: Fn(InputEvent)> {
    owners: Mutex<(u64, BTreeMap<Held, u64>)>,
    next: AtomicU64,
    inject: F,
}
impl<F: Fn(InputEvent)> Sessions<F> {
    fn new(inject: F) -> Arc<Self> {
        Arc::new(Self {
            owners: Mutex::new((0, BTreeMap::new())),
            next: AtomicU64::new(1),
            inject,
        })
    }
    fn acquire(self: &Arc<Self>) -> Lease<F> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        Lease {
            sessions: self.clone(),
            id,
        }
    }
}

pub(crate) struct Lease<F: Fn(InputEvent)> {
    sessions: Arc<Sessions<F>>,
    id: u64,
}
impl<F: Fn(InputEvent)> Lease<F> {
    pub(crate) fn apply(&self, event: InputEvent) {
        // Ownership and injection share a lock: old cleanup cannot race a new down.
        let mut state = self
            .sessions
            .owners
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.0 > self.id {
            return;
        }
        if state.0 < self.id {
            // A newer authenticated connection takes input ownership only on use.
            // Its first event releases previous holds atomically with the handoff.
            for key in state.1.keys() {
                (self.sessions.inject)(key.release());
            }
            state.1.clear();
            state.0 = self.id;
        }
        let owners = &mut state.1;
        let held = match &event {
            InputEvent::Key { vk, down } if *vk <= 255 => Some((Held::Key(*vk), *down)),
            InputEvent::MouseButton { button, down } if *button <= 4 => {
                Some((Held::Button(*button), *down))
            }
            InputEvent::Key { .. } | InputEvent::MouseButton { .. } => return,
            _ => None,
        };
        if let Some((key, down)) = held {
            if down {
                owners.insert(key, self.id);
            } else if owners.get(&key) == Some(&self.id) {
                owners.remove(&key);
            } else {
                return;
            }
        }
        (self.sessions.inject)(event);
    }
}
impl<F: Fn(InputEvent)> Lease<F> {
    pub(crate) fn release(&self) {
        let mut state = self
            .sessions
            .owners
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let owners = &mut state.1;
        owners.retain(|key, owner| {
            if *owner != self.id {
                return true;
            }
            (self.sessions.inject)(key.release());
            false
        });
    }
}

impl<F: Fn(InputEvent)> Drop for Lease<F> {
    fn drop(&mut self) {
        self.release();
    }
}

type NativeSessions = Sessions<fn(InputEvent)>;
pub(crate) fn acquire() -> Lease<fn(InputEvent)> {
    static SESSIONS: OnceLock<Arc<NativeSessions>> = OnceLock::new();
    SESSIONS
        .get_or_init(|| Sessions::new(crate::input_inject::apply as fn(InputEvent)))
        .acquire()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(down: bool) -> InputEvent {
        InputEvent::Key { vk: 65, down }
    }
    #[test]
    fn disconnect_releases_pressed_keys_and_buttons_once() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let sessions = Sessions::new(move |event| output.lock().unwrap().push(event));
        let lease = sessions.acquire();
        lease.apply(key(true));
        lease.apply(key(true));
        lease.apply(InputEvent::MouseButton {
            button: 0,
            down: true,
        });
        drop(lease);
        let events = events.lock().unwrap();
        assert_eq!(events.iter().filter(|e| **e == key(false)).count(), 1);
        assert_eq!(
            events.last(),
            Some(&InputEvent::MouseButton {
                button: 0,
                down: false
            })
        );
    }
    #[test]
    fn old_release_or_cleanup_cannot_release_new_owners_key() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let sessions = Sessions::new(move |event| output.lock().unwrap().push(event));
        let old = sessions.acquire();
        old.apply(key(true));
        let new = sessions.acquire();
        new.apply(key(true));
        old.apply(key(false));
        old.apply(key(true));
        drop(old);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            &[key(true), key(false), key(true)]
        );
        drop(new);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            &[key(true), key(false), key(true), key(false)]
        );
    }
    #[tokio::test]
    async fn cancelled_input_task_releases_held_input() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let sessions = Sessions::new(move |event| output.lock().unwrap().push(event));
        let (ready, wait) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let lease = sessions.acquire();
            lease.apply(key(true));
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        wait.await.unwrap();
        task.abort();
        let _ = task.await;
        assert_eq!(events.lock().unwrap().as_slice(), &[key(true), key(false)]);
    }
}
