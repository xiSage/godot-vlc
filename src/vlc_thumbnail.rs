//! libvlc's thumbnail requests: `libvlc_media_thumbnail_request_*`.
//!
//! A request is how a thumbnail is asked for. It is **asynchronous** -- libvlc may decode a
//! frame, convert it, and only then answer -- and the answer arrives as the media's
//! the media's     humbnail_generated signal signal, carrying a
//! [crate::vlc_picture::VlcPicture] or nothing.
//!
//! The request itself is an opaque pointer, and this wrapper owns it: `Drop` destroys it,
//! and [method destroy] destroys it early, which is also the only way to cancel. A request
//! is not reference-counted -- libvlc allocates it plainly and frees it in one place -- so
//! there is no such thing as two owners of one request.
//!
//! # Cancelling is not as safe as its documentation says
//! Read out of the source at the revision this binding pins, not measured:
//! `libvlc_media_thumbnail_request_destroy` documents "no events will be emitted after this
//! call", and its implementation does two things that disagree with that. For a request
//! that has not started, cancelling calls the completion callback itself, so the event is
//! sent *from inside* `destroy`, before it releases the instance and frees the request. For
//! a request that is already running, cancelling only interrupts it, and the callback
//! arrives from the worker thread afterwards -- after the request has been freed, with the
//! callback reading the media and the instance out of that freed memory.
//!
//! The window is libvlc's, not this binding's, and nothing here can close it: the request
//! must be destroyed eventually, and destroying it is what starts the race in the running
//! case. What the binding does about it is to say so, here and on [method destroy], so that
//! cancelling a thumbnail is a decision taken knowingly rather than a safe-looking call.

use godot::prelude::*;

use crate::vlc::*;

/// One outstanding thumbnail request, holding the only reference to it.
///
/// # Where one comes from
/// [method crate::vlc_media::VlcMedia.thumbnail_request_by_time] and
/// [method crate::vlc_media::VlcMedia.thumbnail_request_by_pos] return one. There is no
/// constructor: libvlc's own creation call takes a media and an instance.
#[derive(GodotClass)]
#[class(base=RefCounted, rename=VLCThumbnailRequest, no_init)]
pub struct VlcThumbnailRequest {
    base: Base<RefCounted>,
    /// The request, or `None` once it has been destroyed.
    ptr: Option<*mut libvlc_media_thumbnail_request_t>,
}

impl Drop for VlcThumbnailRequest {
    fn drop(&mut self) {
        self.destroy();
    }
}

#[allow(clippy::unnecessary_cast)]
#[godot_api]
impl VlcThumbnailRequest {
    /// Seek to the exact time asked for, at the cost of decoding more of the media.
    ///
    /// The cast is for the targets where bindgen types this enum differently -- `c_int` here,
    /// something else on Linux and Android -- which is what made this constant fail to compile
    /// there while Windows was happy.
    #[constant]
    pub(crate) const SEEK_PRECISE: i32 =
        libvlc_thumbnailer_seek_speed_t_libvlc_media_thumbnail_seek_precise as i32;
    /// Jump near the time asked for and take what is there.
    #[constant]
    pub(crate) const SEEK_FAST: i32 =
        libvlc_thumbnailer_seek_speed_t_libvlc_media_thumbnail_seek_fast as i32;

    /// Destroys the request, cancelling it if it has not finished.
    ///
    /// # Note
    /// - **Calling it is the only way to cancel**, and it is not optional: a request that is
    ///   never destroyed leaks the request, its reference to the media and its reference to
    ///   the instance. Dropping the last reference to this object does it too, so a script
    ///   that forgets is saved by the wrapper -- what a script cannot do is keep the object
    ///   and expect the request to go away.
    /// - **Cancelling a request that is already running races libvlc against itself.** Its
    ///   header promises "no events will be emitted after this call"; its implementation
    ///   frees the request while the worker may still be about to call back with it. Nothing
    ///   this binding does can remove that; see the module documentation for the two paths
    ///   and where they differ. Cancelling a request that has not started is safe, and the
    ///   event arrives from inside the call -- a the media's     humbnail_generated signal with nothing in it.
    /// - Destroying twice is safe here, unlike libvlc's own function, which dereferences
    ///   whatever it is given.
    #[func]
    fn destroy(&mut self) {
        let Some(ptr) = self.ptr.take() else {
            return;
        };
        unsafe { libvlc_media_thumbnail_request_destroy(ptr) };
    }
}

impl VlcThumbnailRequest {
    /// Wraps a request libvlc just created, taking ownership of it.
    ///
    /// libvlc returns a plain pointer that it documents as "must be released by
    /// `libvlc_media_thumbnail_request_destroy()`", and that release is this wrapper's
    /// `Drop`. Nothing else may hold one, so a caller that does not wrap it has leaked it.
    pub(crate) fn from_ptr(ptr: *mut libvlc_media_thumbnail_request_t) -> Option<Gd<Self>> {
        if ptr.is_null() {
            return None;
        }
        Some(Gd::from_init_fn(|base| Self {
            base,
            ptr: Some(ptr),
        }))
    }
}
