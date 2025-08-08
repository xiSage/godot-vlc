use std::{
    ffi::{c_int, c_uchar, c_void},
    io::{Read, Seek, SeekFrom},
    ptr,
};

use crate::{vlc::*, vlc_instance};
use godot::{
    classes::{file_access::ModeFlags, WeakRef},
    global::weakref,
    prelude::*,
};

#[derive(GodotClass)]
#[class(base=Resource, rename=VLCMedia, no_init)]
pub struct VlcMedia {
    base: Base<Resource>,
    // file: *mut GFile,
    path: *mut GString,
    pub media_ptr: *mut libvlc_media_t,
    self_gd: *mut Gd<WeakRef>,
}

#[godot_api]
impl VlcMedia {
    #[constant]
    const PARSED_STATUS_NONE: i32 = libvlc_media_parsed_status_t_libvlc_media_parsed_status_none;
    #[constant]
    const PARSED_STATUS_PENDING: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_pending;
    #[constant]
    const PARSED_STATUS_SKIPPED: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_skipped;
    #[constant]
    const PARSED_STATUS_FAILED: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_failed;
    #[constant]
    const PARSED_STATUS_TIMEOUT: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_timeout;
    #[constant]
    const PARSED_STATUS_CANCELLED: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_cancelled;
    #[constant]
    const PARSED_STATUS_DONE: i32 = libvlc_media_parsed_status_t_libvlc_media_parsed_status_done;

    /// Parse media if it's a local file.
    #[constant]
    const PARSE_FLAG_PARSE_LOCAL: i32 = libvlc_media_parse_flag_t_libvlc_media_parse_local;
    /// Parse media even if it's a network file.
    #[constant]
    const PARSE_FLAG_PARSE_NETWORK: i32 = libvlc_media_parse_flag_t_libvlc_media_parse_network;
    /// Force parsing the media even if it would be skipped.
    #[constant]
    const PARSE_FLAG_PARSE_FORCED: i32 = libvlc_media_parse_flag_t_libvlc_media_parse_forced;
    /// Fetch meta and cover art using local resources.
    #[constant]
    const PARSE_FLAG_FETCH_LOCAL: i32 = libvlc_media_parse_flag_t_libvlc_media_fetch_local;
    /// Fetch meta and cover art using network resources.
    #[constant]
    const PARSE_FLAG_FETCH_NETWORK: i32 = libvlc_media_parse_flag_t_libvlc_media_fetch_network;
    /// Interact with the user (via libvlc_dialog_cbs) when preparsing this item (and not its sub items).
    ///
    /// Set this flag in order to receive a callback when the input is asking for credentials.
    #[constant]
    const PARSE_FLAG_DO_INTERACT: i32 = libvlc_media_parse_flag_t_libvlc_media_do_interact;

    /// Parsing state of a `VLCMedia` changed.
    #[signal]
    fn parsed_changed(status: i32);

    #[func]
    fn load_from_file(path: GString) -> Gd<Self> {
        let path = Box::into_raw(Box::new(path));
        let media_ptr = unsafe {
            libvlc_media_new_callbacks(
                Some(media_open_callback),
                Some(media_read_callback),
                Some(media_seek_callback),
                Some(media_close_cb),
                path as *mut c_void,
            )
        };
        let mut media = Gd::from_init_fn(|base| Self {
            base,
            path,
            media_ptr,
            self_gd: ptr::null_mut(),
        });
        let self_gd = Box::into_raw(Box::new(weakref(&media.to_variant()).to::<Gd<WeakRef>>()));
        media.bind_mut().self_gd = self_gd;

        // signals
        unsafe {
            fn get_media(ptr: *mut c_void) -> Gd<VlcMedia> {
                unsafe { (ptr as *mut WeakRef).as_mut().unwrap().get_ref().to() }
            }

            let event_manager = libvlc_media_event_manager(media_ptr);

            unsafe extern "C" fn parsed_changed_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                let mut media = get_media(user_data);
                let status = libvlc_media_get_parsed_status(media.bind().media_ptr);
                media.call_deferred(
                    "emit_signal",
                    &[
                        StringName::from(c"parsed_changed").to_variant(),
                        status.to_variant(),
                    ],
                );
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaParsedChanged,
                Some(parsed_changed_callback),
                self_gd as *mut c_void,
            );
        }

