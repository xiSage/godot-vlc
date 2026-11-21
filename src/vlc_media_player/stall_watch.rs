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

//! Freezing the media when the node stops running.
//!
//! libvlc decodes on threads of its own, so a media keeps advancing behind a node that
//! is not running. Three ways of stopping this node are known to it: a paused tree and
//! a suspended tree both send a notification, and leaving the tree sends one too. A
//! debugger break does not: the whole main loop stops inside the debugger, so no frame
//! of this node runs and no notification arrives, while libvlc keeps decoding. The same
//! holds for any other stall long enough to be seen -- a scene load, a window resize, a
//! shader compile.
//!
//! Nothing on the main thread can act then, so this watches the node's heartbeat from a
//! thread it starts itself and pauses a playing media once the heartbeat has not moved
//! for [`STALL`]. The node resumes the media on the first frame it runs again, and only
//! a media this watch paused is resumed: one the caller paused, or one that stopped, is
//! not the watch's to start. A node that stops processing for its own reasons stops the
//! media with it, which is the rule playback follows -- it plays while the node does.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::vlc::{
    libvlc_media_player_is_playing, libvlc_media_player_set_pause, libvlc_media_player_t,
};

use super::output_sink::OutputSink;

/// How long the node may go without a frame before the media is frozen.
///
/// Long enough that a frame that took its time does not stop the sound -- a game
/// running at ten frames a second still leaves two frames of slack -- and short enough
/// that a break skips little of what it froze: the editor's Pause button leaves the
/// media running for this long before the watch can act.
const STALL: Duration = Duration::from_millis(250);

/// How often the heartbeat is looked at while nothing is happening.
const POLL: Duration = Duration::from_millis(50);

/// A handle to a libvlc player that may be used from another thread.
///
/// The player outlives every thread that holds one of these: [`StallWatch::stop`] joins
/// the watch before the player is released.
struct Player(*mut libvlc_media_player_t);

// SAFETY: the pointer is only used for libvlc calls, which are thread-safe, and the
// thread holding it is joined before the player is released.
unsafe impl Send for Player {}

/// Watches the node's heartbeat and pauses a playing media while it is missing.
pub(crate) struct StallWatch {
    /// Bumped by the node on every frame it processes.
    beat: Arc<AtomicU64>,
    /// Set by the watch when it paused the media, cleared by the node when it resumes.
    paused: Arc<AtomicBool>,
    /// Set when the watch has to stop.
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StallWatch {
    /// Starts watching `player`, which must not be released before this is dropped.
    pub(crate) fn start(player: *mut libvlc_media_player_t) -> Self {
        let beat = Arc::new(AtomicU64::new(0));
        let paused = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("godot-vlc stall watch".to_owned())
            .spawn({
                let beat = Arc::clone(&beat);
                let paused = Arc::clone(&paused);
                let stopping = Arc::clone(&stopping);
                let player = Player(player);
                move || watch(beat, paused, stopping, player)
            })
            .ok();
        Self {
            beat,
            paused,
            stopping,
            thread,
        }
    }

    /// Reports a frame the node processed, and resumes a media the watch paused.
    ///
    /// The heartbeat is bumped first: the watch must see the node running again before
    /// it can decide to pause a media this frame is about to resume.
    pub(crate) fn frame(&self, player: *mut libvlc_media_player_t, sink: &OutputSink) {
        self.beat.fetch_add(1, Ordering::Relaxed);
        if self.paused.swap(false, Ordering::AcqRel) {
            // SAFETY: the pointer is the live player, and this is the main thread.
            unsafe { libvlc_media_player_set_pause(player, 0) };
            // libvlc reports the resume on its own thread, and the pause it reported
            // before may still be on its way: the stream is resumed here rather than
            // one frame after the media already is.
            sink.request_pause(false);
        }
    }

    /// Stops the watch and waits for its thread, if it started one.
    pub(crate) fn stop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // The thread only ever sleeps for a poll interval, so this is short.
            let _ = thread.join();
        }
    }
}

impl Drop for StallWatch {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Pauses a playing media once `beat` has not moved for [`STALL`], until `stopping`.
fn watch(beat: Arc<AtomicU64>, paused: Arc<AtomicBool>, stopping: Arc<AtomicBool>, player: Player) {
    let mut seen = beat.load(Ordering::Relaxed);
    let mut still_since = Instant::now();
    while !stopping.load(Ordering::Acquire) {
        thread::sleep(POLL);
        let now = beat.load(Ordering::Relaxed);
        if now != seen {
            seen = now;
            still_since = Instant::now();
            continue;
        }
        if still_since.elapsed() < STALL || paused.load(Ordering::Acquire) {
            continue;
        }
        // Only a media that is playing is worth freezing: one the caller paused is
        // already still, and one that stopped has nothing to keep from advancing.
        // SAFETY: the player is alive until this thread is joined.
        if !unsafe { libvlc_media_player_is_playing(player.0) } {
            continue;
        }
        // SAFETY: same. libvlc takes calls from any thread; this is not a callback.
        unsafe { libvlc_media_player_set_pause(player.0, 1) };
        paused.store(true, Ordering::Release);
    }
}
