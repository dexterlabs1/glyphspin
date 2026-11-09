//! D3D11 + DirectComposition presentation. One fullscreen pixel-shader pass turns the cell texture into pixels.
//! The swapchain is FP16 scRGB (G10, P709) when the monitor is in HDR mode, 8-bit BGRA otherwise.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use super::super::Surface;
use glyphspin_core::engine::Consts;
use glyphspin_core::logf;
use windows::core::{Interface, Result};

static VS: &[u8] = include_bytes!("../../../shaders/vs.cso");
static PS: &[u8] = include_bytes!("../../../shaders/ps.cso");

pub struct Gpu {
    pub device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    factory: IDXGIFactory2,
    dcomp: IDCompositionDevice,
    target: Option<IDCompositionTarget>,
    visual: IDCompositionVisual,
    swap: Option<IDXGISwapChain1>,
    rtv: Option<ID3D11RenderTargetView>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    cells: Option<(ID3D11Texture2D, ID3D11ShaderResourceView, u32, u32)>,
    atlas: Option<ID3D11ShaderResourceView>,
    pub width: u32,
    pub height: u32,
    pub hdr: bool,
}

pub fn create_factory() -> Result<IDXGIFactory2> {
    unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
}

impl Gpu {
    pub fn new(factory: IDXGIFactory2, adapter: Option<&IDXGIAdapter1>) -> Result<Gpu> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1];
            // the adapter that drives the monitor first; the default hardware adapter as a fallback
            let mut attempts: Vec<(D3D_DRIVER_TYPE, Option<IDXGIAdapter>)> = Vec::new();
            if let Some(a) = adapter {
                attempts.push((D3D_DRIVER_TYPE_UNKNOWN, Some(a.cast()?)));
            }
            attempts.push((D3D_DRIVER_TYPE_HARDWARE, None));
            let mut last = Ok(());
            for (drv, ad) in attempts {
                for flags in [D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
                              D3D11_CREATE_DEVICE_BGRA_SUPPORT] {
                    last = D3D11CreateDevice(ad.as_ref(), drv, HMODULE::default(), flags, Some(&levels),
                                             D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx));
                    match &last {
                        Ok(()) => break,
                        Err(e) => logf!("D3D11CreateDevice({drv:?}, flags {:#x}): {e:?}", flags.0),
                    }
                }
                if last.is_ok() {
                    break;
                }
            }
            last?;
            let device: ID3D11Device = device.unwrap();
            let ctx = ctx.unwrap();
            let dxgi_dev: IDXGIDevice = device.cast()?;
            let _ = dxgi_dev.cast::<IDXGIDevice1>().map(|d| d.SetMaximumFrameLatency(1));
            let dcomp: IDCompositionDevice =
                DCompositionCreateDevice(&dxgi_dev).inspect_err(|e| logf!("DCompositionCreateDevice: {e:?}"))?;
            let visual = dcomp.CreateVisual()?;

            let mut vs = None;
            device.CreateVertexShader(VS, None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(PS, None, Some(&mut ps))?;
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Consts>().next_multiple_of(16) as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            device.CreateBuffer(&desc, None, Some(&mut cb))?;

            Ok(Gpu { device, ctx, factory, dcomp, target: None, visual, swap: None, rtv: None,
                     vs: vs.unwrap(), ps: ps.unwrap(), cb: cb.unwrap(), cells: None, atlas: None,
                     width: 0, height: 0, hdr: false })
        }
    }

    /// Bind the composition tree to `hwnd` (call again after the window is recreated).
    pub fn bind_window(&mut self, hwnd: HWND) -> Result<()> {
        unsafe {
            self.target = None;
            let target = self.dcomp.CreateTargetForHwnd(hwnd, true)?;
            target.SetRoot(&self.visual)?;
            self.target = Some(target);
            self.dcomp.Commit()?;
            Ok(())
        }
    }

    /// (Re)create the swapchain at `w`x`h`, FP16 scRGB when `hdr`.
    pub fn resize(&mut self, w: u32, h: u32, hdr: bool) -> Result<()> {
        unsafe {
            if self.swap.is_some() && self.width == w && self.height == h && self.hdr == hdr {
                return Ok(());
            }
            self.ctx.OMSetRenderTargets(None, None);
            self.rtv = None;
            self.swap = None;
            self.ctx.ClearState();
            self.ctx.Flush();
            let format = if hdr { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
                ..Default::default()
            };
            let swap = self.factory.CreateSwapChainForComposition(&self.device, &desc, None)?;
            if let Ok(s3) = swap.cast::<IDXGISwapChain3>() {
                let cs = if hdr { DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 } else { DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 };
                let _ = s3.SetColorSpace1(cs);
            }
            self.visual.SetContent(&swap)?;
            self.dcomp.Commit()?;
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut rtv = None;
            self.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            self.rtv = rtv;
            self.swap = Some(swap);
            self.width = w;
            self.height = h;
            self.hdr = hdr;
            Ok(())
        }
    }

    unsafe fn read_back(&self, swap: &IDXGISwapChain1) -> Result<(Vec<u8>, usize)> {
        unsafe {
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut st = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut st))?;
            let st = st.unwrap();
            self.ctx.CopyResource(&st, &back);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let bpp = if self.hdr { 8 } else { 4 };
            let mut v = vec![0u8; self.width as usize * self.height as usize * bpp];
            for y in 0..self.height as usize {
                std::ptr::copy_nonoverlapping((m.pData as *const u8).add(y * m.RowPitch as usize),
                                              v.as_mut_ptr().add(y * self.width as usize * bpp),
                                              self.width as usize * bpp);
            }
            self.ctx.Unmap(&st, 0);
            Ok((v, self.width as usize * bpp))
        }
    }

    /// Drop the window binding and the swapchain contents (before the view window is destroyed).
    pub fn unbind(&mut self) {
        unsafe {
            let _ = self.visual.SetContent(None::<&windows::core::IUnknown>);
            if let Some(t) = &self.target {
                let _ = t.SetRoot(None::<&IDCompositionVisual>);
            }
            let _ = self.dcomp.Commit();
            self.target = None;
            self.rtv = None;
            self.swap = None;
            self.ctx.ClearState();
            self.ctx.Flush();
            self.width = 0;
            self.height = 0;
        }
    }

    pub fn device_lost(&self) -> bool {
        unsafe { self.device.GetDeviceRemovedReason().is_err() }
    }
}

