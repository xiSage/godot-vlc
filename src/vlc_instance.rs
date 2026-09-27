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

use crate::{util::cstring_from_gstring, vlc, vlc_track::c_string};
use godot::{
    classes::{
        Engine, ProjectSettings, class_macros::sys::GDEXTENSION_VARIANT_TYPE_STRING,
        notify::ObjectNotification,
    },
    prelude::*,
    register::info::PropertyHint,
};
use printf::printf;
use std::{
    collections::VecDeque,
    ffi::{CString, c_char, c_int, c_uint, c_void},
    mem, ptr,
    sync::{
        Mutex,
        atomic::{AtomicI32, Ordering},
    },
};

/// The rung that reports nothing, and the highest value `vlc/log_level` accepts.
const LOG_LEVEL_DISABLED: i32 = 4;

/// How many log lines may wait for the main thread at once.
///
/// The queue is drained every frame, so it is only ever full if something is
/// logging far faster than the game draws. Unlike the console line, which is
/// written where the message arrives, a line that does not fit here is dropped
/// silently: [signal VLCInstance.log_message] reports what it can, and the console
/// remains the record.
const PARKED_LOG_CAPACITY: usize = 64;

/// Which rung of the log a libvlc message belongs to, or `None` for a level this
/// extension does not report.
///
/// libvlc's own enum is `DEBUG=0`, `NOTICE=2`, `WARNING=3`, `ERROR=4` -- it skips
/// 1 -- while the `vlc/log_level` setting and [method VLCInstance.set_log_level]
/// count rungs: `0` reports everything, `1` drops debug, `2` drops debug and
/// notice, `3` reports errors alone, `4` disables the log. The two scales are
/// different, so every message is translated into the rung it belongs to and that
/// is what the signal and the threshold both use.
fn rung_of(level: vlc::libvlc_log_level) -> Option<i32> {
    match level {
        vlc::libvlc_log_level_LIBVLC_DEBUG => Some(0),
        vlc::libvlc_log_level_LIBVLC_NOTICE => Some(1),
        vlc::libvlc_log_level_LIBVLC_WARNING => Some(2),
        vlc::libvlc_log_level_LIBVLC_ERROR => Some(3),
        _ => None,
    }
}

/// Whether a message on this rung is reported at the configured rung.
///
/// The configured value is a floor: `0` reports everything from debug up, `3`
/// reports errors alone, and `4` -- above every rung rather than a rung of its own
/// -- reports nothing, which is what "Disabled" means. This is the arrangement the
/// callback already had, kept rather than re-derived: it is what `vlc/log_level`'s
/// hint string ("Debug, Info, Warning, Error, Disabled") describes.
fn reports(configured: i32, rung: i32) -> bool {
    configured <= rung
}

/// One log line, on its way from libvlc's thread to the main one.
///
/// The strings are copied out of libvlc's context, which is only valid until the
/// logging callback returns -- and whose module name may even point into a stack
/// buffer of libvlc's own frame.
#[derive(Clone, Debug, PartialEq)]
struct ParkedLog {
    level: i32,
    module: String,
    message: String,
}

/// Where the logging callback leaves what it received, plus the level it reports.
///
/// One allocation holds both because both are handed to libvlc as one pointer and
/// that pointer has to stay put for as long as the instance lives: [VLCInstance]
/// keeps this in a `Box` for exactly that reason.
struct LogSink {
    /// The rung from `vlc/log_level`, or from [method VLCInstance.set_log_level].
    /// An atomic because it is read on libvlc's threads and written on the main
    /// one -- and because re-registering the callback to change it would deadlock
    /// (`libvlc_log_set` waits for pending callbacks, and it is being called from
    /// one).
    level: AtomicI32,
    queue: Mutex<VecDeque<ParkedLog>>,
}

impl LogSink {
    fn new(level: i32) -> Self {
        Self {
            level: AtomicI32::new(level.clamp(0, LOG_LEVEL_DISABLED)),
            queue: Mutex::new(VecDeque::with_capacity(PARKED_LOG_CAPACITY)),
        }
    }

    /// Records one line for the main thread. Called from libvlc's threads.
    fn push(&self, log: ParkedLog) {
        // A poisoned lock means another thread panicked while holding it. Losing a
        // log line is better than propagating a panic into libvlc's thread.
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len() < PARKED_LOG_CAPACITY {
            queue.push_back(log);
        }
    }

