//! libvlc's media lists: `libvlc_media_list_*`.
//!
//! A list is a queue of media, with two uses here: a script builds one and hands it to
//! something that plays through it, and a media hands out the list of what it holds --
//! the entries of a playlist file, a disc or a directory -- through
//! [crate::vlc_media::VlcMedia::get_subitems].
//!
//! Two properties of libvlc's lists shape everything in this file:
//!
//! - **Every read and write of the array must hold the list's own lock.** libvlc's
//!   header says "the libvlc_media_list_lock should be held upon entering this
//!   function" for `count`, `item_at_index`, `index_of_item` and the three writing
//!   calls, and its implementation does not check: those functions read and grow a
//!   plain array with no lock of their own, while the parsing thread appends to the
//!   same array from behind that lock. Not holding it is undefined behaviour that
//!   nothing reports, so every entry point here takes it and gives it back.
//! - **The lock is not recursive, and a callback already holds it.** libvlc sends a
//!   list's events from inside the modification that caused them, with the lock held,
//!   so a callback that called any `libvlc_media_list_*` function would deadlock. The
//!   callbacks here therefore only record, and the signals go out from the main thread
//!   (see [ListBridge]).
//!
//! `libvlc_media_list_new` in this libvlc takes no instance argument -- 3.0's did --
//! and a list created that way is writable, while a media's own subitems list is
//! read-only: its writing calls fail with a message in libvlc's log, which is what
//! [VlcMediaList::is_read_only] is for.
//!
//! `libvlc_media_list_release` releases every media the list holds, so this object's
//! `Drop` is what frees the elements as well as the list.

use crate::{vlc::*, vlc_media::VlcMedia};
use godot::{classes::WeakRef, global::weakref, prelude::*};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

/// How many list events may wait for the main thread at once.
///
/// The queue is drained by a deferred call, so it only fills if something is adding or
/// removing far faster than the main thread runs -- a playlist being parsed is the
/// realistic producer, and it produces one entry at a time.
const PARKED_LIST_CAPACITY: usize = 64;

/// Holds one reference to a media for as long as a record does.
///
/// The events below carry a **borrowed** media: for an item that is being deleted, the
/// list's own reference is gone by the time the callback returns. Retaining it in the
/// callback is what makes the record valid at all, and this is what gives that
/// reference back -- either to a [VlcMedia] wrapper that takes it over, or, if the
/// record is dropped before it is drained, to libvlc.
struct HeldMedia(*mut libvlc_media_t);

impl HeldMedia {
    /// Takes the reference out, for a wrapper that will own it.
    fn into_ptr(self) -> *mut libvlc_media_t {
        let ptr = self.0;
        std::mem::forget(self);
        ptr
    }
}

impl Drop for HeldMedia {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libvlc_media_release(self.0) };
        }
    }
}

/// One thing a list event reported, on its way to the main thread.
enum ParkedListEvent {
    /// An item is about to be added, at this index.
    WillAdd(HeldMedia, i32),
    Added(HeldMedia, i32),
    WillDelete(HeldMedia, i32),
    Deleted(HeldMedia, i32),
    /// The parse that fills a media's subitems is over. libvlc's own event for this
    /// carries nothing: its payload member is never written, so there is nothing to
    /// read and nothing is passed on.
    EndReached,
}

/// Where a list's event callbacks leave what they received.
///
/// The events are attached with this address, which is why [VlcMediaList] keeps it in
/// a `Box`. It also holds the list's own weak reference, because the signals cannot be
/// emitted from the callback: a signal carrying a [VlcMedia] would have to build that
/// Godot object off the main thread. The callback therefore asks for a deferred drain
/// instead -- once, however many events pile up -- and
/// [VlcMediaList::drain_parked_events] emits them on the main thread.
struct ListBridge {
    queue: Mutex<VecDeque<ParkedListEvent>>,
    /// Whether a drain is already queued. One is enough for any number of events;
    /// asking for one per event would queue a Godot call per item from libvlc's
    /// thread, which is what the queue exists to avoid.
    drain_queued: AtomicBool,
    /// The list, weakly: the object owns this bridge, so a strong reference here would
    /// be a cycle that never drops.
    self_gd: Mutex<Option<Gd<WeakRef>>>,
}

