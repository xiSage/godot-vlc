//! Per-frame import + GPU-copy orchestrator. Runs on the Godot render
//! thread (scheduled from `frame_pre_draw` via `call_on_render_thread`).
//! Drains the mailbox for resize events, opens the new D3D11 shared
//! texture on Godot's D3D12 device, allocates a Godot-RD destination, and
//! runs `PrivateQueue::copy_and_sync` for the frame.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use godot::classes::rendering_device::{DataFormat, TextureSamples, TextureType, TextureUsageBits};
use godot::classes::{RdTextureFormat, RdTextureView, RenderingServer, Texture2Drd};
use godot::obj::NewGd;
use godot::prelude::*;

use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};

use super::output_callbacks::Backend;
use super::private_queue::PrivateQueue;
use super::rd_import::{
    ImportError, godot_d3d12_device, godot_rd_texture_native, open_shared_fence,
    open_shared_texture,
};

#[derive(Debug)]
pub enum ImporterError {
    Import(ImportError),
    PrivateQueue(super::private_queue::PrivateQueueError),
    AllocateRdDest,
}

impl std::fmt::Display for ImporterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Import(e) => write!(f, "{e}"),
            Self::PrivateQueue(e) => write!(f, "{e}"),
            Self::AllocateRdDest => write!(
                f,
                "godot-vlc: RenderingDevice::texture_create returned invalid Rid"
            ),
        }
    }
}

impl From<ImportError> for ImporterError {
    fn from(e: ImportError) -> Self {
        Self::Import(e)
    }
}
impl From<super::private_queue::PrivateQueueError> for ImporterError {
    fn from(e: super::private_queue::PrivateQueueError) -> Self {
        Self::PrivateQueue(e)
    }
}

/// The currently imported D3D11 shared texture (as an `ID3D12Resource`)
/// paired with the Godot-RD destination it copies into.
struct CurrentImport {
    d3d12_src: ID3D12Resource,
    dst_rid: Rid,
    d3d12_dst: ID3D12Resource,
}

/// Owned by `VLCMediaPlayer` and captured by the per-frame Callable. The
/// player disconnects the signal on Drop, releasing the captured Arc.
pub struct ImporterTask {
    backend: Arc<Backend>,
    d3d12_device: ID3D12Device,
    d3d12_fence: ID3D12Fence,
    private_queue: PrivateQueue,
    /// Bound to the Godot-RD destination's Rid. Sampled by Godot.
    pub texture_2drd: Mutex<Gd<Texture2Drd>>,
    current: Mutex<Option<CurrentImport>>,
    pub frames_copied: AtomicU64,
}

// SAFETY: The COM interfaces are reference-counted and thread-safe at the
// API level. Mutex covers the mutable state. Gd<Texture2Drd> requires the
// godot crate's experimental-threads feature, which is enabled in Cargo.toml.
unsafe impl Send for ImporterTask {}
unsafe impl Sync for ImporterTask {}

impl ImporterTask {
    /// RID of the current GPU destination, if any.
    pub fn current_dst_rid(&self) -> Option<Rid> {
        self.current
            .lock()
            .expect("importer current poisoned")
            .as_ref()
            .map(|c| c.dst_rid)
    }

    pub fn create(backend: Arc<Backend>) -> Result<Arc<Self>, ImporterError> {
        let d3d12_device = godot_d3d12_device()?;
        let d3d12_fence = open_shared_fence(&d3d12_device, backend.fence_shared_handle())?;
        let private_queue = PrivateQueue::create(&d3d12_device)?;
        let texture_2drd = Texture2Drd::new_gd();
        Ok(Arc::new(Self {
            backend,
            d3d12_device,
            d3d12_fence,
            private_queue,
            texture_2drd: Mutex::new(texture_2drd),
            current: Mutex::new(None),
            frames_copied: AtomicU64::new(0),
        }))
    }
}

/// One frame's work on the Godot render thread. Drains the mailbox to
/// pick up resize events, then copies the latest fence value into the
/// destination.
pub fn run_frame(task: &ImporterTask) {
    if let Err(e) = try_run_frame(task) {
        eprintln!("godot-vlc: importer run_frame failed: {e}");
    }
}

fn try_run_frame(task: &ImporterTask) -> Result<(), ImporterError> {
    if let Some(event) = task.backend.mailbox.take() {
        rebuild_current(task, event.handle, event.width, event.height)?;
    }

    let current_lock = task.current.lock().expect("importer current poisoned");
    let Some(current) = current_lock.as_ref() else {
        return Ok(());
    };
    let latest = task.backend.fence.current();
    if latest == 0 {
        // libvlc hasn't signaled a frame yet; waiting on 0 would block forever.
        return Ok(());
    }
    task.private_queue.copy_and_sync(
        &task.d3d12_fence,
        latest,
        &current.d3d12_src,
        &current.d3d12_dst,
    )?;
    task.frames_copied.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

fn rebuild_current(
    task: &ImporterTask,
    handle: windows::Win32::Foundation::HANDLE,
    width: u32,
    height: u32,
) -> Result<(), ImporterError> {
    let d3d12_src = open_shared_texture(&task.d3d12_device, handle)?;
    let new_rid = create_sampled_texture(width, height).ok_or(ImporterError::AllocateRdDest)?;
    let d3d12_dst = godot_rd_texture_native(new_rid)?;

    task.texture_2drd
        .lock()
        .expect("texture_2drd poisoned")
        .set_texture_rd_rid(new_rid);

    let mut slot = task.current.lock().expect("importer current poisoned");
    if let Some(old) = slot.take() {
        free_rid(old.dst_rid);
    }
    *slot = Some(CurrentImport {
        d3d12_src,
        dst_rid: new_rid,
        d3d12_dst,
    });
    Ok(())
}

fn create_sampled_texture(width: u32, height: u32) -> Option<Rid> {
    let mut rd = RenderingServer::singleton().get_rendering_device()?;
    let mut format = RdTextureFormat::new_gd();
    format.set_format(DataFormat::R8G8B8A8_UNORM);
    format.set_width(width);
    format.set_height(height);
    format.set_depth(1);
    format.set_array_layers(1);
    format.set_mipmaps(1);
    format.set_texture_type(TextureType::TYPE_2D);
    format.set_samples(TextureSamples::SAMPLES_1);
    let usage = TextureUsageBits::SAMPLING_BIT
        | TextureUsageBits::CAN_COPY_TO_BIT
        | TextureUsageBits::CAN_COPY_FROM_BIT;
    format.set_usage_bits(usage);
    let view = RdTextureView::new_gd();
    let rid = rd.texture_create(&format, &view);
    if !rid.is_valid() {
        return None;
    }
    Some(rid)
}

fn free_rid(rid: Rid) {
    if !rid.is_valid() {
        return;
    }
    if let Some(mut rd) = RenderingServer::singleton().get_rendering_device() {
        rd.free_rid(rid);
    }
}

impl Drop for ImporterTask {
    fn drop(&mut self) {
        if let Some(current) = self
            .current
            .lock()
            .expect("importer current poisoned")
            .take()
        {
            free_rid(current.dst_rid);
        }
    }
}