    fn pop(&self) -> Option<ParkedLog> {
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.pop_front()
    }
}

pub fn get() -> *mut vlc::libvlc_instance_t {
    Engine::singleton()
        .get_singleton("VLCInstance")
        .expect("VLCInstance not found")
        .cast::<VLCInstance>()
        .bind()
        .get_vlc_instance()
}

/// The reason libvlc recorded for the last failure **on this thread**.
///
/// `libvlc_errmsg` keeps one message per thread and is **not** cleared by a later
/// success -- only by another error, by [clear_last_error], or by the thread
/// ending. Reading it without clearing first therefore answers "something failed
/// here at some point", which is why this binding reads it only where it has just
/// seen a failure, right after clearing it. The string belongs to libvlc and is
/// valid until the next error on this thread, so this copies it.
pub(crate) fn last_error() -> String {
    c_string(unsafe { vlc::libvlc_errmsg() })
}

/// Drops whatever error this thread had recorded, so that the next [last_error]
/// answers about the call in between and nothing else.
pub(crate) fn clear_last_error() {
    unsafe { vlc::libvlc_clearerr() };
}

/// Turns whatever libvlc has logged since the last frame into signals.
///
/// Called from a player's per-frame hook: `VLCInstance` is an `Object`, so it has
/// no frame of its own, and a signal needs an object to be emitted from. The
/// console line was already written where the message arrived, so this is only the
/// script-visible half -- which means a project that never creates a
/// [VLCMediaPlayer] gets the console output and no signal.
pub(crate) fn drain_parked_logs() {
    let Some(singleton) = Engine::singleton().get_singleton("VLCInstance") else {
        return;
    };
    let mut singleton = singleton.cast::<VLCInstance>();
    singleton.bind_mut().emit_parked_logs();
}

/// The one LibVLC instance this extension runs on, registered as an engine
/// singleton while the scene layer is initialised.
///
/// It is built from two project settings -- `vlc/log_level` and `vlc/arguments` --
/// and both are read **once**, there: an edit to either applies to the next run of
/// the project, not to the one that is running. `vlc/log_level` has a runtime
/// override ([method set_log_level]); `vlc/arguments` has none, because libvlc
/// offers no way to change an instance's arguments after the instance exists. What
/// this run was started with is [method get_arguments], and whether there is an
/// instance at all is [method has_instance].
///
/// libvlc's own header advises against that list -- "There is absolutely no warranty
/// or promise of forward, backward and cross-platform compatibility with regards to
/// libvlc_new() arguments. We recommend that you do not use them, other than when
/// debugging." -- so an empty `vlc/arguments` is the supported state, and the
/// arguments that are worth setting per media go through
/// [method VLCMedia.add_option] instead.
#[derive(GodotClass)]
#[class(base=Object, tool)]
pub struct VLCInstance {
    instance: Option<*mut vlc::libvlc_instance_t>,
    /// What `libvlc_new` was handed, kept for [method VLCInstance.get_arguments]:
    /// the setting it came from can be edited afterwards and this instance cannot
    /// follow it, so the copy is the only record of what this run is running with.
    arguments: Vec<GString>,
    log: Box<LogSink>,
    base: Base<Object>,
}

