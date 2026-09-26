//! libvlc's media list player: `libvlc_media_list_player_*`.
//!
//! This is libvlc's answer to "play A, then B, then C". It holds a list and a player,
//! and it advances by itself when an item ends -- because it listens for
//! `libvlc_MediaPlayerStopped` on the player it was given, on a thread of its own
//! (`vlc-playlist`). Three consequences shape this file:
//!
//! - **Stopping the player directly makes the list advance.** libvlc's list player
//!   cannot tell "the item ended" from "someone stopped the player", so a script that
//!   calls [method VLCMediaPlayer.stop_async] on a player a list player is driving gets
//!   the *next* item instead of a stop. Only the list player's own
//!   [method VlcMediaListPlayer.stop_async] takes the listener off first.
//! - **The list player owns a reference to the player and to the list.** Both therefore
//!   outlive the list player, not the other way round: a media player node that is freed
//!   while a list player still holds it would go on playing with this extension's
//!   callbacks -- the audio, video and event contexts in [super::VlcMediaPlayer] -- still
//!   pointing at the Rust object that was just destroyed. That is a use-after-free
//!   rather than a leak, so the association is ended deliberately: see
//!   [VlcMediaListPlayer::on_player_exiting].
//! - **It has no state of its own.** `is_playing` and `get_state` are the underlying
//!   player's, and the player can still be driven directly -- it is the same object.
//!
//! Two more things are worth knowing before reading the methods. `Played` is documented
//! as "playback has started" and is emitted when the list *runs out*; and `release` does
//! not stop playback, so freeing this node while it is playing leaves the player
//! playing.

use super::VlcMediaPlayer;
use crate::{
    vlc::*,
    vlc_event_attachments::EventAttachments,
    vlc_instance,
    vlc_media_list::{HeldMedia, VlcMediaList},
};
use godot::{
    classes::{INode, Node, notify::NodeNotification},
    prelude::*,
};
use std::{collections::VecDeque, ffi::c_void, sync::Mutex};

/// How many list-player events may wait for the main thread at once.
///
/// The queue is drained every frame, and these events are one per item transition, so a
/// full queue means something is wrong rather than that playback is busy.
const PARKED_LIST_PLAYER_CAPACITY: usize = 64;

/// One thing a list player event reported, on its way to the main thread.
enum ParkedListPlayerEvent {
    /// The list ran out. This is libvlc's own `MediaListPlayerPlayed`, whose
    /// documentation says playback started and whose implementation sends it at the end.
    Played,
    /// The next item is about to play, and this is it.
    NextItemSet(HeldMedia),
    /// The list player was stopped by its own [method VlcMediaListPlayer.stop_async].
    Stopped,
}

/// Where the list player's event callbacks leave what they received.
struct ListPlayerPark {
    queue: Mutex<VecDeque<ParkedListPlayerEvent>>,
}

impl ListPlayerPark {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(PARKED_LIST_PLAYER_CAPACITY)),
        }
    }

    /// Records one event. Called from libvlc's threads.
    fn push(&self, event: ParkedListPlayerEvent) {
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len() < PARKED_LIST_PLAYER_CAPACITY {
            queue.push_back(event);
        }
    }

    fn pop(&self) -> Option<ParkedListPlayerEvent> {
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.pop_front()
    }
}

unsafe extern "C" fn played_callback(_event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let Some(park) = (data as *const ListPlayerPark).as_ref() else {
            return;
        };
        park.push(ParkedListPlayerEvent::Played);
    }
}

unsafe extern "C" fn next_item_set_callback(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let Some(park) = (data as *const ListPlayerPark).as_ref() else {
            return;
        };
        let media = (*event).u.media_list_player_next_item_set.item;
        // Borrowed, and released the moment this callback returns.
        park.push(ParkedListPlayerEvent::NextItemSet(HeldMedia::retain(media)));
    }
}

unsafe extern "C" fn stopped_callback(_event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let Some(park) = (data as *const ListPlayerPark).as_ref() else {
            return;
        };
        park.push(ParkedListPlayerEvent::Stopped);
    }
}

