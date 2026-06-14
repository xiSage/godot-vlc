//! `libvlc_video_engine_d3d11` output callbacks. The `Backend` owns the
//! D3D11 device + immediate context + shared fence + current output
//! texture; the opaque pointer libvlc threads through every callback is
//! `Arc::into_raw(backend)` and `cleanup_cb` reclaims it via `Arc::from_raw`.
//! All callbacks run on libvlc's render thread.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
    ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource1;
use windows::core::Interface;

use crate::vlc::{
    libvlc_media_player_t, libvlc_video_color_primaries_t_libvlc_video_primaries_BT709,
    libvlc_video_color_space_t_libvlc_video_colorspace_BT709,
    libvlc_video_engine_t_libvlc_video_engine_d3d11,
    libvlc_video_orient_t_libvlc_video_orient_top_left, libvlc_video_output_cfg_t,
    libvlc_video_render_cfg_t, libvlc_video_set_output_callbacks, libvlc_video_setup_device_cfg_t,
    libvlc_video_setup_device_info_t, libvlc_video_transfer_func_t_libvlc_video_transfer_func_SRGB,
};

use super::event_queue::{EventMailbox, OutputEvent};
use super::shared_texture::SharedFence;

/// `DXGI_FORMAT_R8G8B8A8_UNORM` as a raw `c_int` for libvlc's output cfg
/// (`windows-rs` types it as `DXGI_FORMAT(28)`).
const DXGI_FORMAT_R8G8B8A8_UNORM_RAW: i32 = 28;

/// State shared between libvlc's render thread (callbacks) and the Godot
/// render-thread importer (per-frame copy). Both sides hold `Arc<Backend>`.
pub struct Backend {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub fence: SharedFence,
    pub mailbox: Arc<EventMailbox>,
    pub(crate) current: Mutex<Option<CurrentOutput>>,
    pub update_output_calls: AtomicU64,
    pub swap_calls: AtomicU64,
    pub make_current_calls: AtomicU64,
}

unsafe impl Send for Backend {}
unsafe impl Sync for Backend {}

/// Both fields are read only by libvlc through the COM pointers; we hold
/// them so the underlying objects stay alive for the output's lifetime.
#[allow(dead_code)]
pub(crate) struct CurrentOutput {
    texture: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
}

#[derive(Debug)]
pub enum CallbackError {
    CreateTexture(String),
    QueryDxgiResource(String),
    CreateSharedHandle(String),
    CreateRtv(String),
    CreateFence(super::shared_texture::SharedError),
    SetOutputCallbacksFailed,
}

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateTexture(s) => write!(f, "godot-vlc: CreateTexture2D failed: {s}"),
            Self::QueryDxgiResource(s) => {
                write!(f, "godot-vlc: cast to IDXGIResource1 failed: {s}")
            }
            Self::CreateSharedHandle(s) => {
                write!(f, "godot-vlc: CreateSharedHandle failed: {s}")
            }
            Self::CreateRtv(s) => write!(f, "godot-vlc: CreateRenderTargetView failed: {s}"),
            Self::CreateFence(e) => write!(f, "{e}"),
            Self::SetOutputCallbacksFailed => {
                write!(
                    f,
                    "godot-vlc: libvlc_video_set_output_callbacks returned false"
                )
            }
        }
    }
}

impl std::error::Error for CallbackError {}

impl Backend {
    pub fn new(
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        mailbox: Arc<EventMailbox>,
    ) -> Result<Self, CallbackError> {
        let fence = SharedFence::create(&device).map_err(CallbackError::CreateFence)?;
        Ok(Self {
            device,
            context,
            fence,
            mailbox,
            current: Mutex::new(None),
            update_output_calls: AtomicU64::new(0),
            swap_calls: AtomicU64::new(0),
            make_current_calls: AtomicU64::new(0),
        })
    }

    /// Shared NT handle of the producer fence; the importer opens it once
    /// on its D3D12 device to get an `ID3D12Fence`.
    pub fn fence_shared_handle(&self) -> HANDLE {
        self.fence.shared_handle
    }

    /// Allocate a new shared D3D11 texture + RTV, mint a fresh NT handle,
    /// push the handle to the mailbox, and install as the current output.
    /// Called from `update_output_cb` on libvlc's render thread.
    fn rebuild_current_output(&self, width: u32, height: u32) -> Result<(), CallbackError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            CPUAccessFlags: 0,
            // SHARED_NTHANDLE must be paired with SHARED or SHARED_KEYEDMUTEX;
            // SHARED + shared fence is enough for our cross-API sync.
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED).0 as u32,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(|e| CallbackError::CreateTexture(e.message()))?;
        let texture =
            texture.ok_or_else(|| CallbackError::CreateTexture("null texture out-param".into()))?;

        let dxgi_resource: IDXGIResource1 = texture
            .cast()
            .map_err(|e| CallbackError::QueryDxgiResource(e.message()))?;
        let handle = unsafe { dxgi_resource.CreateSharedHandle(None, GENERIC_ALL.0, None) }
            .map_err(|e| CallbackError::CreateSharedHandle(e.message()))?;

        let mut rtv: Option<ID3D11RenderTargetView> = None;
        let resource: ID3D11Resource = texture
            .cast()
            .map_err(|e| CallbackError::QueryDxgiResource(e.message()))?;
        unsafe {
            self.device
                .CreateRenderTargetView(&resource, None, Some(&mut rtv))
        }
        .map_err(|e| CallbackError::CreateRtv(e.message()))?;
        let rtv = rtv.ok_or_else(|| CallbackError::CreateRtv("null RTV out-param".into()))?;

        // Pre-bind the RTV for this output's lifetime, matching libvlc's
        // own d3d11_player.cpp example: `select_plane_cb` leaves *output
        // NULL so libvlc reuses what we bound here.
        let rtvs = [Some(rtv.clone())];
        unsafe {
            self.context.OMSetRenderTargets(Some(&rtvs), None);
        }

        self.mailbox.push(OutputEvent {
            handle,
            width,
            height,
        });

        let mut current = self.current.lock().expect("backend.current poisoned");
        *current = Some(CurrentOutput { texture, rtv });
        Ok(())
    }
}