#[godot_api]
impl IObject for VLCInstance {
    fn init(base: Base<Object>) -> Self {
        if !ProjectSettings::singleton().has_setting("vlc/log_level") {
            ProjectSettings::singleton().set_setting("vlc/log_level", &Variant::from(4));
        }
        ProjectSettings::singleton().set_initial_value("vlc/log_level", &Variant::from(4));
        let mut info = VarDictionary::new();
        let _ = info.insert("name", "vlc/log_level");
        let _ = info.insert("type", VariantType::INT);
        let _ = info.insert("hint", PropertyHint::ENUM);
        let _ = info.insert("hint_string", "Debug, Info, Warning, Error, Disabled");
        ProjectSettings::singleton().add_property_info(&info);
        ProjectSettings::singleton().set_restart_if_changed("vlc/log_level", true);
        let debug_level: i32 = ProjectSettings::singleton()
            .get_setting("vlc/log_level")
            .try_to()
            .unwrap();

        if !ProjectSettings::singleton().has_setting("vlc/arguments") {
            ProjectSettings::singleton()
                .set_setting("vlc/arguments", &Variant::from(Array::<GString>::new()));
        }
        ProjectSettings::singleton()
            .set_initial_value("vlc/arguments", &Variant::from(Array::<GString>::new()));
        let mut info = VarDictionary::new();
        let _ = info.insert("name", "vlc/arguments");
        let _ = info.insert("type", VariantType::ARRAY);
        let _ = info.insert("hint", PropertyHint::TYPE_STRING);
        let _ = info.insert(
            "hint_string",
            format!("{}:", GDEXTENSION_VARIANT_TYPE_STRING),
        );
        ProjectSettings::singleton().add_property_info(&info);
        ProjectSettings::singleton().set_restart_if_changed("vlc/arguments", true);
        // A project that writes this setting by hand can leave it as a plain array,
        // which is not the array of strings the setting is declared as and does not
        // convert. Starting with none is the safe reading of that; saying so is what
        // keeps it from being a silent one.
        let configured_setting = ProjectSettings::singleton().get_setting("vlc/arguments");
        let configured_arguments: Array<GString> = match configured_setting.try_to() {
            Ok(configured_arguments) => configured_arguments,
            Err(_) => {
                godot_warn!(
                    "godot-vlc: vlc/arguments is not an array of strings ({configured_setting:?}); starting with none"
                );
                Array::new()
            }
        };

        // Copied rather than used in place: a Godot Array is reference-counted, so
        // appending the Android default to it would edit the project setting.
        let configured: Vec<String> = configured_arguments
            .iter_shared()
            .map(|argument| argument.to_string())
            .collect();
        let arguments: Vec<GString> = effective_arguments(&configured, cfg!(target_os = "android"))
            .into_iter()
            .map(|argument| GString::from(argument.as_str()))
            .collect();

        let args: Vec<CString> = arguments
            .iter()
            .cloned()
            .map(cstring_from_gstring)
            .collect();
        let argc = args.len() as c_int;
        let args: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        let argv = args.as_ptr();

        // LibVLC derives its plugin, libexec and data directories from the
        // location of its own module and says nothing when they turn out not to
        // exist. Point them explicitly at the runtime we ship before any
        // instance is created. See src/vlc_runtime.rs.
        crate::vlc_runtime::configure_vlc_paths();

        let log = Box::new(LogSink::new(debug_level));

        // The reason matters more here than anywhere else: an instance that did
        // not come up makes every other call in this extension meaningless, and
        // libvlc says why through `libvlc_errmsg` rather than through a return
        // value. Reporting it is what turns a null pointer into a diagnosis.
        //
        // This is the one place that does **not** clear the status first, and the
        // reason is measured: the thread-local that holds it is created while
        // libvlc initialises, so `libvlc_clearerr` before the first `libvlc_new`
        // dereferences a key that does not exist yet -- it crashes rather than
        // answering NULL. `libvlc_new` records its own failure, so the message is
        // there to read without clearing anything.
        // What the instance is being started with, said once and in one line. The
        // arguments come from a project setting that is read here and nowhere else,
        // so a project that wonders why an edit did nothing has no other way to see
        // what this run actually got. It is printed before the call rather than
        // after it, so that a `libvlc_new` that fails is still explained by what it
        // was asked for -- and it is the same list [method get_arguments] answers
        // with, quoted the same way.
        godot_print!(
            "godot-vlc: libvlc_new({} args): [{}]",
            arguments.len(),
            arguments
                .iter()
                .map(|argument| format!("{:?}", argument.to_string()))
                .collect::<Vec<_>>()
                .join(", ")
        );

        let instance = unsafe { vlc::libvlc_new(argc, argv) };
        if instance.is_null() {
            godot_error!(
                "godot-vlc: libvlc_new failed, so this extension has no LibVLC to work with: {}",
                last_error()
            );
            return Self {
                instance: None,
                // Nothing was started, so there are no arguments in force:
                // [method get_arguments] answers with the empty list rather than
                // with the ones that were refused.
                arguments: Vec::new(),
                log,
                base,
            };
        }

        // The callback is attached with the sink as its data, and both the level
        // and the queue live in there: the pointer has to stay valid for as long
        // as libvlc can call it, which is why the sink is a `Box` on this object.
        #[allow(clippy::missing_transmute_annotations)]
        let cb = unsafe { Some(mem::transmute(VLCInstance::log_callback_impl as *const ())) };
        unsafe {
            vlc::libvlc_log_set(instance, cb, log.as_ref() as *const LogSink as *mut c_void);
        }
        Self {
            instance: Some(instance),
            arguments,
            log,
            base,
        }
    }

