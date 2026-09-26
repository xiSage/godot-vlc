/*
* Copyright (c) 2025 xiSage
*
* This library is free software; you can redistribute it and/or
* modify it under the terms of the GNU Lesser General Public
* License as published by the Free Software Foundation; either
* version 2.1 of the License, or (at your option) any later version.
*
* This library is distributed in the hope that it will be useful,
* but WITHOUT ANY WARRANTY; without even the implied warranty of
* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
* Lesser General Public License for more details.
*
* You should have received a copy of the GNU Lesser General Public
* License along with this library; if not, write to the Free Software
* Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301
* USA
*/

//! What the video and audio callbacks of one player write into, and the tombstone
//! that keeps it readable after the player's wrapper is gone.
//!
//! libvlc is handed this value's address once per callback set, and it reads that
//! address again for **every** output it opens -- there is no callback that says
//! "no more outputs will come". The wrapper, on the other hand, can be freed while
//! libvlc still holds the player: a media list player retains the player it is
//! given, and the release that would stop it and join its threads is the one that
//! only runs on the last reference. An output opened after that reads an address
//! whose owner is gone.
//!
//! So the address is never freed. `close` is what `Drop` calls: it takes the ring
//! buffer out (five seconds of stereo audio is the one piece worth reclaiming) and
//! leaves the empty shell behind, which is the size of an `Arc` header and two
//! `None`s. A callback that arrives afterwards finds nothing to write to.
//!
//! The audio callbacks fill the ring buffer, but they do not touch the node that
//! plays it: that node is a child of the player, Godot frees the children before it
//! frees the extension instance, and `Drop` -- which is where the player learns that
//! it is going away -- runs after both. Asking the node whether it is still valid is
//! therefore a race that was measured, not a guard: the callback passed
//! `is_instance_valid` and then aborted inside `is_inside_tree`, with Godot's
//! teardown freeing the node in between. So the callbacks leave what they want done
//! here instead, and the frame applies it on the main thread, where the node is only
//! ever reached while the object that owns it is alive.

use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc,
    },
};

use godot::{
    classes::{Image, native::AudioFrame},
    prelude::*,
};
use ringbuf::HeapProd;

/// One audio output's destination: the ring buffer the callbacks fill, and what
/// `Drop` takes out of the sink when the player goes away.
pub(super) type AudioSlot = HeapProd<AudioFrame>;

/// The frames of one video output: `(is_resized, image)`.
pub(super) type VideoSender = mpsc::Sender<(bool, Gd<Image>)>;

/// `pause_request` values: nothing to do, pause the stream, resume it.
const PAUSE_NONE: u8 = 0;
const PAUSE_PAUSED: u8 = 1;
const PAUSE_RESUMED: u8 = 2;

pub(crate) struct OutputSink {
    closed: AtomicBool,
    accepting: AtomicBool,
    play_requested: AtomicBool,
    flush_requested: AtomicBool,
    pause_request: AtomicU8,
    video_tx: Mutex<Option<VideoSender>>,
    audio: Mutex<Option<AudioSlot>>,
}

// SAFETY: libvlc calls the video and audio callbacks from its own output threads,
// which is what the `Mutex`es and the atomics are for. The audio callbacks reach the
// node only through the requests below, which the main thread reads; the one Godot
// handle they hold is behind the same lock that `Drop` takes it out with. `Gd`
// requires the godot crate's experimental-threads feature, which is enabled in
// Cargo.toml -- the same footing `ImporterTask` stands on for the GPU path.
unsafe impl Send for OutputSink {}
unsafe impl Sync for OutputSink {}

impl OutputSink {
    pub(crate) fn new(video_tx: VideoSender, audio: AudioSlot) -> Arc<Self> {
        Arc::new(Self {
            closed: AtomicBool::new(false),
            accepting: AtomicBool::new(true),
            play_requested: AtomicBool::new(false),
            flush_requested: AtomicBool::new(false),
            pause_request: AtomicU8::new(PAUSE_NONE),
            video_tx: Mutex::new(Some(video_tx)),
            audio: Mutex::new(Some(audio)),
        })
    }

    /// The address libvlc is handed, with one reference leaked on purpose: this is
    /// the tombstone, and it stays readable for as long as libvlc can read it.
    pub(crate) fn leak(self: &Arc<Self>) -> *mut c_void {
        Arc::into_raw(Arc::clone(self)) as *mut c_void
    }

    /// The sink behind an address libvlc handed back.
    ///
    /// # Safety
    ///
    /// `data` has to be an address from [`OutputSink::leak`]. Those are never
    /// freed, so the reference is valid for the rest of the process.
    pub(crate) unsafe fn from_opaque<'a>(data: *mut c_void) -> &'a Self {
        unsafe { &*(data as *const Self) }
    }

    /// Whether the player this sink belongs to is gone.
    ///
    /// The callbacks read this before taking a lock, so a frame or a sample that
    /// arrives during teardown costs an atomic load rather than a lock.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Whether the player is somewhere its audio output can be played from.
    ///
    /// This replaces the callbacks' own `is_inside_tree`: it answers the same
    /// question, and it answers it without touching a node that Godot may be
    /// freeing on the other thread. The frame sets it when the player leaves the
    /// scene tree and when it comes back.
    pub(crate) fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }

    pub(crate) fn set_accepting(&self, accepting: bool) {
        self.accepting.store(accepting, Ordering::Release);
    }

    /// Asks for the node to be playing, from a thread that must not ask it itself.
    pub(crate) fn request_play(&self) {
        self.play_requested.store(true, Ordering::Release);
    }

    /// Takes the pending play request, if there is one.
    pub(crate) fn take_play(&self) -> bool {
        self.play_requested.swap(false, Ordering::Acquire)
    }

    /// Asks for the stream's buffered audio to be dropped.
    pub(crate) fn request_flush(&self) {
        self.flush_requested.store(true, Ordering::Release);
    }

    /// Takes the pending flush request, if there is one.
    pub(crate) fn take_flush(&self) -> bool {
        self.flush_requested.swap(false, Ordering::Acquire)
    }

    /// Records what libvlc wants the stream's paused state to be. The last request
    /// wins, which is what pause and resume mean when they arrive together.
    pub(crate) fn request_pause(&self, paused: bool) {
        let request = if paused { PAUSE_PAUSED } else { PAUSE_RESUMED };
        self.pause_request.store(request, Ordering::Release);
    }

    /// Takes the pending paused state, if it changed since the last frame.
    pub(crate) fn take_pause(&self) -> Option<bool> {
        match self.pause_request.swap(PAUSE_NONE, Ordering::Acquire) {
            PAUSE_PAUSED => Some(true),
            PAUSE_RESUMED => Some(false),
            _ => None,
        }
    }

    /// A sender for the frames of a video output, or `None` when there is nothing
    /// left to deliver to -- an output opened after the player was freed.
    pub(crate) fn video_sender(&self) -> Option<VideoSender> {
        if self.is_closed() {
            return None;
        }
        self.video_tx.lock().ok()?.clone()
    }

    pub(crate) fn audio(&self) -> &Mutex<Option<AudioSlot>> {
        &self.audio
    }

    /// Stops the callbacks and releases what they were writing into.
    ///
    /// The shell itself stays: libvlc may open another output and read it again.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.accepting.store(false, Ordering::Release);
        if let Ok(mut video_tx) = self.video_tx.lock() {
            *video_tx = None;
        }
        if let Ok(mut audio) = self.audio.lock() {
            *audio = None;
        }
    }
}