impl ListBridge {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(PARKED_LIST_CAPACITY)),
            drain_queued: AtomicBool::new(false),
            self_gd: Mutex::new(None),
        }
    }

    /// Records one event. Called from libvlc's threads.
    fn push(&self, event: ParkedListEvent) {
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len() < PARKED_LIST_CAPACITY {
            queue.push_back(event);
        }
    }

    fn pop(&self) -> Option<ParkedListEvent> {
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.pop_front()
    }

    /// Asks for a deferred drain, unless one is already on its way.
    ///
    /// `call_deferred` is the crate's established way for an object that has no frame
    /// of its own to reach the main thread: a `VLCMediaList` is a `RefCounted` a script
    /// builds, not a node, so there is no `on_notification` to drain it from.
    fn queue_drain(&self) {
        if self.drain_queued.swap(true, Ordering::Relaxed) {
            return;
        }
        let list = self.self_gd.lock().ok().and_then(|self_gd| {
            let weak = self_gd.as_ref()?;
            let object = weak.get_ref();
            // A freed object answers with nothing, which is the state the events are
            // detached in.
            if object.is_nil() {
                None
            } else {
                Some(object.to::<Gd<VlcMediaList>>())
            }
        });
        match list {
            Some(mut list) => {
                list.call_deferred("drain_parked_events", &[]);
            }
            None => self.drain_queued.store(false, Ordering::Relaxed),
        }
    }

    fn mark_drained(&self) {
        self.drain_queued.store(false, Ordering::Relaxed);
    }
}

/// Holds a list's lock for as long as it lives.
///
/// libvlc requires it for the calls that read or write the array, and it is not
/// recursive, so holding it across another such call is a deadlock rather than a
/// mistake that reports itself. Keeping it in a guard means the lock is given back
/// even if one of those calls panics.
struct ListLock(*mut libvlc_media_list_t);

impl ListLock {
    fn new(list: *mut libvlc_media_list_t) -> Self {
        unsafe { libvlc_media_list_lock(list) };
        Self(list)
    }
}

impl Drop for ListLock {
    fn drop(&mut self) {
        unsafe { libvlc_media_list_unlock(self.0) };
    }
}

/// A list of media: a queue a script builds, or what a media holds.
///
/// # Locking is handled for you
/// libvlc requires its own lock to be held around every read and write of a list's
/// array, and does not take it itself. Every method here takes it, so a script never
/// has to -- and must not, in a signal handler: libvlc sends those from inside the
/// modification that caused them, with the lock already held, and it is not recursive.
///
/// # What a list holds, and for how long
/// A list holds a reference to every media in it, and releases them all when the list
/// is released -- which happens when this object is freed. Adding a media therefore
/// keeps it alive for as long as the list does, and taking one out
/// ([method item_at_index]) hands back a reference of its own.
#[derive(GodotClass)]
#[class(base=RefCounted, rename=VLCMediaList)]
pub struct VlcMediaList {
    base: Base<RefCounted>,
    /// The list itself. This object owns one reference to it, which `Drop` gives back.
    ptr: *mut libvlc_media_list_t,
    /// Where the event callbacks leave what they received. Boxed because the events are
    /// attached with this address, and it is written from libvlc's threads.
    events: Box<ListBridge>,
}