/// Plays a list of media, one after another.
///
/// # What it drives
/// libvlc's list player plays through a media player, and this one is meant to use the
/// [VLCMediaPlayer] node in the scene: assign it to [member player] and the list's items
/// play with that node's video, audio and track handling. Leave it unassigned and the
/// list plays through a player of libvlc's own, which has no output here at all -- no
/// video, no audio, just the list advancing.
///
/// # Free this node before the player it drives
/// **Assigning [member player] makes libvlc's list player take a reference to that
/// player**, so the player outlives this node unless this node is released first. That
/// matters more than it sounds: freeing the player *node* while a list player still holds
/// it leaves a live libvlc player whose audio, video and event callbacks point into the
/// Rust object that was just dropped, and libvlc goes on calling them -- a use-after-free
/// inside libvlc's own threads. Those callbacks cannot be detached: libvlc requires the
/// first of each set (`libvlc_video_set_callbacks`'s lock, `libvlc_audio_set_callbacks`'s
/// play) and offers no way to unset them again.
///
/// So: free this node first, or free both together (which is what a scene change does,
/// since Godot frees children in reverse order and this node is normally added after the
/// player it drives). Freeing the player alone, while this node stays alive, is the one
/// order that is not safe.
///
/// # The player can still be driven directly, and should not be
/// Both this node and the script hold the same [VLCMediaPlayer], and **stopping that
/// player directly makes this list advance to the next item**: libvlc's list player
/// cannot tell an item that ended from a player that was stopped, and its listener is
/// what starts the next item. Stop the list through [method stop_async] instead.
///
/// # Playback modes
/// [method set_playback_mode] takes [constant PLAYBACK_MODE_DEFAULT] (play the list once
/// and stop), [constant PLAYBACK_MODE_LOOP] (start again at the first item) or
/// [constant PLAYBACK_MODE_REPEAT] (play the current item forever). `REPEAT` is measured
/// to ignore [method next] and [method previous]: they replay the current item instead of
/// moving.
#[derive(GodotClass)]
#[class(base=Node, rename=VLCMediaListPlayer)]
pub struct VlcMediaListPlayer {
    base: Base<Node>,
    /// libvlc's list player, or `None` once it has been released -- which happens in
    /// `Drop`, and when the player node it drives leaves the tree.
    ptr: Option<*mut libvlc_media_list_player_t>,
    /// The list to play.
    ///
    /// Not an exported property: `VLCMediaList` is a `RefCounted` and not a `Resource`,
    /// and Godot's object properties are for nodes and resources only -- the same limit
    /// GDScript has. It is reached through [method set_media_list] and
    /// [method get_media_list] instead.
    media_list: Option<Gd<VlcMediaList>>,
    /// Where the event callbacks leave what they received. Boxed because the events are
    /// attached with this address, and it is written from libvlc's threads.
    park: Box<ListPlayerPark>,
    /// The three list-player events, kept so that `Drop` can detach them
    /// (`vlc_event_attachments.rs`).
    ///
    /// Detaching is what keeps a callback from running against the park once
    /// this object is gone. libvlc's own release does join the list player's
    /// thread, so it would be a barrier too -- but only for a list player this
    /// object is the last reference to, and the detach is the one that does not
    /// depend on that.
    attachments: EventAttachments,
}

impl Drop for VlcMediaListPlayer {
    fn drop(&mut self) {
        if let Some(ptr) = self.ptr.take() {
            unsafe {
                self.attachments
                    .detach_all(libvlc_media_list_player_event_manager(ptr));
                libvlc_media_list_player_release(ptr);
            }
        }
    }
}

impl VlcMediaListPlayer {
    /// libvlc's list player, or `None` when it has been released.
    fn ptr(&self) -> Option<*mut libvlc_media_list_player_t> {
        self.ptr
    }

    /// The [VLCMediaPlayer] this node is a child of, if it is.
    ///
    /// The parent is the player, and that is the whole association: Godot frees children
    /// before their parent, so this node is always released first.
    fn parent_player(&self) -> Option<Gd<VlcMediaPlayer>> {
        self.base()
            .get_parent()
            .and_then(|parent| parent.try_cast::<VlcMediaPlayer>().ok())
    }

