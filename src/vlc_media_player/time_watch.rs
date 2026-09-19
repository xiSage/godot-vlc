//! The player's playback-time watcher: `libvlc_media_player_watch_time`.
//!
//! libvlc calls the three callbacks below on whatever thread moved the clock -- the
//! video output thread after a display, the audio output thread after a write, or the
//! input thread -- and, measured, it holds the player's **timer** lock while it does
//! (`src/player/timer.c` locks it around the fan-out and unlocks after). The player's
//! own lock is reentrant, that one is not, so a callback here that called
//! `libvlc_media_player_unwatch_time` would deadlock, and the header forbids calling
//! any media player function from these callbacks outright. They therefore record, and
//! the main thread reports.

use crate::vlc::*;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

/// One time point, as libvlc handed it over.
///
/// Copied out of the callback's argument, which is a stack copy of libvlc's own
/// (`lib/media_player.c` builds a local and passes its address) and is therefore only
/// valid for the duration of that call -- libvlc's header says `always valid` without
/// saying for how long, and the C source says one call.
pub(crate) type TimePoint = libvlc_media_player_time_point_t;

/// How many watched events may wait for the main thread at once.
///
/// Time points do not count against this: they coalesce (see [WatchSink::push]), so a
/// full queue means something other than the clock is repeating itself.
const PARKED_WATCH_CAPACITY: usize = 64;

/// One thing the watcher was told, on its way to the main thread.
#[derive(Clone, Debug)]
pub(crate) enum ParkedWatch {
    /// An update: the point libvlc had when the output displayed or wrote something.
    Point(TimePoint),
    /// The player was paused, or is stopping. The date is libvlc's system date of the
    /// event, and is **0 when the player is stopping** rather than pausing: libvlc
    /// sends one callback for both and its date is only valid for a pause.
    Paused(i64),
    /// A seek was asked for, at this point.
    Seek(TimePoint),
    /// That seek is over. libvlc's own signal for this is the seek callback with a
    /// null point.
    SeekFinished,
}

/// Where the watcher's callbacks leave what they received.
///
/// One allocation holds the latest point and the queue, because one address is handed
/// to libvlc and has to stay put for as long as the watcher is registered -- the same
/// reason [crate::vlc_instance]'s log sink is one allocation, and why the player keeps
/// this in a `Box`.
pub(crate) struct WatchSink {
    state: Mutex<WatchState>,
    /// Whether libvlc is watching.
    ///
    /// Tracked here because `libvlc_media_player_unwatch_time` is not a call that can
    /// be made freely: it asserts that a watcher exists, and that assert is compiled
    /// out of a release build, after which it removes a node from a list through a
    /// null pointer. Calling it without a watcher is a crash inside libvlc rather than
    /// an error it reports, so nothing may call it unless this is true.
    watching: AtomicBool,
}

struct WatchState {
    /// The point from the most recent update, for [crate::vlc_media_player]'s
    /// interpolation. Kept apart from the queue because interpolation needs the value
    /// that is current *now*, not the oldest one that has not been reported yet.
    latest: Option<TimePoint>,
    queue: VecDeque<ParkedWatch>,
}

impl WatchSink {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(WatchState {
                latest: None,
                queue: VecDeque::with_capacity(PARKED_WATCH_CAPACITY),
            }),
            watching: AtomicBool::new(false),
        }
    }

    pub(crate) fn watching(&self) -> bool {
        self.watching.load(Ordering::Relaxed)
    }

    /// Registers the watcher, unless this player already has one.
    ///
    /// libvlc allows one watcher per player and answers `-1` for a second one, with its
    /// own error message; this answers the same without asking, so that a caller that
    /// lost track of its own state does not also get libvlc's complaint. Any other
    /// failure is libvlc's and is passed on as it came.
    pub(crate) fn register(&self, player: *mut libvlc_media_player_t, min_period_us: i64) -> i32 {
        if self.watching() {
            return -1;
        }
        let data = self as *const Self as *mut c_void;
        let status = unsafe {
            libvlc_media_player_watch_time(
                player,
                min_period_us,
                Some(on_update),
                Some(on_paused),
                Some(on_seek),
                data,
            )
        };
        if status == 0 {
            self.watching.store(true, Ordering::Relaxed);
        }
        status
    }

    /// Unregisters the watcher, if there is one.
    ///
    /// Returns whether one was registered. Nothing else may call
    /// `libvlc_media_player_unwatch_time`, for the reason on [Self::watching].
    pub(crate) fn unregister(&self, player: *mut libvlc_media_player_t) -> bool {
        if !self.watching.swap(false, Ordering::Relaxed) {
            return false;
        }
        unsafe { libvlc_media_player_unwatch_time(player) };
        true
    }

    /// The point from the most recent update, if there has been one.
    pub(crate) fn latest(&self) -> Option<TimePoint> {
        self.lock().latest
    }

    /// Records one thing the watcher reported. Called from libvlc's threads.
    fn push(&self, watch: ParkedWatch) {
        let mut state = self.lock();
        let point = match &watch {
            ParkedWatch::Point(point) => Some(*point),
            _ => None,
        };
        if let Some(point) = point {
            state.latest = Some(point);
            // A newer point makes the one before it worthless -- nothing can
            // interpolate from a point that has already been superseded -- so it takes
            // the place of a queued point rather than queueing behind it. That is also
            // what keeps a producer running at the output's own rate (measured: about
            // twenty a second for a ten-frame-a-second media) from filling the queue
            // between two frames and making libvlc's own events wait behind stale
            // clock readings.
            if let Some(ParkedWatch::Point(previous)) = state.queue.back_mut() {
                *previous = point;
                return;
            }
            if state.queue.len() < PARKED_WATCH_CAPACITY {
                state.queue.push_back(ParkedWatch::Point(point));
            }
            return;
        }
        if state.queue.len() < PARKED_WATCH_CAPACITY {
            state.queue.push_back(watch);
        }
    }

    /// Takes the oldest event, if there is one. Called from the main thread.
    pub(crate) fn pop(&self) -> Option<ParkedWatch> {
        self.lock().queue.pop_front()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        // A poisoned lock means another thread panicked while holding it. The queue is
        // still usable, and losing a clock reading is a better outcome than
        // propagating a panic into libvlc's thread.
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Records one time point. Called from libvlc's threads, holding its timer lock.
unsafe extern "C" fn on_update(value: *const TimePoint, data: *mut c_void) {
    unsafe {
        let Some(sink) = (data as *const WatchSink).as_ref() else {
            return;
        };
        if let Some(point) = value.as_ref() {
            sink.push(ParkedWatch::Point(*point));
        }
    }
}

/// Records a pause, or a stop: libvlc sends one callback for both, and the date it
/// carries is valid only for the pause (a stop sends `0`).
unsafe extern "C" fn on_paused(system_date_us: i64, data: *mut c_void) {
    unsafe {
        let Some(sink) = (data as *const WatchSink).as_ref() else {
            return;
        };
        sink.push(ParkedWatch::Paused(system_date_us));
    }
}

/// Records a seek: libvlc calls this twice for one seek, first with the point that was
/// asked for and then with a null one when the seek is over.
unsafe extern "C" fn on_seek(value: *const TimePoint, data: *mut c_void) {
    unsafe {
        let Some(sink) = (data as *const WatchSink).as_ref() else {
            return;
        };
        match value.as_ref() {
            Some(point) => sink.push(ParkedWatch::Seek(*point)),
            None => sink.push(ParkedWatch::SeekFinished),
        }
    }
}