impl Drop for VlcMediaList {
    fn drop(&mut self) {
        unsafe {
            // Detached before the list goes: the callbacks are handed this object's
            // weak reference, and a list that outlived its wrapper would call into
            // freed memory. (`VlcMedia` does not do this for its own event, which is a
            // known gap recorded in the analysis; there is no reason to repeat it.)
            let event_manager = libvlc_media_list_event_manager(self.ptr);
            let data = self.events.as_ref() as *const ListBridge as *mut c_void;
            libvlc_event_detach(
                event_manager,
                libvlc_event_e_libvlc_MediaListWillAddItem as libvlc_event_type_t,
                Some(will_add_callback),
                data,
            );
            libvlc_event_detach(
                event_manager,
                libvlc_event_e_libvlc_MediaListItemAdded as libvlc_event_type_t,
                Some(added_callback),
                data,
            );
            libvlc_event_detach(
                event_manager,
                libvlc_event_e_libvlc_MediaListWillDeleteItem as libvlc_event_type_t,
                Some(will_delete_callback),
                data,
            );
            libvlc_event_detach(
                event_manager,
                libvlc_event_e_libvlc_MediaListItemDeleted as libvlc_event_type_t,
                Some(deleted_callback),
                data,
            );
            libvlc_event_detach(
                event_manager,
                libvlc_event_e_libvlc_MediaListEndReached as libvlc_event_type_t,
                Some(end_reached_callback),
                data,
            );
            libvlc_media_list_release(self.ptr);
        }
    }
}

#[godot_api]
impl IRefCounted for VlcMediaList {
    fn init(base: Base<RefCounted>) -> Self {
        let ptr = unsafe { libvlc_media_list_new() };
        assert!(
            !ptr.is_null(),
            "libvlc could not create a media list: {}",
            crate::vlc_instance::last_error()
        );
        let list = Self {
            base,
            ptr,
            events: Box::new(ListBridge::new()),
        };
        // `to_init_gd` is how an object reaches itself from `init`: it is the one moment
        // there is no other `Gd` to take a weak reference from.
        let weak = weakref(&list.base.to_init_gd().to_variant()).to::<Gd<WeakRef>>();
        attach_events(ptr, &list.events, weak);
        list
    }
}

/// Records what an event reported, retaining the media it names.
///
/// # Safety
/// The event must be one of the list's, and `data` the [ListBridge] it was attached
/// with. The media in the payload is borrowed, and (for a deletion) the list's own
/// reference is gone by the time this returns, so the retain here is what keeps the
/// record valid.
unsafe fn record(event: *const libvlc_event_t, data: *mut c_void, kind: EventKind) {
    unsafe {
        let Some(bridge) = (data as *const ListBridge).as_ref() else {
            return;
        };
        let (item, index) = match kind {
            EventKind::WillAdd => {
                let added = (*event).u.media_list_will_add_item;
                (added.item, added.index)
            }
            EventKind::Added => {
                let added = (*event).u.media_list_item_added;
                (added.item, added.index)
            }
            EventKind::WillDelete => {
                let deleted = (*event).u.media_list_will_delete_item;
                (deleted.item, deleted.index)
            }
            EventKind::Deleted => {
                let deleted = (*event).u.media_list_item_deleted;
                (deleted.item, deleted.index)
            }
        };
        if !item.is_null() {
            libvlc_media_retain(item);
        }
        let held = HeldMedia(item);
        bridge.push(match kind {
            EventKind::WillAdd => ParkedListEvent::WillAdd(held, index),
            EventKind::Added => ParkedListEvent::Added(held, index),
            EventKind::WillDelete => ParkedListEvent::WillDelete(held, index),
            EventKind::Deleted => ParkedListEvent::Deleted(held, index),
        });
        bridge.queue_drain();
    }
}

/// Which of the four item events a callback is for.
#[derive(Clone, Copy)]
enum EventKind {
    WillAdd,
    Added,
    WillDelete,
    Deleted,
}

unsafe extern "C" fn will_add_callback(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe { record(event, data, EventKind::WillAdd) };
}

unsafe extern "C" fn added_callback(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe { record(event, data, EventKind::Added) };
}

unsafe extern "C" fn will_delete_callback(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe { record(event, data, EventKind::WillDelete) };
}

unsafe extern "C" fn deleted_callback(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe { record(event, data, EventKind::Deleted) };
}

unsafe extern "C" fn end_reached_callback(_event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let Some(bridge) = (data as *const ListBridge).as_ref() else {
            return;
        };
        bridge.push(ParkedListEvent::EndReached);
        bridge.queue_drain();
    }
}

