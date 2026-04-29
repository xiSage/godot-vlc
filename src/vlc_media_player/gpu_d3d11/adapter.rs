//! Adapter LUID matching (Godot's D3D12 device → matching DXGI adapter)
//! and `D3D11CreateDevice` on that adapter for the GPU backend.

use std::ffi::c_void;
use std::fmt;

use godot::classes::rendering_device::DriverResource;
use godot::classes::RenderingServer;
use godot::prelude::*;

use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};

#[derive(Debug)]
pub enum AdapterError {
    NotD3D12(String),
    NoRenderingDevice,
    NoLogicalDevice,
    DxgiFactory(String),
    LuidNotFound(i64),
    D3D11CreateFailed(String),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotD3D12(api) => write!(
                f,
                "godot-vlc: GPU backend requires --rendering-driver d3d12 (rendering API: {api})"
            ),
            Self::NoRenderingDevice => {
                write!(f, "godot-vlc: RenderingServer has no RenderingDevice")
            }
            Self::NoLogicalDevice => write!(
                f,
                "godot-vlc: RenderingDevice did not yield a D3D12 logical device handle"
            ),
            Self::DxgiFactory(s) => write!(f, "godot-vlc: CreateDXGIFactory1 failed: {s}"),
            Self::LuidNotFound(luid) => {
                write!(f, "godot-vlc: no DXGI adapter matched LUID {luid:#x}")
            }
            Self::D3D11CreateFailed(s) => write!(f, "godot-vlc: D3D11CreateDevice failed: {s}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Pack a Win32 `LUID` into a signed 64-bit value for stable comparison.
pub fn luid_to_i64(luid: LUID) -> i64 {
    (((luid.HighPart as u32 as u64) << 32) | (luid.LowPart as u64)) as i64
}

/// Abstraction over adapter enumeration so `find_adapter_by_luid` is unit
/// testable without a live DXGI factory.
pub trait AdapterEnumerator {
    type Adapter;
    fn enumerate(&self) -> Vec<(i64, Self::Adapter)>;
}

pub fn find_adapter_by_luid<E: AdapterEnumerator>(
    enumerator: &E,
    target_luid: i64,
) -> Option<E::Adapter> {
    enumerator
        .enumerate()
        .into_iter()
        .find_map(|(luid, a)| if luid == target_luid { Some(a) } else { None })
}

pub struct DxgiAdapterEnumerator;

impl AdapterEnumerator for DxgiAdapterEnumerator {
    type Adapter = IDXGIAdapter1;

    fn enumerate(&self) -> Vec<(i64, IDXGIAdapter1)> {
        unsafe {
            let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
                Ok(f) => f,
                Err(_) => return Vec::new(),
            };
            let mut out = Vec::new();
            let mut idx = 0u32;
            while let Ok(adapter) = factory.EnumAdapters1(idx) {
                if let Ok(desc) = adapter.GetDesc1() {
                    out.push((luid_to_i64(desc.AdapterLuid), adapter));
                }
                idx += 1;
            }
            out
        }
    }
}

/// True when Godot's video adapter API version looks like a D3D12 feature
/// level (`<digits>_<digits>`, e.g. `12_0`). Vulkan reports `X.Y.Z`,
/// OpenGL reports text, dummy reports empty.
pub fn is_d3d12_api_string(api: &str) -> bool {
    let mut parts = api.split('_');
    let lhs = parts.next();
    let rhs = parts.next();
    if parts.next().is_some() {
        return false;
    }
    matches!(
        (lhs, rhs),
        (Some(a), Some(b)) if !a.is_empty()
            && !b.is_empty()
            && a.chars().all(|c| c.is_ascii_digit())
            && b.chars().all(|c| c.is_ascii_digit())
    )
}

/// LUID of Godot's D3D12 logical device. Errors if the rendering driver
/// isn't D3D12 or no rendering device is available.
pub fn godot_d3d12_luid() -> Result<i64, AdapterError> {
    let rs = RenderingServer::singleton();
    let api = rs.get_video_adapter_api_version().to_string();
    if !is_d3d12_api_string(&api) {
        return Err(AdapterError::NotD3D12(api));
    }

    let mut rd = rs
        .get_rendering_device()
        .ok_or(AdapterError::NoRenderingDevice)?;

    let handle = rd.get_driver_resource(DriverResource::LOGICAL_DEVICE, Rid::Invalid, 0);
    if handle == 0 {
        return Err(AdapterError::NoLogicalDevice);
    }

    let raw = handle as *mut c_void;
    let device = unsafe {
        ID3D12Device::from_raw_borrowed(&raw).ok_or(AdapterError::NoLogicalDevice)?
    };
    let luid = unsafe { device.GetAdapterLuid() };
    Ok(luid_to_i64(luid))
}

/// Find the DXGI adapter matching `target_luid` and return its
/// self-reported LUID. Used to round-trip-verify the Godot ↔ DXGI match.
pub fn dxgi_adapter_luid_for(target_luid: i64) -> Result<i64, AdapterError> {
    let enumerator = DxgiAdapterEnumerator;
    let adapter = find_adapter_by_luid(&enumerator, target_luid)
        .ok_or(AdapterError::LuidNotFound(target_luid))?;
    let desc = unsafe { adapter.GetDesc1().map_err(|e| AdapterError::DxgiFactory(e.message())) }?;
    Ok(luid_to_i64(desc.AdapterLuid))
}

/// D3D11 device + immediate context paired together.
pub struct CreatedD3D11 {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

/// Create a D3D11 device on `adapter` with the flags libvlc's D3D11 output
/// engine and hardware-decode pipeline need (`BGRA_SUPPORT | VIDEO_SUPPORT`,
/// plus thread protection enabled on the device's immediate context).
pub fn create_d3d11_device(adapter: &IDXGIAdapter1) -> Result<CreatedD3D11, AdapterError> {
    let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    if std::env::var_os("GODOT_VLC_D3D11_DEBUG").is_some() {
        flags |= windows::Win32::Graphics::Direct3D11::D3D11_CREATE_DEVICE_DEBUG;
    }

    let hr = unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            flags,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    };
    hr.map_err(|e| AdapterError::D3D11CreateFailed(e.message()))?;