    fn on_notification(&mut self, what: ObjectNotification) {
        if what == ObjectNotification::PREDELETE
            && let Some(instance) = self.instance.take()
        {
            unsafe {
                // Unset before releasing: this is the call that waits for a log
                // callback already running to return, so that nothing can be
                // writing into the sink -- which lives in this object -- after
                // it returns. The release that follows destroys the instance.
                vlc::libvlc_log_unset(instance);
                vlc::libvlc_release(instance);
            }
        }
    }
}

#[godot_api]
impl VLCInstance {
    /// `0` reports everything libvlc says, including the debug chatter that only
    /// makes sense with a source tree at hand.
    #[constant]
    const LOG_LEVEL_DEBUG: i32 = 0;
    /// `1` drops debug and keeps the rest.
    #[constant]
    const LOG_LEVEL_INFO: i32 = 1;
    /// `2` drops debug and notice, and keeps warnings and errors.
    #[constant]
    const LOG_LEVEL_WARNING: i32 = 2;
    /// `3` keeps errors alone.
    #[constant]
    const LOG_LEVEL_ERROR: i32 = 3;
    /// `4` reports nothing. This is what `vlc/log_level` defaults to.
    #[constant]
    const LOG_LEVEL_DISABLED: i32 = 4;

    /// Emitted for every line libvlc logs, as far as the configured level lets it
    /// through.
    ///
    /// # Parameters
    /// - [param level] which rung the message belongs to: [constant LOG_LEVEL_DEBUG],
    ///   [constant LOG_LEVEL_INFO], [constant LOG_LEVEL_WARNING] or
    ///   [constant LOG_LEVEL_ERROR]. These are the same values
    ///   [method set_log_level] takes, so a handler can compare them.
    /// - [param module] the libvlc module that logged, with its directory and
    ///   extension already stripped (`access_http`, `avcodec`), or `""` when
    ///   libvlc did not name one.
    /// - [param message] the formatted message, without the module or the source
    ///   location.
    ///
    /// # Note
    /// - It arrives from a [VLCMediaPlayer]'s per-frame work, because a signal
    ///   needs an object that runs every frame and `VLCInstance` is an `Object`,
    ///   which has none. **A project that never creates a player gets the console
    ///   output and no signal.**
    /// - The console line is written where the message arrives -- on libvlc's
    ///   thread -- and carries more than this signal does: warnings and errors
    ///   also name the C source file and line they came from. This signal is the
    ///   script-visible half, and it is deliberately the smaller one.
    /// - A burst can lose lines: at most 64 wait for the main thread between two
    ///   frames, and one that does not fit is dropped. The console keeps the
    ///   whole record.
    /// - Whatever libvlc logged before the callback was attached is not here:
    ///   libvlc's own header warns that the messages from its initialisation cannot
    ///   be captured this way. Everything after that waits in a queue until a frame
    ///   drains it, however long that takes -- the lines of a media created before
    ///   the first player existed still arrive with that player's first frame.
    /// - This signal carries arguments. A handler written for a signal that
    ///   carried none is no longer called by Godot, which reports the argument
    ///   mismatch each time it is emitted.
    #[signal]
    fn log_message(level: i32, module: GString, message: GString);

    /// The LibVLC this addon is running against, as `"4.0.0-dev Otto Chriek"`.
    ///
    /// # Returns
    /// the version string libvlc was compiled with. It is a constant of the
    /// runtime, never empty.
    ///
    /// # Note
    /// - This is the runtime's own answer, which is the one a bug report needs:
    ///   the addon carries a pinned build, and [method get_changeset] names the
    ///   revision it came from.
    /// - It is a static call: `VLCInstance.get_version()` works before -- and
    ///   without -- the engine singleton, which is what an editor import or a
    ///   resource loader can rely on.
    /// - It does not need an instance and cannot fail.
    #[func]
    fn get_version() -> GString {
        GString::from(c_string(unsafe { vlc::libvlc_get_version() }).as_str())
    }

