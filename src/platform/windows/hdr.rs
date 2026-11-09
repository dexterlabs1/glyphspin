//! Per-monitor HDR state: is the output in HDR (BT.2100 PQ) mode, its peak luminance, and the user's SDR white
//! level (Settings > Display > HDR > "SDR content brightness").

use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

#[derive(Clone, Debug)]
pub struct DisplayInfo {
    pub device_name: String,
    pub hdr_active: bool,
    pub color_space: i32,
    pub max_nits: f32,
    pub max_full_frame_nits: f32,
    pub sdr_white_nits: f32,
    pub bits_per_color: u32,
}

impl Default for DisplayInfo {
    fn default() -> Self {
        DisplayInfo { device_name: String::new(), hdr_active: false, color_space: 0, max_nits: 80.0,
                      max_full_frame_nits: 80.0, sdr_white_nits: 80.0, bits_per_color: 8 }
    }
}

/// Find the adapter + output that drive the monitor with GDI name `device` ("\\.\DISPLAY1").
pub fn find_output(factory: &IDXGIFactory1, device: &str) -> Option<(IDXGIAdapter1, IDXGIOutput6)> {
    unsafe {
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                if let Ok(desc) = output.GetDesc() {
                    let name = String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0').to_string();
                    if name.eq_ignore_ascii_case(device) {
                        if let Ok(o6) = output.cast::<IDXGIOutput6>() {
                            return Some((adapter, o6));
                        }
                    }
                }
                oi += 1;
            }
            ai += 1;
        }
        None
    }
}

pub fn query(output: Option<&IDXGIOutput6>, device: &str) -> DisplayInfo {
    let mut info = DisplayInfo { device_name: device.to_string(), ..Default::default() };
    if let Some(o) = output {
        if let Ok(d) = unsafe { o.GetDesc1() } {
            info.color_space = d.ColorSpace.0;
            info.hdr_active = d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
            info.max_nits = d.MaxLuminance;
            info.max_full_frame_nits = d.MaxFullFrameLuminance;
            info.bits_per_color = d.BitsPerColor;
        }
    }
    info.sdr_white_nits = sdr_white_level(device).unwrap_or(if info.hdr_active { 200.0 } else { 80.0 });
    if !info.hdr_active {
        info.sdr_white_nits = 80.0;
    }
    info
}

/// SDR white level in nits for the display whose GDI source name is `device`.
fn sdr_white_level(device: &str) -> Option<f32> {
    unsafe {
        let mut np = 0u32;
        let mut nm = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
            != ERROR_SUCCESS
        {
            return None;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let name = String::from_utf16_lossy(&src.viewGdiDeviceName).trim_end_matches('\0').to_string();
            if !name.eq_ignore_ascii_case(device) {
                continue;
            }
            let mut wl = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
            wl.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
            wl.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
            wl.header.adapterId = p.targetInfo.adapterId;
            wl.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut wl.header) == 0 {
                return Some(wl.SDRWhiteLevel as f32 / 1000.0 * 80.0);
            }
        }
        None
    }
}