/// Register `Backend` with libvlc as the GPU output. Consumes one
/// `Arc<Backend>` clone for libvlc's opaque pointer; `cleanup_cb` reclaims
/// it. Caller keeps its own clone for the importer side.
///
/// Returns `Err` if `libvlc_video_set_output_callbacks` rejects the engine
/// or callback set. On error the consumed Arc is reclaimed via `from_raw`
/// before returning so the refcount stays correct.
pub fn register(
    player: *mut libvlc_media_player_t,
    backend: Arc<Backend>,
) -> Result<(), CallbackError> {
    let raw = Arc::into_raw(backend) as *mut c_void;
    let ok = unsafe {
        libvlc_video_set_output_callbacks(
            player,
            libvlc_video_engine_t_libvlc_video_engine_d3d11,
            Some(setup_cb),
            Some(cleanup_cb),
            None, // window_cb — unused
            Some(update_output_cb),
            Some(swap_cb),
            Some(make_current_cb),
            None, // getProcAddress_cb — OpenGL only
            None, // metadata_cb — HDR only, not used in v1
            Some(select_plane_cb),
            raw,
        )
    };
    if !ok {
        unsafe {
            let _ = Arc::from_raw(raw as *const Backend);
        };
        return Err(CallbackError::SetOutputCallbacksFailed);
    }
    Ok(())
}

unsafe fn backend_ref<'a>(opaque: *mut c_void) -> &'a Backend {
    unsafe { &*(opaque as *const Backend) }
}

unsafe extern "C" fn setup_cb(
    opaque: *mut *mut c_void,
    _cfg: *const libvlc_video_setup_device_cfg_t,
    out: *mut libvlc_video_setup_device_info_t,
) -> bool {
    unsafe {
        if opaque.is_null() || (*opaque).is_null() || out.is_null() {
            return false;
        }
        let backend = backend_ref(*opaque);
        // context_mutex stays NULL: the immediate context is never touched
        // outside libvlc's own callbacks.
        (*out).__bindgen_anon_1.d3d11.device_context = backend.context.as_raw();
        (*out).__bindgen_anon_1.d3d11.context_mutex = std::ptr::null_mut();
        true
    }
}

unsafe extern "C" fn cleanup_cb(opaque: *mut c_void) {
    unsafe {
        if opaque.is_null() {
            return;
        }
        let _ = Arc::from_raw(opaque as *const Backend);
    }
}

unsafe extern "C" fn update_output_cb(
    opaque: *mut c_void,
    cfg: *const libvlc_video_render_cfg_t,
    output: *mut libvlc_video_output_cfg_t,
) -> bool {
    unsafe {
        if opaque.is_null() || cfg.is_null() || output.is_null() {
            return false;
        }
        let backend = backend_ref(opaque);
        backend.update_output_calls.fetch_add(1, Ordering::SeqCst);
        let width = (*cfg).width;
        let height = (*cfg).height;
        if width == 0 || height == 0 {
            return false;
        }
        if let Err(e) = backend.rebuild_current_output(width, height) {
            // godot_error! requires the main thread; we're on libvlc's. eprintln
            // surfaces on the _console binary's stderr.
            eprintln!("godot-vlc: update_output_cb rebuild failed: {e}");
            return false;
        }

        // RGBA8 full-range BT.709 sRGB-transfer top-left, matching libvlc's own
        // d3d11_player.cpp example.
        (*output).__bindgen_anon_1.dxgi_format = DXGI_FORMAT_R8G8B8A8_UNORM_RAW;
        (*output).full_range = true;
        (*output).colorspace = libvlc_video_color_space_t_libvlc_video_colorspace_BT709;
        (*output).primaries = libvlc_video_color_primaries_t_libvlc_video_primaries_BT709;
        (*output).transfer = libvlc_video_transfer_func_t_libvlc_video_transfer_func_SRGB;
        (*output).orientation = libvlc_video_orient_t_libvlc_video_orient_top_left;
        true
    }
}

unsafe extern "C" fn swap_cb(opaque: *mut c_void) {
    unsafe {
        if opaque.is_null() {
            return;
        }
        let backend = backend_ref(opaque);
        backend.swap_calls.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = backend.fence.signal_next(&backend.context) {
            eprintln!("godot-vlc: swap_cb fence signal failed: {e}");
        }
    }
}

unsafe extern "C" fn make_current_cb(opaque: *mut c_void, _enter: bool) -> bool {
    unsafe {
        // Stub: RTV pre-bound in update_output_cb. Cannot be NULL — libvlc
        // requires it for the d3d11 engine.
        if !opaque.is_null() {
            backend_ref(opaque)
                .make_current_calls
                .fetch_add(1, Ordering::SeqCst);
        }
        true
    }
}

unsafe extern "C" fn select_plane_cb(
    _opaque: *mut c_void,
    plane: usize,
    _output: *mut c_void,
) -> bool {
    // Returning true with *output left unmodified makes libvlc reuse the
    // RTV bound by update_output_cb. RGBA8 is single-plane.
    plane == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxgi_format_constant_matches_windows_rs() {
        assert_eq!(DXGI_FORMAT_R8G8B8A8_UNORM_RAW, DXGI_FORMAT_R8G8B8A8_UNORM.0);
    }
}
