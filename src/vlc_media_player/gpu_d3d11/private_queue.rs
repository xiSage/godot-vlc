//! Private D3D12 command queue running the per-frame
//! `Wait(imported_fence) → CopyResource → Signal(done_fence) → CPU-block`
//! pipeline. Godot's main command queue is private in `RenderingDevice`
//! with no GDExtension accessor, so we own a separate queue on the same
//! `ID3D12Device` and CPU-block before Godot samples the destination.

use std::sync::atomic::{AtomicU64, Ordering};

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandQueue, ID3D12Device, ID3D12Fence,
    ID3D12GraphicsCommandList, ID3D12Resource, D3D12_COMMAND_LIST_TYPE_DIRECT,
    D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_COMMAND_QUEUE_PRIORITY_NORMAL,
    D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST,
    D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_TRANSITION_BARRIER,
    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

#[derive(Debug)]
pub enum PrivateQueueError {
    CreateCommandQueue(String),
    CreateCommandAllocator(String),
    CreateCommandList(String),
    CreateFence(String),
    CreateEvent(String),
    ResetAllocator(String),
    ResetList(String),
    CloseList(String),
    SignalFence(String),
    WaitFence(String),
    SetEventOnCompletion(String),
    EventWaitFailed(u32),
}

impl std::fmt::Display for PrivateQueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use PrivateQueueError::*;
        match self {
            CreateCommandQueue(s) => write!(f, "godot-vlc: CreateCommandQueue failed: {s}"),
            CreateCommandAllocator(s) => {
                write!(f, "godot-vlc: CreateCommandAllocator failed: {s}")
            }
            CreateCommandList(s) => write!(f, "godot-vlc: CreateCommandList failed: {s}"),
            CreateFence(s) => write!(f, "godot-vlc: ID3D12Device::CreateFence failed: {s}"),
            CreateEvent(s) => write!(f, "godot-vlc: CreateEventW failed: {s}"),
            ResetAllocator(s) => {
                write!(f, "godot-vlc: ID3D12CommandAllocator::Reset failed: {s}")
            }
            ResetList(s) => write!(f, "godot-vlc: ID3D12GraphicsCommandList::Reset failed: {s}"),
            CloseList(s) => write!(f, "godot-vlc: ID3D12GraphicsCommandList::Close failed: {s}"),
            SignalFence(s) => write!(f, "godot-vlc: ID3D12CommandQueue::Signal failed: {s}"),
            WaitFence(s) => write!(f, "godot-vlc: ID3D12CommandQueue::Wait failed: {s}"),
            SetEventOnCompletion(s) => {
                write!(f, "godot-vlc: ID3D12Fence::SetEventOnCompletion failed: {s}")
            }
            EventWaitFailed(code) => {
                write!(f, "godot-vlc: WaitForSingleObject returned {code:#x}")
            }
        }
    }
}

impl std::error::Error for PrivateQueueError {}

/// Direct command queue + allocator + list + completion fence + CPU wait
/// event on Godot's `ID3D12Device`. Owns its own fence values so it can't
/// race Godot's main queue.
pub struct PrivateQueue {
    queue: ID3D12CommandQueue,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    done_fence: ID3D12Fence,
    done_event: HANDLE,
    next_value: AtomicU64,
}

unsafe impl Send for PrivateQueue {}
unsafe impl Sync for PrivateQueue {}

impl PrivateQueue {
    pub fn create(device: &ID3D12Device) -> Result<Self, PrivateQueueError> {
        let queue_desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        };
        let queue: ID3D12CommandQueue = unsafe { device.CreateCommandQueue(&queue_desc) }
            .map_err(|e| PrivateQueueError::CreateCommandQueue(e.message()))?;

        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT) }
                .map_err(|e| PrivateQueueError::CreateCommandAllocator(e.message()))?;

        let list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
        }
        .map_err(|e| PrivateQueueError::CreateCommandList(e.message()))?;
        unsafe { list.Close() }.map_err(|e| PrivateQueueError::CloseList(e.message()))?;

        let done_fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
            .map_err(|e| PrivateQueueError::CreateFence(e.message()))?;

        let done_event = unsafe { CreateEventW(None, false, false, None) }
            .map_err(|e| PrivateQueueError::CreateEvent(e.message()))?;

        Ok(Self {
            queue,
            allocator,
            list,
            done_fence,
            done_event,
            next_value: AtomicU64::new(0),
        })
    }

    /// Wait `imported_fence` for `imported_value`, `CopyResource` src → dst,
    /// signal our completion fence, then CPU-block until the copy lands.
    /// Caller can safely sample `dst` after this returns.
    pub fn copy_and_sync(
        &self,
        imported_fence: &ID3D12Fence,
        imported_value: u64,
        src: &ID3D12Resource,
        dst: &ID3D12Resource,
    ) -> Result<(), PrivateQueueError> {
        unsafe {
            self.allocator
                .Reset()
                .map_err(|e| PrivateQueueError::ResetAllocator(e.message()))?;
            self.list
                .Reset(&self.allocator, None)
                .map_err(|e| PrivateQueueError::ResetList(e.message()))?;
            // Round-trip COMMON ↔ COPY_SOURCE/COPY_DEST so the destination is
            // back in COMMON when Godot's render graph next transitions it.
            // Without this, Godot believes the texture is still in COMMON
            // while the hardware is in COPY_DEST, the implicit transition on
            // sample is invalid, and the GPU returns zeros.
            let pre_barriers = [
                transition_barrier(src, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE),
                transition_barrier(dst, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST),
            ];
            self.list.ResourceBarrier(&pre_barriers);
            self.list.CopyResource(dst, src);
            let post_barriers = [
                transition_barrier(src, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON),
                transition_barrier(dst, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON),
            ];
            self.list.ResourceBarrier(&post_barriers);
            self.list
                .Close()
                .map_err(|e| PrivateQueueError::CloseList(e.message()))?;

            self.queue
                .Wait(imported_fence, imported_value)
                .map_err(|e| PrivateQueueError::WaitFence(e.message()))?;

            let list_for_exec: ID3D12GraphicsCommandList = self.list.clone();
            let cmd_lists = [Some(list_for_exec.cast().unwrap())];
            self.queue.ExecuteCommandLists(&cmd_lists);

            let frame_value = self.next_value.fetch_add(1, Ordering::SeqCst) + 1;
            self.queue
                .Signal(&self.done_fence, frame_value)
                .map_err(|e| PrivateQueueError::SignalFence(e.message()))?;

            if self.done_fence.GetCompletedValue() < frame_value {
                self.done_fence
                    .SetEventOnCompletion(frame_value, self.done_event)
                    .map_err(|e| PrivateQueueError::SetEventOnCompletion(e.message()))?;
                let res = WaitForSingleObject(self.done_event, INFINITE);
                if res != WAIT_OBJECT_0 {
                    return Err(PrivateQueueError::EventWaitFailed(res.0));
                }
            }
        }
        Ok(())
    }
}

fn transition_barrier(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: std::mem::ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

impl Drop for PrivateQueue {
    fn drop(&mut self) {
        if !self.done_event.is_invalid() {
            unsafe { _ = CloseHandle(self.done_event) };
        }
    }
}