    /// The revision the runtime was built from, as libvlc reports it.
    ///
    /// # Returns
    /// libvlc's changeset string, never empty.
    ///
    /// # Note
    /// - This is `git describe --tags --long --match '?.*.*' --always` in the
    ///   build that produced the runtime -- `4.0.0-dev-37536-g546e18e53e` for the
    ///   pinned build, whose commit is `546e18e53e` inside that string. It is
    ///   **not** a bare commit hash: a build made outside a tagged checkout gets
    ///   the abbreviated hash alone, and the shipped one carries the tag with it.
    /// - The pinned revision is in `build/vlc/vlc.lock`, and the artifacts record
    ///   it as `build-info.txt`; this is the same answer from inside a running
    ///   game, where neither of those files can be read.
    /// - Static, like [method get_version], and never empty.
    #[func]
    fn get_changeset() -> GString {
        GString::from(c_string(unsafe { vlc::libvlc_get_changeset() }).as_str())
    }

    /// Sets how much of libvlc's log is reported, from this frame on.
    ///
    /// # Parameters
    /// - [param level] [constant LOG_LEVEL_DEBUG] through [constant LOG_LEVEL_DISABLED].
    ///   A value outside that range is clamped into it.
    ///
    /// # Note
    /// - It takes effect immediately, and it does **not** write `vlc/log_level`:
    ///   the project setting stays the value the instance was created with, and
    ///   this is the runtime override on top of it. The setting keeps its
    ///   `restart_if_changed`, so a project that wants the quieter level from the
    ///   first frame sets it there; a project that wants to see what went wrong
    ///   right now calls this.
    /// - Raising the level is the cheap direction: libvlc does the formatting and
    ///   this binding does the filtering, so a message that is not reported still
    ///   cost the call.
    /// - The console line and [signal log_message] both follow it; the lines that
    ///   were already parked for the next frame are not retroactively dropped.
    #[func]
    fn set_log_level(&self, level: i32) {
        self.log
            .level
            .store(level.clamp(0, LOG_LEVEL_DISABLED), Ordering::Relaxed);
    }

    /// The level in force right now.
    ///
    /// # Returns
    /// the rung [method set_log_level] last set, or the one `vlc/log_level` had
    /// when this instance was created.
    #[func]
    fn get_log_level(&self) -> i32 {
        self.log.level.load(Ordering::Relaxed)
    }

    /// The arguments this instance was created with, exactly as they were handed to
    /// libvlc.
    ///
    /// # Returns
    /// the list `vlc/arguments` held while the extension was loading, in order, plus
    /// the one default this binding adds on Android; empty when libvlc did not come
    /// up at all, which [method has_instance] reports.
    ///
    /// # Note
    /// - This is the instance's own copy, not the project setting: the setting is
    ///   read once, at creation, and editing it afterwards changes what the **next**
    ///   run gets. A report about what is playing now needs this one; what the
    ///   project asks for is `ProjectSettings.get_setting("vlc/arguments")`.
    /// - It is this binding's record, not libvlc's answer: `libvlc_new`'s arguments
    ///   are write-only, and no API reads an instance's options back.
    /// - On Android the list can hold an argument no one wrote: `--vout=vmem` is
    ///   appended when the project did not choose a video output, because that is
    ///   the only output that can produce a texture there.
    /// - The same list is printed once, when the instance is created, so a log
    ///   already answers this for the run it came from.
    #[func]
    fn get_arguments(&self) -> PackedStringArray {
        self.arguments.iter().cloned().collect()
    }

    /// Whether this extension has a LibVLC instance at all.
    ///
    /// # Returns
    /// `false` when `libvlc_new` failed while the extension was loading -- a missing
    /// runtime library or a data directory it could not find -- and `true` otherwise.
    ///
    /// # Note
    /// - The failure is already reported: it is logged once, with libvlc's own
    ///   reason, where the instance is created. This method exists so that a project
    ///   can check before calling something that would panic: with no instance,
    ///   every method that needs one fails with that same reason rather than
    ///   answering.
    /// - [method get_arguments] answers with an empty list in that state, because
    ///   nothing was started with any argument.
    #[func]
    fn has_instance(&self) -> bool {
        self.instance.is_some()
    }