/// Attaches the five list events, and records the weak reference the callbacks use.
fn attach_events(ptr: *mut libvlc_media_list_t, events: &ListBridge, weak: Gd<WeakRef>) {
    unsafe {
        match events.self_gd.lock() {
            Ok(mut self_gd) => *self_gd = Some(weak),
            Err(poisoned) => *poisoned.into_inner() = Some(weak),
        }

        let event_manager = libvlc_media_list_event_manager(ptr);
        let data = events as *const ListBridge as *mut c_void;
        for (event_type, callback) in [
            (
                libvlc_event_e_libvlc_MediaListWillAddItem,
                will_add_callback as unsafe extern "C" fn(_, _),
            ),
            (libvlc_event_e_libvlc_MediaListItemAdded, added_callback),
            (
                libvlc_event_e_libvlc_MediaListWillDeleteItem,
                will_delete_callback,
            ),
            (libvlc_event_e_libvlc_MediaListItemDeleted, deleted_callback),
            (
                libvlc_event_e_libvlc_MediaListEndReached,
                end_reached_callback,
            ),
        ] {
            libvlc_event_attach(
                event_manager,
                event_type as libvlc_event_type_t,
                Some(callback),
                data,
            );
        }
    }
}

#[godot_api]
impl VlcMediaList {
    /// A media was added to the list, at an index.
    ///
    /// # Parameters
    /// - [param media] the media that was added.
    /// - [param index] where in the list it went.
    ///
    /// # Note
    /// - This arrives on a later frame, not inside the call that added the media: see
    ///   the note on [signal item_will_add].
    /// - libvlc raises it on the thread that added the media -- a script's own thread
    ///   for [method add_media], and the parsing thread for the entries of a playlist
    ///   file, which is why it cannot be emitted any earlier than the next frame.
    #[signal]
    fn item_added(media: Gd<VlcMedia>, index: i32);
    /// A media is about to be added, at an index. Only emitted for a **writable** list:
    /// libvlc raises it from inside the modification, so it comes with the list's lock
    /// held and cannot be waited for.
    ///
    /// # Parameters
    /// The same two as [signal item_added].
    ///
    /// # Note
    /// - Do not call any [VLCMediaList] method from a handler for this or for any of
    ///   the other three item signals: libvlc is holding the list's own lock while it
    ///   sends them, and that lock is not recursive.
    /// - All four item signals are emitted from a deferred drain, so they arrive after
    ///   the call that caused them, in the order libvlc sent them.
    #[signal]
    fn item_will_add(media: Gd<VlcMedia>, index: i32);
    /// A media was removed from the list, and this is the one that was at that index.
    ///
    /// # Parameters
    /// The same two as [signal item_added]: the media, and the index it was removed
    /// from.
    ///
    /// # Note
    /// The media stays valid here: libvlc's list has already let go of it, but this
    /// binding holds a reference until this signal has gone out.
    #[signal]
    fn item_deleted(media: Gd<VlcMedia>, index: i32);
    /// A media is about to be removed from the list, at an index.
    ///
    /// # Parameters
    /// The same two as [signal item_deleted].
    ///
    /// # Note
    /// See [signal item_will_add] for what may not be done from a handler.
    #[signal]
    fn item_will_delete(media: Gd<VlcMedia>, index: i32);
    /// The parsing that fills a media's subitems has finished.
    ///
    /// # Note
    /// - It carries nothing, and it has to: libvlc sends this one with a payload member
    ///   it never writes, so there is nothing to read.
    /// - It is the way to know that a playlist's entries are all there.
    /// - It arrives from a **parse**, not from playing the media. Measured: libvlc sends
    ///   it from the one place it reports a media's parsed status changing
    ///   (`send_parsed_changed`, `lib/media.c:296-300`), while playing a media goes
    ///   through the input instead -- its entries still arrive, as
    ///   [signal item_added], but this never does. Call
    ///   [method VLCMedia.parse_request] to get it.
    /// - For a media that is not being parsed it never arrives, and it can arrive more
    ///   than once for a media that is parsed again.
    #[signal]
    fn end_reached();

