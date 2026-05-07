//! `ID3D11Fence` shareable as an NT handle. D3D11 signals after writes,
//! D3D12 waits before sampling, monotonic value tracked in an atomic.

use std::sync::atomic::{AtomicU64, Ordering};

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence,
    D3D11_FENCE_FLAG_SHARED,
};

#[derive(Debug)]
pub enum SharedError {
    CreateSharedHandle(String),
    Device5Cast(String),
    DeviceContext4Cast(String),
    CreateFence(String),
    SignalFence(String),
}

impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateSharedHandle(s) => write!(f, "godot-vlc: CreateSharedHandle failed: {s}"),
            Self::Device5Cast(s) => write!(f, "godot-vlc: cast to ID3D11Device5 failed: {s}"),
            Self::DeviceContext4Cast(s) => {
                write!(f, "godot-vlc: cast to ID3D11DeviceContext4 failed: {s}")
            }
            Self::CreateFence(s) => write!(f, "godot-vlc: ID3D11Device5::CreateFence failed: {s}"),
            Self::SignalFence(s) => write!(f, "godot-vlc: ID3D11DeviceContext4::Signal failed: {s}"),
        }
    }
}

impl std::error::Error for SharedError {}

pub struct SharedFence {
    pub fence: ID3D11Fence,
    pub shared_handle: HANDLE,
    pub signaled_value: AtomicU64,
}

impl SharedFence {
    pub fn create(device: &ID3D11Device) -> Result<Self, SharedError> {
        let device5: ID3D11Device5 = device
            .cast()
            .map_err(|e| SharedError::Device5Cast(e.message()))?;
        let mut fence: Option<ID3D11Fence> = None;
        unsafe { device5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
            .map_err(|e| SharedError::CreateFence(e.message()))?;
        let fence = fence
            .ok_or_else(|| SharedError::CreateFence("null fence out-param".into()))?;
        let handle = unsafe { fence.CreateSharedHandle(None, GENERIC_ALL.0, None) }
            .map_err(|e| SharedError::CreateSharedHandle(e.message()))?;
        Ok(Self {
            fence,
            shared_handle: handle,
            signaled_value: AtomicU64::new(0),
        })
    }

    /// Bump and signal the next fence value. D3D12 side waits on this.
    pub fn signal_next(&self, ctx: &ID3D11DeviceContext) -> Result<u64, SharedError> {
        let ctx4: ID3D11DeviceContext4 = ctx
            .cast()
            .map_err(|e| SharedError::DeviceContext4Cast(e.message()))?;
        let next = self.signaled_value.fetch_add(1, Ordering::SeqCst) + 1;
        unsafe { ctx4.Signal(&self.fence, next) }
            .map_err(|e| SharedError::SignalFence(e.message()))?;
        Ok(next)
    }

    pub fn current(&self) -> u64 {
        self.signaled_value.load(Ordering::SeqCst)
    }
}

impl Drop for SharedFence {
    fn drop(&mut self) {
        if !self.shared_handle.is_invalid() {
            unsafe { _ = CloseHandle(self.shared_handle) };
        }
    }
}