    /// The clock behind every time value libvlc reports, in microseconds.
    ///
    /// # Returns
    /// the current time on libvlc's own clock: monotonic, in microseconds, with an
    /// arbitrary but system-wide origin (`CLOCK_MONOTONIC` where the platform has
    /// one). It never goes backwards.
    ///
    /// # Note
    /// - This is the clock the `system_date_us` of
    ///   [signal VLCMediaPlayer.time_point] is on, and the one
    ///   [method VLCMediaPlayer.interpolate_time_point] reads internally. A caller
    ///   that keeps an interpolated time and compares it with a later one needs this
    ///   to read the later one, and it is the only correct source: Godot's own
    ///   microsecond clock (`Time.get_ticks_usec`) starts at another origin, so a
    ///   value from one of them lands anywhere at all on this one.
    /// - Static, like [method get_version]: libvlc's clock exists before and
    ///   independently of any instance.
    #[func]
    fn get_clock_us() -> i64 {
        unsafe { vlc::libvlc_clock() }
    }

    pub fn get_vlc_instance(&self) -> *mut vlc::libvlc_instance_t {
        self.instance.expect(
            "libvlc_new failed, so this extension has no LibVLC instance; the reason is in the error it logged",
        )
    }

    /// Emits [signal log_message] for everything logged since the last frame.
    ///
    /// Called from a player's frame, on the main thread.
    fn emit_parked_logs(&mut self) {
        while let Some(log) = self.log.pop() {
            self.signals().log_message().emit(
                log.level,
                &GString::from(log.module.as_str()),
                &GString::from(log.message.as_str()),
            );
        }
    }

    /// libvlc's logging callback.
    ///
    /// This runs on **whatever thread logged the message**, holding no lock this
    /// binding can see, and libvlc only promises that the callback itself is
    /// thread-safe to call. Two things follow, and both are in the code below: the
    /// console line is written here -- where the message is, and as this binding
    /// has always done -- and everything a signal needs is copied out, because the
    /// context libvlc hands over, the format string and the varargs are all only
    /// valid until this returns.
    unsafe extern "C" fn log_callback_impl(
        data: *mut c_void,
        level: c_int,
        ctx: *const vlc::libvlc_log_t,
        fmt: *const c_char,
        args: *mut c_void,
    ) {
        unsafe {
            let Some(sink) = (data as *const LogSink).as_ref() else {
                return;
            };
            let Some(rung) = rung_of(level as vlc::libvlc_log_level) else {
                // A level this extension does not know: libvlc's enum skips 1, and
                // a future one could add a value.
                return;
            };
            let configured = sink.level.load(Ordering::Relaxed);
            if !reports(configured, rung) {
                return;
            }

            // The module name can point into a stack buffer of libvlc's own frame
            // (`vlc_vaLog` strips the directory and the extension into one), so
            // this is copied now or not at all -- and an empty or absent module is
            // both possible, which is why the formatter below tests for the string
            // and not for the pointer.
            let mut module: *const c_char = ptr::null();
            let mut file: *const c_char = ptr::null();
            let mut line: c_uint = 0;
            vlc::libvlc_log_get_context(ctx, &mut module, &mut file, &mut line);
            let module = c_string(module);
            let file = c_string(file);

            let message: String = printf(fmt, args);

            // Warnings and errors name where they came from, which is the one
            // thing a bug report cannot be written without; the two quiet rungs
            // stay short, because a debug line with a source location in it is
            // noise at the rate libvlc can emit them.
            let mut location = String::new();
            if !file.is_empty() && line != 0 && line != u32::MAX {
                location = format!("({file}:{line}) ");
            }
            // The signal carries the bare module name; the console line wraps it in
            // brackets, and leaves it out entirely when libvlc did not name one.
            let prefix = if module.is_empty() {
                String::new()
            } else {
                format!("[{module}] ")
            };

            match rung {
                0 => godot_print!("LibVLC: [DEBUG] {prefix}{message}"),
                1 => godot_print!("LibVLC: {prefix}{message}"),
                2 => godot_warn!("LibVLC: {prefix}{location}{message}"),
                _ => godot_error!("LibVLC: {prefix}{location}{message}"),
            }

            sink.push(ParkedLog {
                level: rung,
                module,
                message,
            });
        }
    }
}