    /// How many media the list holds.
    ///
    /// # Returns
    /// the count, `0` for an empty list.
    #[func]
    fn count(&self) -> i32 {
        let _lock = ListLock::new(self.ptr);
        unsafe { libvlc_media_list_count(self.ptr) }
    }

    /// The media at an index.
    ///
    /// # Parameters
    /// - [param index] the position, `0`-based.
    ///
    /// # Returns
    /// the media, or null when the index is outside the list. The reference libvlc
    /// hands back is the caller's, and [VLCMedia]'s own release is what gives it back.
    ///
    /// # Note
    /// The wrapper is new on every call: two calls for the same index answer two
    /// different objects that stand for the same media, exactly as
    /// [method VLCMedia.duplicate_media] does.
    #[func]
    fn item_at_index(&self, index: i32) -> Option<Gd<VlcMedia>> {
        let _lock = ListLock::new(self.ptr);
        // libvlc retains it on success, and `from_ptr` takes that reference over.
        let media = unsafe { libvlc_media_list_item_at_index(self.ptr, index) };
        VlcMedia::from_ptr(media)
    }

    /// Every media in the list, read under one lock.
    ///
    /// # Returns
    /// an array of [VLCMedia], empty when the list is. Positions in the array are
    /// positions in the list.
    ///
    /// # Note
    /// This is one lock for the whole list rather than one per element, which is what
    /// makes it the call to use for a list another thread is filling -- a playlist
    /// being parsed, say. The array is still a snapshot: the list can have changed by
    /// the time a caller looks at it.
    #[func]
    fn get_media_array(&self) -> Array<Gd<VlcMedia>> {
        let _lock = ListLock::new(self.ptr);
        let count = unsafe { libvlc_media_list_count(self.ptr) };
        let mut media = Array::new();
        for index in 0..count {
            let item = unsafe { libvlc_media_list_item_at_index(self.ptr, index) };
            if let Some(media_item) = VlcMedia::from_ptr(item) {
                media.push(&media_item);
            }
        }
        media
    }

    /// Where a media sits in the list.
    ///
    /// # Parameters
    /// - [param media] the media to look for.
    ///
    /// # Returns
    /// its index, or `-1` when it is not in the list. libvlc compares the descriptors
    /// themselves, so a duplicate of a media in the list is not found.
    #[func]
    fn index_of_item(&self, media: Gd<VlcMedia>) -> i32 {
        let _lock = ListLock::new(self.ptr);
        let ptr = media.bind().media_ptr;
        unsafe { libvlc_media_list_index_of_item(self.ptr, ptr) }
    }

    /// Appends a media to the list.
    ///
    /// # Parameters
    /// - [param media] the media to add. The list takes its own reference, so the
    ///   caller may let go of it.
    ///
    /// # Returns
    /// `0`, or `-1` for a read-only list -- a media's own subitems are read-only, and
    /// libvlc writes a line to its log saying so.
    #[func]
    fn add_media(&self, media: Gd<VlcMedia>) -> i32 {
        let _lock = ListLock::new(self.ptr);
        let ptr = media.bind().media_ptr;
        unsafe { libvlc_media_list_add_media(self.ptr, ptr) }
    }

    /// Inserts a media at a position.
    ///
    /// # Parameters
    /// - [param media] the media to insert.
    /// - [param index] where it goes; the rest move down.
    ///
    /// # Returns
    /// `0`, or `-1` for a read-only list or an index outside it.
    #[func]
    fn insert_media(&self, media: Gd<VlcMedia>, index: i32) -> i32 {
        let _lock = ListLock::new(self.ptr);
        let ptr = media.bind().media_ptr;
        unsafe { libvlc_media_list_insert_media(self.ptr, ptr, index) }
    }

    /// Removes the media at a position, and lets go of it.
    ///
    /// # Parameters
    /// - [param index] the position to remove.
    ///
    /// # Returns
    /// `0`, or `-1` for a read-only list or an index outside it.
    ///
    /// # Note
    /// The list's reference goes away here. A script that still holds the [VLCMedia]
    /// keeps it alive; one that does not, and has no other reference, sees it freed --
    /// and [signal item_deleted] holds a reference of its own until it has gone out.
    #[func]
    fn remove_index(&self, index: i32) -> i32 {
        let _lock = ListLock::new(self.ptr);
        unsafe { libvlc_media_list_remove_index(self.ptr, index) }
    }