    /// Hands libvlc the player this node is a child of, so the list plays through it.
    ///
    /// A list player that is not a child of a [VLCMediaPlayer] says so and then plays
    /// through libvlc's own player, which has no output here: see the class documentation
    /// for why the parent is the player rather than something a script assigns.
    fn attach_to_parent_player(&mut self) {
        let Some(ptr) = self.ptr() else {
            return;
        };
        let Some(player) = self.parent_player() else {
            godot_error!(
                "godot-vlc: this VLCMediaListPlayer is not a child of a VLCMediaPlayer, so its list will play with no video or audio; make it a child of the player it should drive"
            );
            return;
        };
        unsafe {
            libvlc_media_list_player_set_media_player(ptr, player.bind().player_ptr());
        }
    }
}

#[godot_api]
impl VlcMediaListPlayer {
    /// Whether libvlc's list player is still there.
    ///
    /// # Returns
    /// `true` for as long as this node lives, whether or not a player has been assigned:
    /// libvlc builds one of its own, and a list can be played through it (with no output).
    /// It answers `false` during this node's own teardown, when the list player has
    /// already been released and the rest of the methods have become no-ops.
    #[func]
    fn is_playable(&self) -> bool {
        self.ptr.is_some()
    }

    /// The player this list plays through: the [VLCMediaPlayer] this node is a child of.
    ///
    /// # Returns
    /// that node, or null when this one is not a child of a `VLCMediaPlayer` -- in which
    /// case libvlc's own player is the one playing, and it has no output here.
    ///
    /// # Note
    /// There is nothing to assign: the parent *is* the player, and that is deliberate.
    /// Godot frees a node's children before the node itself, so a list player that is a
    /// child of the player it drives is always released first -- which is the order that
    /// keeps libvlc from outliving this extension's output callbacks. See the class
    /// documentation.
    #[func]
    fn get_player(&self) -> Option<Gd<VlcMediaPlayer>> {
        self.parent_player()
    }

    /// Sets the list to play.
    ///
    /// # Parameters
    /// - [param media_list] the list of media, or null to leave the current one.
    ///
    /// # Note
    /// The list player keeps a reference of its own, so the list stays alive while it is
    /// set here even if the script lets go of it.
    #[func]
    fn set_media_list(&mut self, media_list: Option<Gd<VlcMediaList>>) {
        let Some(ptr) = self.ptr() else {
            return;
        };
        let raw = media_list.as_ref().map(|list| list.bind().media_list_ptr());
        unsafe {
            libvlc_media_list_player_set_media_list(ptr, raw.unwrap_or(std::ptr::null_mut()))
        };
        self.media_list = media_list;
    }

    /// The list being played, if one was set.
    #[func]
    fn get_media_list(&self) -> Option<Gd<VlcMediaList>> {
        self.media_list.clone()
    }

    /// Starts playing the list.
    ///
    /// # Note
    /// - It plays from the current position: the item that was set last, or the first one
    ///   if nothing has played yet.
    /// - With no list set it does nothing at all, and there is no return value to say so
    ///   -- libvlc's own call is `void`, and it writes a line to its log. This binding
    ///   writes an error instead, because a script that forgot [method set_media_list]
    ///   would otherwise see silence.
    #[func]
    fn play(&mut self) {
        let Some(ptr) = self.ptr() else {
            return;
        };
        if self.media_list.is_none() {
            godot_error!(
                "godot-vlc: play() was called with no media list set; libvlc's list player has nothing to play"
            );
            return;
        }
        unsafe { libvlc_media_list_player_play(ptr) };
    }

    /// Plays the item at an index of the list.
    ///
    /// # Parameters
    /// - [param index] the position in the list.
    ///
    /// # Returns
    /// `0`, or `-1` when the index is outside the list.
    ///
    /// # Note
    /// This one addresses the list's own items only: unlike [method next], it does not
    /// descend into a subitem of the entry at that index.
    #[func]
    fn play_item_at_index(&mut self, index: i32) -> i32 {
        match self.ptr() {
            Some(ptr) => unsafe { libvlc_media_list_player_play_item_at_index(ptr, index) },
            None => -1,
        }
    }