/// The arguments this binding hands to `libvlc_new`, in order.
///
/// Split out from `VLCInstance::init` so that the rule can be tested without an
/// engine and without an instance, and so that [method VLCInstance.get_arguments],
/// the startup line and the call itself cannot disagree about what was handed over.
///
/// The project's list is passed through unchanged. Android has no window for LibVLC
/// to draw into, and the software callbacks are the only output that can produce a
/// texture, so `vmem` is what this extension always wants there. Registering those
/// callbacks already sets the media player's own `vout`, which outranks an instance
/// option; this default earns its place when that registration did not happen, where
/// it turns a silent "no video" into vmem's own "missing lock callback" error -- the
/// difference between a symptom and a reason. A project that named a video output
/// itself has made that choice and keeps it, so the default is appended only when
/// neither `--vout...` nor `:vout...` is there already.
///
/// `android` is a parameter rather than a `cfg` so that both answers can be tested
/// from one build; the caller passes `cfg!(target_os = "android")`. It works on
/// `String` rather than on `GString` for the same reason -- a `GString` allocates
/// through Godot's own interface, which a unit test has not started.
fn effective_arguments(configured: &[String], android: bool) -> Vec<String> {
    let mut arguments = configured.to_vec();
    let chosen_an_output = arguments
        .iter()
        .any(|argument| argument.starts_with("--vout") || argument.starts_with(":vout"));
    if android && !chosen_an_output {
        arguments.push(String::from("--vout=vmem"));
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    /// libvlc numbers its levels `DEBUG=0`, `NOTICE=2`, `WARNING=3`, `ERROR=4` -- it
    /// skips 1 -- while this extension counts rungs `0..4`. Every message is
    /// translated on the way through, and this is that translation: a value it does
    /// not know, and one that has not been invented yet, both fall through to
    /// `None` rather than being reported as something they are not.
    #[test]
    fn translates_the_levels_libvlc_uses_into_the_rungs_this_extension_uses() {
        assert_eq!(rung_of(vlc::libvlc_log_level_LIBVLC_DEBUG), Some(0));
        assert_eq!(rung_of(vlc::libvlc_log_level_LIBVLC_NOTICE), Some(1));
        assert_eq!(rung_of(vlc::libvlc_log_level_LIBVLC_WARNING), Some(2));
        assert_eq!(rung_of(vlc::libvlc_log_level_LIBVLC_ERROR), Some(3));
        // 1 is the level libvlc skips, and 5 is one a later version could add.
        assert_eq!(rung_of(1), None);
        assert_eq!(rung_of(5), None);
    }

    /// The configured value is a floor, and the highest one silences everything.
    #[test]
    fn reports_what_the_configured_rung_covers() {
        // Debug: everything, including the debug chatter.
        assert!(reports(0, 0) && reports(0, 1) && reports(0, 3));
        // Info: no debug, but notices, warnings and errors.
        assert!(!reports(1, 0) && reports(1, 1) && reports(1, 3));
        // Warning: warnings and errors.
        assert!(!reports(2, 1) && reports(2, 2) && reports(2, 3));
        // Error: errors alone.
        assert!(!reports(3, 2) && reports(3, 3));
        // Disabled: nothing, which is what makes it a rung above the others rather
        // than a fourth one.
        assert!(!reports(4, 0) && !reports(4, 3));
        // And a value below the range is the loudest state, which is what it has
        // always been here; the setter clamps before anything is stored.
        assert!(reports(-1, 0));
    }

    /// The project's list reaches libvlc as it was written: same entries, same order,
    /// and on the desktop nothing else at all.
    #[test]
    fn passes_the_configured_arguments_through_unchanged() {
        assert!(effective_arguments(&[], false).is_empty());

        let configured = [String::from("--no-video"), String::from("--verbose=2")];
        let arguments = effective_arguments(&configured, false);
        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0], "--no-video");
        assert_eq!(arguments[1], "--verbose=2");
    }

    /// Android appends its `vmem` default, and only when the project has not named a
    /// video output itself: naming one is the project's decision to make.
    #[test]
    fn adds_the_android_video_output_only_when_none_was_chosen() {
        let configured = [String::from("--no-audio")];
        let arguments = effective_arguments(&configured, true);
        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0], "--no-audio");
        assert_eq!(arguments[1], "--vout=vmem");

        for chosen in ["--vout=dummy", ":vout=gl", "--vout", ":vout"] {
            let configured = [String::from(chosen)];
            let arguments = effective_arguments(&configured, true);
            assert_eq!(arguments.len(), 1, "{chosen} should have been kept alone");
            assert_eq!(arguments[0], chosen);
        }
    }
}
