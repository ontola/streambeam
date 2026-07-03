//! Shared stream control between fakehost control + media tasks.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::media::VideoCatalog;

/// Signals from the control stream to the media sender.
#[derive(Debug)]
pub struct StreamControl {
    pub force_idr: AtomicBool,
    pub catalog_index: AtomicUsize,
    pub catalog_len: usize,
}

impl StreamControl {
    pub fn new(catalog: &VideoCatalog) -> Self {
        Self {
            force_idr: AtomicBool::new(false),
            catalog_index: AtomicUsize::new(0),
            catalog_len: catalog.len(),
        }
    }

    pub fn request_idr(&self, catalog: &VideoCatalog) {
        let cur = self.catalog_index.load(Ordering::Relaxed);
        let prev = catalog.previous_keyframe_index(cur);
        self.catalog_index.store(prev, Ordering::Relaxed);
        self.force_idr.store(true, Ordering::Relaxed);
    }

    pub fn take_force_idr(&self) -> bool {
        self.force_idr.swap(false, Ordering::Relaxed)
    }

    pub fn advance(&self) -> usize {
        let cur = self.catalog_index.load(Ordering::Relaxed);
        let next = (cur + 1) % self.catalog_len;
        self.catalog_index.store(next, Ordering::Relaxed);
        next
    }

    pub fn current_index(&self) -> usize {
        self.catalog_index.load(Ordering::Relaxed)
    }
}