    /// Whether the list refuses changes.
    ///
    /// # Returns
    /// `true` for a media's own subitems list, which is filled by libvlc's parsing
    /// thread and belongs to that media; `false` for a list a script built.
    #[func]
    fn is_read_only(&self) -> bool {
        unsafe { libvlc_media_list_is_readonly(self.ptr) }
    }

    /// Sets the media this list belongs to.
    ///
    /// # Parameters
    /// - [param media] the media to associate, or null to leave it.
    ///
    /// # Note
    /// - Only one association exists and it is set when libvlc builds a media's own
    ///   subitems list: for such a list this call does nothing at all, silently.
    /// - It takes the list's lock itself, so it must not be called from an event
    ///   handler -- see [signal item_will_add].
    #[func]
    fn set_media(&self, media: Option<Gd<VlcMedia>>) {
        let ptr = media
            .as_ref()
            .map_or(std::ptr::null_mut(), |media| media.bind().media_ptr);
        unsafe { libvlc_media_list_set_media(self.ptr, ptr) };
    }

    /// The media this list belongs to, if any.
    ///
    /// # Returns
    /// the media whose entries these are -- what a list from
    /// [method VLCMedia.get_subitems] answers -- or null for a list a script built.
    ///
    /// # Note
    /// Like [method set_media] this takes the list's lock itself, and the reference it
    /// returns is the caller's.
    #[func]
    fn media(&self) -> Option<Gd<VlcMedia>> {
        let media = unsafe { libvlc_media_list_media(self.ptr) };
        VlcMedia::from_ptr(media)
    }

    /// Emits the signals for everything the list's callbacks have recorded.
    ///
    /// # Note
    /// This is the deferred call the event callbacks make, not something a script
    /// calls: a signal carrying a [VLCMedia] has to build that object on the main
    /// thread, and the callbacks run on libvlc's.
    #[func]
    fn drain_parked_events(&mut self) {
        self.events.mark_drained();
        while let Some(event) = self.events.pop() {
            match event {
                ParkedListEvent::WillAdd(media, index) => {
                    if let Some(media) = VlcMedia::from_ptr(media.into_ptr()) {
                        self.signals().item_will_add().emit(&media, index);
                    }
                }
                ParkedListEvent::Added(media, index) => {
                    if let Some(media) = VlcMedia::from_ptr(media.into_ptr()) {
                        self.signals().item_added().emit(&media, index);
                    }
                }
                ParkedListEvent::WillDelete(media, index) => {
                    if let Some(media) = VlcMedia::from_ptr(media.into_ptr()) {
                        self.signals().item_will_delete().emit(&media, index);
                    }
                }
                ParkedListEvent::Deleted(media, index) => {
                    if let Some(media) = VlcMedia::from_ptr(media.into_ptr()) {
                        self.signals().item_deleted().emit(&media, index);
                    }
                }
                ParkedListEvent::EndReached => self.signals().end_reached().emit(),
            }
        }
    }

    /// Wraps a list libvlc hands over, taking the reference that came with it.
    ///
    /// Used for a media's own subitems, which `libvlc_media_subitems` retains for the
    /// caller. The wrapper owns that reference, `Drop` gives it back, and the events
    /// are attached like they are for a list a script built.
    pub(crate) fn from_ptr(ptr: *mut libvlc_media_list_t) -> Option<Gd<Self>> {
        if ptr.is_null() {
            return None;
        }
        let list = Gd::from_init_fn(|base| Self {
            base,
            ptr,
            events: Box::new(ListBridge::new()),
        });
        let weak = weakref(&list.to_variant()).to::<Gd<WeakRef>>();
        attach_events(ptr, list.bind().events.as_ref(), weak);
        Some(list)
    }
}
