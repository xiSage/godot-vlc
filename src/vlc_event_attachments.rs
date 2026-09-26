//! The libvlc events one object attached, kept so that its `Drop` can detach them.
//!
//! `libvlc_event_detach` finds a handler by the triple it was attached with --
//! event, callback and user data -- compared byte for byte, and a triple libvlc
//! does not know is not an error there: it is `abort()`. So the triple is
//! recorded when it is attached rather than rebuilt at detach time, where a
//! pointer that moved (a boxed field, the object behind a `Gd` handle rather
//! than the handle) would turn a teardown into a crash.
//!
//! Detaching is also the only teardown that does not go through libvlc's
//! reference counting. `libvlc_media_player_release` stops the player, joins its
//! threads and destroys its event manager only when the count reaches zero, and
//! a wrapper can be freed while another holder keeps the libvlc object alive: a
//! media list player retains the player it is given
//! ([method VLCMediaListPlayer.set_media_player]), and a media list retains every
//! media in it. In that state nothing releases the object, the callbacks stay
//! attached, and the user data they were attached with is freed with the
//! wrapper -- so the next event calls into freed memory. Detaching at `Drop` is
//! what keeps that from happening, and it works whatever the count is: libvlc
//! runs a callback while holding the event manager's lock, and takes that same
//! lock to detach, so the detach returns only once no callback of these is
//! running.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use crate::vlc::{
    libvlc_callback_t, libvlc_event_attach, libvlc_event_detach, libvlc_event_manager_t,
    libvlc_event_type_t,
};

/// One registration, as libvlc will look for it again.
struct Attachment {
    event_type: libvlc_event_type_t,
    callback: libvlc_callback_t,
    user_data: *mut c_void,
}

/// The events one object attached to one event manager, in attach order.
#[derive(Default)]
pub(crate) struct EventAttachments {
    attached: Vec<Attachment>,
}

impl EventAttachments {
    /// Attach one event and remember the triple `detach_all` has to hand back.
    ///
    /// # Safety
    /// `manager` must be a live libvlc event manager, and `user_data` must stay
    /// valid until [method detach_all] runs.
    pub(crate) unsafe fn attach(
        &mut self,
        manager: *mut libvlc_event_manager_t,
        event_type: libvlc_event_type_t,
        callback: libvlc_callback_t,
        user_data: *mut c_void,
    ) {
        unsafe {
            libvlc_event_attach(manager, event_type, callback, user_data);
        }
        self.attached.push(Attachment {
            event_type,
            callback,
            user_data,
        });
        OUTSTANDING.fetch_add(1, Ordering::Relaxed);
    }

    /// Detach everything, newest first, and forget it.
    ///
    /// Returns once no callback of these is running (see the module note). The
    /// list is emptied as it goes: detaching the same triple twice is an abort,
    /// not a no-op.
    ///
    /// # Safety
    /// `manager` must be the event manager the events were attached to, and it
    /// must still be alive.
    pub(crate) unsafe fn detach_all(&mut self, manager: *mut libvlc_event_manager_t) {
        for attachment in self.attached.drain(..).rev() {
            unsafe {
                libvlc_event_detach(
                    manager,
                    attachment.event_type,
                    attachment.callback,
                    attachment.user_data,
                );
            }
            OUTSTANDING.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// How many attachments are outstanding over the whole process.
///
/// Only the tests read it ([method VLCMediaPlayer._debug_outstanding_attachments]):
/// "every wrapper detached what it attached" is the invariant they check, and it
/// is the one thing about the detach that cannot be seen from the outside.
static OUTSTANDING: AtomicIsize = AtomicIsize::new(0);

/// The value [member OUTSTANDING] holds: zero when every wrapper that was freed
/// detached what it attached.
pub(crate) fn outstanding() -> i64 {
    OUTSTANDING.load(Ordering::Relaxed) as i64
}