    /// Plays a media that is in the list.
    ///
    /// # Parameters
    /// - [param media] the media to play.
    ///
    /// # Returns
    /// `0`, or `-1` when the media is not in the list. libvlc compares the descriptors
    /// themselves, so a copy of a media in the list is not found.
    ///
    /// # Note
    /// This is the one entry point of the three that does **not** emit
    /// [signal next_item_set] -- libvlc's own inconsistency, kept as it is.
    #[func]
    fn play_item(&mut self, media: Gd<crate::vlc_media::VlcMedia>) -> i32 {
        match self.ptr() {
            Some(ptr) => unsafe { libvlc_media_list_player_play_item(ptr, media.bind().media_ptr) },
            None => -1,
        }
    }

    /// Pauses, or resumes, the playback the list is running.
    #[func]
    fn pause(&mut self) {
        if let Some(ptr) = self.ptr() {
            unsafe { libvlc_media_list_player_pause(ptr) };
        }
    }

    /// Asks for a pause (`true`) or for playback to carry on (`false`).
    ///
    /// # Parameters
    /// - [param do_pause] whether to pause.
    ///
    /// # Note
    /// This is the one that says which of the two it wants; [method pause] toggles, and
    /// libvlc's own documentation does not say what it toggles from.
    #[func]
    fn set_pause(&mut self, do_pause: bool) {
        if let Some(ptr) = self.ptr() {
            unsafe { libvlc_media_list_player_set_pause(ptr, do_pause as i32) };
        }
    }

    /// Whether the list is playing.
    ///
    /// # Returns
    /// the underlying player's answer: `true` while it is opening or playing. It says
    /// nothing about how much of the list is left, and it is `false` between two items.
    #[func]
    fn is_playing(&self) -> bool {
        match self.ptr() {
            Some(ptr) => unsafe { libvlc_media_list_player_is_playing(ptr) },
            None => false,
        }
    }

    /// The state of the playback the list is running.
    ///
    /// # Returns
    /// one of the `STATE_*` constants, as [method VLCMediaPlayer.get_state] answers
    /// them: it is that player's state, because the list player has none of its own.
    #[func]
    fn get_state(&self) -> i32 {
        match self.ptr() {
            Some(ptr) => unsafe { libvlc_media_list_player_get_state(ptr) as i32 },
            None => super::VlcMediaPlayer::STATE_NOTHING_SPECIAL,
        }
    }

    /// Moves to the next item.
    ///
    /// # Returns
    /// `0`, or `-1` when there is no next item or no list player.
    ///
    /// # Note
    /// Under [constant PLAYBACK_MODE_REPEAT] this replays the current item instead of
    /// moving: measured, libvlc's repeat mode does not advance at all.
    #[func]
    fn next(&mut self) -> i32 {
        match self.ptr() {
            Some(ptr) => unsafe { libvlc_media_list_player_next(ptr) },
            None => -1,
        }
    }

    /// Moves to the previous item.
    ///
    /// # Returns
    /// `0`, or `-1` when there is no previous item or no list player.
    ///
    /// # Note
    /// At the first item this wraps to the end of the list, as the mode allows, and under
    /// [constant PLAYBACK_MODE_REPEAT] it replays the current item instead.
    #[func]
    fn previous(&mut self) -> i32 {
        match self.ptr() {
            Some(ptr) => unsafe { libvlc_media_list_player_previous(ptr) },
            None => -1,
        }
    }

    /// Stops the list.
    ///
    /// # Note
    /// - This is the stop to use: it takes the list player's own listener off the
    ///   underlying player first, so the stop is not mistaken for an item that ended and
    ///   followed by the next one.
    /// - It is asynchronous, exactly as [method VLCMediaPlayer.stop_async] is, and it
    ///   emits [signal stopped] when it has been asked for -- not when playback has
    ///   finished stopping.
    #[func]
    fn stop_async(&mut self) {
        if let Some(ptr) = self.ptr() {
            unsafe { libvlc_media_list_player_stop_async(ptr) };
        }
    }

