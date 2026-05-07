//! 1-slot mailbox carrying VLC's "new output texture" events from libvlc's
//! render thread to the Godot render-thread importer. Latest-wins shape:
//! push replaces, the previous event's NT handle closes on drop.

use std::sync::Mutex;

use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// Emitted by `update_output_cb` on every (re-)allocation. Closes the NT
/// handle on drop so a never-consumed event can't leak.
pub struct OutputEvent {
    pub handle: HANDLE,
    pub width: u32,
    pub height: u32,
}

unsafe impl Send for OutputEvent {}

impl OutputEvent {
    /// Take the handle without closing it. Caller is then responsible for
    /// closing it (or letting the dropped event close it).
    #[allow(dead_code)]
    pub fn into_parts(mut self) -> (HANDLE, u32, u32) {
        let handle = std::mem::take(&mut self.handle);
        (handle, self.width, self.height)
    }
}

impl Drop for OutputEvent {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe { _ = CloseHandle(self.handle) };
        }
    }
}

pub struct EventMailbox {
    slot: Mutex<Option<OutputEvent>>,
}

impl EventMailbox {
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    /// Replace any pending event. The prior event (if any) is dropped,
    /// closing its NT handle.
    pub fn push(&self, e: OutputEvent) {
        let _prev = self
            .slot
            .lock()
            .expect("event mailbox poisoned")
            .replace(e);
    }

    pub fn take(&self) -> Option<OutputEvent> {
        self.slot
            .lock()
            .expect("event mailbox poisoned")
            .take()
    }
}

impl Default for EventMailbox {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fake_event(width: u32, height: u32) -> OutputEvent {
        OutputEvent {
            handle: HANDLE::default(),
            width,
            height,
        }
    }

    #[test]
    fn empty_mailbox_returns_none() {
        let mb = EventMailbox::new();
        assert!(mb.take().is_none());
    }

    #[test]
    fn push_then_take_round_trip() {
        let mb = EventMailbox::new();
        mb.push(fake_event(1920, 1080));
        let got = mb.take().expect("event should be present");
        assert_eq!(got.width, 1920);
        assert_eq!(got.height, 1080);
    }

    #[test]
    fn second_push_supersedes_first() {
        let mb = EventMailbox::new();
        mb.push(fake_event(640, 480));
        mb.push(fake_event(1280, 720));
        let got = mb.take().expect("event should be present");
        assert_eq!((got.width, got.height), (1280, 720));
        assert!(mb.take().is_none());
    }

    #[test]
    fn take_after_drain_is_empty_again() {
        let mb = EventMailbox::new();
        mb.push(fake_event(800, 600));
        let _ = mb.take();
        assert!(mb.take().is_none());
        mb.push(fake_event(1024, 768));
        let got = mb.take().expect("re-push should be observable");
        assert_eq!((got.width, got.height), (1024, 768));
    }

    #[test]
    fn into_parts_extracts_handle() {
        let e = fake_event(100, 200);
        let (handle, w, h) = e.into_parts();
        assert!(handle.is_invalid());
        assert_eq!((w, h), (100, 200));
    }

    #[test]
    fn cross_thread_send() {
        let mb = Arc::new(EventMailbox::new());
        let producer = {
            let mb = mb.clone();
            std::thread::spawn(move || {
                mb.push(fake_event(42, 7));
            })
        };
        producer.join().expect("producer thread");
        let got = mb.take().expect("event delivered across threads");
        assert_eq!((got.width, got.height), (42, 7));
    }
}
