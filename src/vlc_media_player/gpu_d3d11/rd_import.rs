//! Open shared NT handles on Godot's D3D12 device and recover the
//! underlying `ID3D12Resource*` from an RD-allocated texture.

use std::ffi::c_void;

use godot::classes::rendering_device::DriverResource;
use godot::classes::RenderingServer;
use godot::prelude::*;

use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};

#[derive(Debug)]
pub enum ImportError {
    NoRenderingDevice,
    NoLogicalDevice,
    OpenSharedHandle(String),
    NoNativeTextureHandle,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRenderingDevice => write!(f, "godot-vlc: no Godot RenderingDevice"),
            Self::NoLogicalDevice => write!(
                f,
                "godot-vlc: Godot returned no D3D12 logical device handle"
            ),
            Self::OpenSharedHandle(s) => {
                write!(f, "godot-vlc: D3D12 OpenSharedHandle failed: {s}")
            }
            Self::NoNativeTextureHandle => write!(
                f,
                "godot-vlc: get_driver_resource(TEXTURE_VIEW) returned 0 (RID invalid?)"
            ),
        }
    }
}

impl std::error::Error for ImportError {}

/// Godot's `ID3D12Device`, AddRef'd. Caller's Drop will Release.
pub fn godot_d3d12_device() -> Result<ID3D12Device, ImportError> {
    let rd = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or(ImportError::NoRenderingDevice)?;
    let raw_handle = rd.get_driver_resource(DriverResource::LOGICAL_DEVICE, Rid::Invalid, 0);
    if raw_handle == 0 {
        return Err(ImportError::NoLogicalDevice);
    }
    let raw = raw_handle as *mut c_void;
    unsafe {
        let borrowed =
            ID3D12Device::from_raw_borrowed(&raw).ok_or(ImportError::NoLogicalDevice)?;
        Ok(borrowed.clone())
    }
}

pub fn open_shared_texture(
    device: &ID3D12Device,
    handle: HANDLE,
) -> Result<ID3D12Resource, ImportError> {
    let mut resource: Option<ID3D12Resource> = None;
    unsafe { device.OpenSharedHandle(handle, &mut resource) }
        .map_err(|e| ImportError::OpenSharedHandle(e.message()))?;
    resource.ok_or_else(|| ImportError::OpenSharedHandle("null resource out-param".into()))
}

pub fn open_shared_fence(
    device: &ID3D12Device,
    handle: HANDLE,
) -> Result<ID3D12Fence, ImportError> {
    let mut fence: Option<ID3D12Fence> = None;
    unsafe { device.OpenSharedHandle(handle, &mut fence) }
        .map_err(|e| ImportError::OpenSharedHandle(e.message()))?;
    fence.ok_or_else(|| ImportError::OpenSharedHandle("null fence out-param".into()))
}

/// Native `ID3D12Resource*` for a Godot-allocated RD texture, AddRef'd.
///
/// Uses `DRIVER_RESOURCE_TEXTURE_VIEW`, not `TEXTURE`: on Godot 4.5.2's
/// D3D12 driver `TEXTURE` returns `tex_info->main_texture`, which is null
/// for plain `texture_create()` allocations (only set for sliced/shared
/// views). `TEXTURE_VIEW` returns `tex_info->resource` and works on both
/// 4.5.2 and 4.7+.
pub fn godot_rd_texture_native(rid: Rid) -> Result<ID3D12Resource, ImportError> {
    let rd = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or(ImportError::NoRenderingDevice)?;
    let raw_handle = rd.get_driver_resource(DriverResource::TEXTURE_VIEW, rid, 0);
    if raw_handle == 0 {
        return Err(ImportError::NoNativeTextureHandle);
    }
    let raw = raw_handle as *mut c_void;
    unsafe {
        let borrowed =
            ID3D12Resource::from_raw_borrowed(&raw).ok_or(ImportError::NoNativeTextureHandle)?;
        Ok(borrowed.clone())
    }
}