    /// Sets what happens when the list runs out.
    ///
    /// # Parameters
    /// - [param mode] [constant PLAYBACK_MODE_DEFAULT] or [constant PLAYBACK_MODE_LOOP].
    ///   [constant PLAYBACK_MODE_REPEAT] is refused -- see below.
    ///
    /// # `REPEAT` is refused, because libvlc aborts on it
    /// Measured, twice: selecting it and letting its one code path run trips an
    /// assertion inside libvlc and takes the process down with it. Its implementation
    /// gives the mode a single behaviour -- replay the item it is already on, by setting
    /// that same media on the player again (`lib/media_list_player.c:777-790`) -- and
    /// libvlc's player refuses a state transition that repeats the state it is already
    /// in (`src/player/input.c:326`). The shipped runtime is built with assertions on,
    /// so this is an abort rather than a warning.
    ///
    /// The value is refused here the same way [method VLCMediaPlayer.set_spu_text_scale]
    /// refuses an out-of-range scale: the call writes an error and changes nothing. Use
    /// [constant PLAYBACK_MODE_LOOP] to repeat a list.
    #[func]
    fn set_playback_mode(&mut self, mode: i32) {
        let Some(ptr) = self.ptr() else {
            return;
        };
        if mode == Self::PLAYBACK_MODE_REPEAT {
            godot_error!(
                "godot-vlc: set_playback_mode(PLAYBACK_MODE_REPEAT) was refused: libvlc's repeat mode replays its item by setting the same media on the player again, which trips an assertion inside libvlc (src/player/input.c:326) and aborts a build with assertions on -- which this runtime is. Use PLAYBACK_MODE_LOOP instead."
            );
            return;
        }
        unsafe { libvlc_media_list_player_set_playback_mode(ptr, mode as libvlc_playback_mode_t) };
    }

    /// Plays the list once and stops at the end. The default.
    ///
    /// The casts are for the targets where bindgen types this enum as `c_int` rather
    /// than `u32`, as the `STATE_*` constants are.
    #[allow(clippy::unnecessary_cast)]
    #[constant]
    const PLAYBACK_MODE_DEFAULT: i32 = libvlc_playback_mode_t_libvlc_playback_mode_default as i32;
    /// Starts again at the first item when the list runs out.
    #[allow(clippy::unnecessary_cast)]
    #[constant]
    const PLAYBACK_MODE_LOOP: i32 = libvlc_playback_mode_t_libvlc_playback_mode_loop as i32;
    /// Plays the current item forever, without ever moving to another.
    ///
    /// # Note
    /// **This value is refused by [method set_playback_mode].** Measured, it aborts the
    /// process in this runtime: see that method, and use
    /// [constant PLAYBACK_MODE_LOOP] instead. It is exported so that a caller reading
    /// libvlc's own documentation can see what it is and what happened to it.
    #[allow(clippy::unnecessary_cast)]
    #[constant]
    const PLAYBACK_MODE_REPEAT: i32 = libvlc_playback_mode_t_libvlc_playback_mode_repeat as i32;

    /// Emitted when the list has run out and playback stopped.
    ///
    /// # Note
    /// - It carries nothing, and it has to: libvlc sends this one with a payload member it
    ///   never writes.
    /// - **libvlc's own documentation for it is wrong.** It says "playback ... has
    ///   started"; the implementation sends it where the list runs out, which is also the
    ///   only place it is sent at all. This binding follows the implementation, as it
    ///   does everywhere.
    /// - There is no "the item ended" event: VLC 4 removed the one that used to exist, so
    ///   an item that ended and a player that was stopped look the same from outside. A
    ///   list player is what makes the difference unnecessary.
    #[signal]
    fn played();
    /// Emitted with the item that is about to play.
    ///
    /// # Parameters
    /// - [param media] the item the list moved to.
    ///
    /// # Note
    /// - [method play] emits it for the first item, and so do [method next] and
    ///   [method previous]; [method play_item] does not, and neither does
    ///   [method play_item_at_index].
    /// - The media is held by this binding until the signal has gone out, although
    ///   libvlc's own event borrows it.
    #[signal]
    fn next_item_set(media: Gd<crate::vlc_media::VlcMedia>);
    /// Emitted when [method stop_async] was asked for.
    ///
    /// # Note
    /// - It carries nothing: libvlc sends it with a payload member it never writes.
    /// - Freeing the list player does **not** emit it, and does not stop playback
    ///   either: libvlc's own release leaves the player running, which is worth knowing
    ///   before freeing this node in the middle of a scene change.
    #[signal]
    fn stopped();
}