        media
    }

    /// Parse the media asynchronously with options.\
    /// This fetches (local or network) art, meta data and/or tracks information.\
    /// To track when this is over you can listen to [signal parsed_changed] signal. However if this functions returns an error, you will not receive any events.\
    /// It uses a flag to specify parse options ([constant PARSE_FLAG_PARSE_LOCAL], [constant PARSE_FLAG_PARSE_NETWORK],...). All these flags can be combined. By default, media is parsed if it's a local file.
    ///
    /// # Note
    /// Parsing can be aborted with [method parse_stop].
    ///
    /// # Parameters
    /// - [param parse_flag] parse options:
    /// - [param timeout] maximum time allowed to preparse the media. If -1, the default "preparse-timeout" option will be used as a timeout. If 0, it will wait indefinitely. If > 0, the timeout will be used (in milliseconds).
    ///
    /// # Returns
    /// -1 in case of error, 0 otherwise
    #[func]
    fn parse_request(&mut self, parse_flag: i32, timeout: i32) -> i32 {
        unsafe {
            libvlc_media_parse_request(vlc_instance::get(), self.media_ptr, parse_flag | libvlc_media_parse_flag_t_libvlc_media_fetch_local, timeout)
        }
    }

    /// Stop the parsing of the media.\
    /// When the media parsing is stopped, the [signal parsed_changed] signal will be sent with the [constant PARSED_STATUS_TIMEOUT] status.
    #[func]
    fn parse_stop(&mut self) {
        unsafe {
            libvlc_media_parse_stop(vlc_instance::get(), self.media_ptr);
        }
    }
}

impl Drop for VlcMedia {
    fn drop(&mut self) {
        unsafe {
            if !self.media_ptr.is_null() {
                libvlc_media_release(self.media_ptr);
            }
            if !self.path.is_null() {
                drop(Box::from_raw(self.path));
            }
            if !self.self_gd.is_null() {
                drop(Box::from_raw(self.self_gd));
            }
        }
    }
}

unsafe extern "C" fn media_open_callback(
    opaque: *mut c_void,
    datap: *mut *mut c_void,
    sizep: *mut u64,
) -> c_int {
    if let Some(path) = (opaque as *mut GString).as_ref() {
        if let Ok(mut file) = GFile::open(path, ModeFlags::READ) {
            *sizep = file.length();
            let _ = file.seek(SeekFrom::Start(0));
            *datap = Box::into_raw(Box::new(file)) as *mut c_void;
            return 0;
        }
    }
    godot_error!("godot-vlc: unable to open media file");
    -1
}

unsafe extern "C" fn media_read_callback(
    opaque: *mut c_void,
    buf: *mut c_uchar,
    len: usize,
) -> isize {
    if let Some(file) = (opaque as *mut GFile).as_mut() {
        let buf = std::slice::from_raw_parts_mut(buf, len);
        match file.read(buf) {
            Ok(n) => n.try_into().unwrap(),
            Err(_) => -1,
        }
    } else {
        -1
    }
}

unsafe extern "C" fn media_seek_callback(opaque: *mut c_void, offset: u64) -> c_int {
    if let Some(file) = (opaque as *mut GFile).as_mut() {
        match file.seek(SeekFrom::Start(offset)) {
            Ok(_) => 0,
            Err(_) => -1,
        }
    } else {
        -1
    }
}

unsafe extern "C" fn media_close_cb(opaque: *mut ::std::os::raw::c_void) {
    if !opaque.is_null() {
        drop(Box::from_raw(opaque as *mut GFile));
    }
}