    let device = device.ok_or_else(|| {
        AdapterError::D3D11CreateFailed("D3D11CreateDevice succeeded but device is null".into())
    })?;
    let context = context.ok_or_else(|| {
        AdapterError::D3D11CreateFailed("D3D11CreateDevice succeeded but context is null".into())
    })?;
    // libvlc's d3d11 output engine requires multithread-protected device.
    let multithread: windows::Win32::Graphics::Direct3D11::ID3D11Multithread = device
        .cast()
        .map_err(|e| AdapterError::D3D11CreateFailed(format!("ID3D11Multithread cast: {}", e.message())))?;
    unsafe {
        let _ = multithread.SetMultithreadProtected(true);
    }
    Ok(CreatedD3D11 { device, context })
}

/// Full chain: read Godot's D3D12 device LUID, find the matching DXGI
/// adapter, and create a D3D11 device on it.
pub fn create_d3d11_device_for_godot() -> Result<(IDXGIAdapter1, CreatedD3D11), AdapterError> {
    let luid = godot_d3d12_luid()?;
    let adapter = find_adapter_by_luid(&DxgiAdapterEnumerator, luid)
        .ok_or(AdapterError::LuidNotFound(luid))?;
    let d3d11 = create_d3d11_device(&adapter)?;
    Ok((adapter, d3d11))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEnumerator(Vec<(i64, &'static str)>);
    impl AdapterEnumerator for FakeEnumerator {
        type Adapter = &'static str;
        fn enumerate(&self) -> Vec<(i64, &'static str)> {
            self.0.clone()
        }
    }

    #[test]
    fn match_first() {
        let e = FakeEnumerator(vec![(0x1234, "first"), (0x5678, "second")]);
        assert_eq!(find_adapter_by_luid(&e, 0x1234), Some("first"));
    }

    #[test]
    fn match_last() {
        let e = FakeEnumerator(vec![(0x1234, "first"), (0x5678, "last")]);
        assert_eq!(find_adapter_by_luid(&e, 0x5678), Some("last"));
    }

    #[test]
    fn no_match() {
        let e = FakeEnumerator(vec![(0x1234, "a"), (0x5678, "b")]);
        assert_eq!(find_adapter_by_luid(&e, 0x9999), None);
    }

    #[test]
    fn duplicate_luid_returns_first() {
        let e = FakeEnumerator(vec![(0x1234, "first"), (0x1234, "second")]);
        assert_eq!(find_adapter_by_luid(&e, 0x1234), Some("first"));
    }

    #[test]
    fn empty_enumerator() {
        let e = FakeEnumerator(vec![]);
        assert_eq!(find_adapter_by_luid(&e, 0x1234), None);
    }

    #[test]
    fn d3d12_api_string_accepts_feature_levels() {
        assert!(is_d3d12_api_string("12_0"));
        assert!(is_d3d12_api_string("12_1"));
        assert!(is_d3d12_api_string("11_0"));
    }

    #[test]
    fn d3d12_api_string_rejects_other_drivers() {
        assert!(!is_d3d12_api_string(""));
        assert!(!is_d3d12_api_string("1.3.250"));
        assert!(!is_d3d12_api_string("OpenGL ES 3.0"));
        assert!(!is_d3d12_api_string("12_0_0"));
        assert!(!is_d3d12_api_string("12_"));
        assert!(!is_d3d12_api_string("_0"));
        assert!(!is_d3d12_api_string("ab_cd"));
    }

    #[test]
    fn luid_pack_roundtrip() {
        let luid = LUID {
            LowPart: 0xDEAD_BEEF,
            HighPart: 0x1234_5678,
        };
        let packed = luid_to_i64(luid);
        assert_eq!(packed as u64 & 0xFFFF_FFFF, 0xDEAD_BEEF);
        assert_eq!((packed as u64) >> 32, 0x1234_5678);
    }
}