impl VlcMediaListPlayer {
    /// Emits the signals for everything the list player's callbacks have recorded.
    fn emit_parked_events(&mut self) {
        while let Some(event) = self.park.pop() {
            match event {
                ParkedListPlayerEvent::Played => self.signals().played().emit(),
                ParkedListPlayerEvent::NextItemSet(media) => {
                    if let Some(media) = crate::vlc_media::VlcMedia::from_ptr(media.into_ptr()) {
                        self.signals().next_item_set().emit(&media);
                    }
                }
                ParkedListPlayerEvent::Stopped => self.signals().stopped().emit(),
            }
        }
    }
}

#[godot_api]
impl INode for VlcMediaListPlayer {
    fn init(base: Base<Node>) -> Self {
        let instance = vlc_instance::get();
        let ptr = unsafe { libvlc_media_list_player_new(instance) };
        assert!(
            !ptr.is_null(),
            "libvlc could not create a media list player: {}",
            crate::vlc_instance::last_error()
        );
        let mut player = Self {
            base,
            ptr: Some(ptr),
            media_list: None,
            park: Box::new(ListPlayerPark::new()),
            attachments: EventAttachments::default(),
        };

        // The three events, attached with the park's address.
        unsafe {
            let manager = libvlc_media_list_player_event_manager(ptr);
            let data = player.park.as_ref() as *const ListPlayerPark as *mut c_void;
            for (event_type, callback) in [
                (
                    libvlc_event_e_libvlc_MediaListPlayerPlayed,
                    played_callback as unsafe extern "C" fn(_, _),
                ),
                (
                    libvlc_event_e_libvlc_MediaListPlayerNextItemSet,
                    next_item_set_callback,
                ),
                (
                    libvlc_event_e_libvlc_MediaListPlayerStopped,
                    stopped_callback,
                ),
            ] {
                player.attachments.attach(
                    manager,
                    event_type as libvlc_event_type_t,
                    Some(callback),
                    data,
                );
            }
        }
        player
    }

    /// The frame hook and the parent lookup, set up from a notification rather than from
    /// the `ready` virtual: a script attached to this node may define `_ready` itself, and
    /// both of these have to happen whether it does or not. `VlcMediaPlayer` is built the
    /// same way, for the same reason.
    fn on_notification(&mut self, what: NodeNotification) {
        match what {
            // Both, because a list player can be moved into a player after it was already
            // in the tree somewhere else.
            NodeNotification::READY | NodeNotification::ENTER_TREE => {
                self.base_mut().set_process_internal(true);
                self.attach_to_parent_player();
            }
            NodeNotification::INTERNAL_PROCESS => self.emit_parked_events(),
            _ => {}
        }
    }

    /// Tells the editor when this node is not where it belongs.
    ///
    /// A `VLCMediaListPlayer` plays through the `VLCMediaPlayer` it is a child of, so a
    /// node in the wrong place is a scene that will not play anything -- and the editor
    /// says so, in the scene tree's own warning, rather than leaving it to be discovered
    /// at runtime.
    fn get_configuration_warnings(&self) -> PackedStringArray {
        let mut warnings = PackedStringArray::new();
        if self.parent_player().is_none() {
            warnings.push(
                "This VLCMediaListPlayer is not a child of a VLCMediaPlayer. It plays through the player it is a child of, so its list would play with no video or audio: make it a child of the VLCMediaPlayer it should drive.",
            );
        }
        warnings
    }
}