impl Surface for Gpu {
    type Error = windows::core::Error;

    /// Upload the glyph atlas (R8 coverage, `w`x`h`).
    fn set_atlas(&mut self, pixels: &[u8], w: u32, h: u32) -> Result<()> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr() as *const _, SysMemPitch: w, SysMemSlicePitch: 0 };
            let mut tex = None;
            self.device.CreateTexture2D(&desc, Some(&init), Some(&mut tex))?;
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex.unwrap(), None, Some(&mut srv))?;
            self.atlas = srv;
            Ok(())
        }
    }

    /// Upload the cell grid: 4 halfs per cell (glyph, level, tint, glow).
    fn upload_cells(&mut self, data: &[u16], cols: u32, rows: u32) -> Result<()> {
        unsafe {
            let need_new = match &self.cells {
                Some((_, _, c, r)) => *c != cols || *r != rows,
                None => true,
            };
            if need_new {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: cols,
                    Height: rows,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                };
                let mut tex = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
                let tex = tex.unwrap();
                let mut srv = None;
                self.device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
                self.cells = Some((tex, srv.unwrap(), cols, rows));
            }
            let (tex, _, _, _) = self.cells.as_ref().unwrap();
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(tex, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))?;
            let row_bytes = cols as usize * 8;
            for y in 0..rows as usize {
                let src = &data[y * cols as usize * 4..(y + 1) * cols as usize * 4];
                std::ptr::copy_nonoverlapping(src.as_ptr() as *const u8, (m.pData as *mut u8).add(y * m.RowPitch as usize),
                                              row_bytes);
            }
            self.ctx.Unmap(tex, 0);
            Ok(())
        }
    }

    /// Draw one frame. When `readback` is set, the frame is also copied to the CPU before presenting and returned as
    /// raw texels (8 bytes per pixel for FP16, 4 for BGRA8) with the row pitch.
    fn draw(&mut self, consts: &Consts, readback: bool) -> Result<Option<(Vec<u8>, usize)>> {
        unsafe {
            let (Some(rtv), Some(swap)) = (self.rtv.clone(), self.swap.clone()) else { return Ok(None) };
            self.ctx.UpdateSubresource(&self.cb, 0, None, consts as *const Consts as *const _, 0, 0);
            self.ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            let vp = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: self.width as f32, Height: self.height as f32,
                                      MinDepth: 0.0, MaxDepth: 1.0 };
            self.ctx.RSSetViewports(Some(&[vp]));
            self.ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.IASetInputLayout(None);
            self.ctx.VSSetShader(&self.vs, None);
            self.ctx.PSSetShader(&self.ps, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            let cells = self.cells.as_ref().map(|c| c.1.clone());
            self.ctx.PSSetShaderResources(0, Some(&[cells, self.atlas.clone()]));
            self.ctx.Draw(3, 0);

            let mut out = None;
            if readback {
                out = Some(self.read_back(&swap)?);
            }
            let hr = swap.Present(1, DXGI_PRESENT(0));
            hr.ok()?;
            Ok(out)
        }
    }

    fn ready(&self) -> bool {
        self.rtv.is_some()
    }
}
