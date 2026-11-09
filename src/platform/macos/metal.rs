//! Metal presentation. One fullscreen fragment pass (shaders/glyph.metal) turns the cell texture into pixels on a
//! CAMetalLayer per screen. On EDR-capable screens the layer is RGBA16Float in extended linear sRGB (1.0 = SDR
//! white, values above it use the screen's EDR headroom); otherwise 8-bit BGRA with the sRGB encode in the shader,
//! exactly like the Windows SDR path.

use super::super::Surface;
use glyphspin_core::engine::Consts;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CGSize;
use objc2_core_graphics::{CGColorSpace, kCGColorSpaceExtendedLinearSRGB, kCGColorSpaceSRGB};
use objc2_foundation::NSString;
use objc2_metal::*;
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use std::ptr::NonNull;
use std::rc::Rc;

static SHADER: &str = include_str!("../../../shaders/glyph.metal");

type Dev = ProtocolObject<dyn MTLDevice>;

/// Device, queue and the two pipelines, shared by every screen.
pub struct Gpu {
    pub device: Retained<Dev>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipe_sdr: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pipe_hdr: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub name: String,
}

impl Gpu {
    pub fn new() -> Result<Rc<Gpu>, String> {
        let device = MTLCreateSystemDefaultDevice().ok_or("no Metal device")?;
        let queue = device.newCommandQueue().ok_or("no command queue")?;
        let lib = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADER), None)
            .map_err(|e| format!("shader compile: {}", e.localizedDescription()))?;
        let vs = lib.newFunctionWithName(&NSString::from_str("vs_main")).ok_or("no vs_main")?;
        let ps = lib.newFunctionWithName(&NSString::from_str("ps_main")).ok_or("no ps_main")?;
        let pipe = |fmt: MTLPixelFormat| -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>, String> {
            let d = MTLRenderPipelineDescriptor::new();
            d.setVertexFunction(Some(&vs));
            d.setFragmentFunction(Some(&ps));
            unsafe { d.colorAttachments().objectAtIndexedSubscript(0) }.setPixelFormat(fmt);
            device.newRenderPipelineStateWithDescriptor_error(&d)
                .map_err(|e| format!("pipeline: {}", e.localizedDescription()))
        };
        let pipe_sdr = pipe(MTLPixelFormat::BGRA8Unorm)?;
        let pipe_hdr = pipe(MTLPixelFormat::RGBA16Float)?;
        let name = device.name().to_string();
        Ok(Rc::new(Gpu { device, queue, pipe_sdr, pipe_hdr, name }))
    }
}

/// One screen's layer and textures.
pub struct MetalSurface {
    gpu: Rc<Gpu>,
    pub layer: Retained<CAMetalLayer>,
    cells: Option<(Retained<ProtocolObject<dyn MTLTexture>>, u32, u32)>,
    atlas: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    pub width: u32,
    pub height: u32,
    pub hdr: bool,
}

impl MetalSurface {
    pub fn new(gpu: Rc<Gpu>) -> MetalSurface {
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&gpu.device));
        layer.setFramebufferOnly(false); // the readback (--dump) blits from the drawable
        layer.setMaximumDrawableCount(2);
        layer.setOpaque(true);
        MetalSurface { gpu, layer, cells: None, atlas: None, width: 0, height: 0, hdr: false }
    }

    /// Size the drawable to `w`x`h` pixels (`scale` = backing scale factor) and pick the format.
    pub fn resize(&mut self, w: u32, h: u32, scale: f64, hdr: bool) {
        self.layer.setContentsScale(scale);
        self.layer.setDrawableSize(CGSize { width: w as f64, height: h as f64 });
        if hdr {
            self.layer.setPixelFormat(MTLPixelFormat::RGBA16Float);
            self.layer.setWantsExtendedDynamicRangeContent(true);
            let cs = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearSRGB }));
            self.layer.setColorspace(cs.as_deref());
        } else {
            self.layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            self.layer.setWantsExtendedDynamicRangeContent(false);
            let cs = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
            self.layer.setColorspace(cs.as_deref());
        }
        self.width = w;
        self.height = h;
        self.hdr = hdr;
    }

    fn texture(&self, fmt: MTLPixelFormat, w: u32, h: u32) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
        let d = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(fmt, w as usize,
                                                                                            h as usize, false)
        };
        d.setUsage(MTLTextureUsage::ShaderRead);
        d.setStorageMode(MTLStorageMode::Shared);
        self.gpu.device.newTextureWithDescriptor(&d).ok_or_else(|| "texture".to_string())
    }
}

fn region(w: u32, h: u32) -> MTLRegion {
    MTLRegion {
        origin: MTLOrigin { x: 0, y: 0, z: 0 },
        size: MTLSize { width: w as usize, height: h as usize, depth: 1 },
    }
}

impl Surface for MetalSurface {
    type Error = String;

    fn set_atlas(&mut self, pixels: &[u8], w: u32, h: u32) -> Result<(), String> {
        let t = self.texture(MTLPixelFormat::R8Unorm, w, h)?;
        unsafe {
            t.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region(w, h), 0,
                                                             NonNull::new(pixels.as_ptr() as *mut _).unwrap(),
                                                             w as usize);
        }
        self.atlas = Some(t);
        Ok(())
    }

    fn upload_cells(&mut self, data: &[u16], cols: u32, rows: u32) -> Result<(), String> {
        let need_new = match &self.cells {
            Some((_, c, r)) => *c != cols || *r != rows,
            None => true,
        };
        if need_new {
            self.cells = Some((self.texture(MTLPixelFormat::RGBA16Float, cols, rows)?, cols, rows));
        }
        let (t, _, _) = self.cells.as_ref().unwrap();
        unsafe {
            t.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region(cols, rows), 0,
                                                             NonNull::new(data.as_ptr() as *mut _).unwrap(),
                                                             cols as usize * 8);
        }
        Ok(())
    }

    fn draw(&mut self, consts: &Consts, readback: bool) -> Result<Option<(Vec<u8>, usize)>, String> {
        let (Some((cells, _, _)), Some(atlas)) = (&self.cells, &self.atlas) else { return Ok(None) };
        if self.width == 0 {
            return Ok(None);
        }
        let Some(drawable) = self.layer.nextDrawable() else { return Err("no drawable".into()) };
        let target = drawable.texture();
        let cb = self.gpu.queue.commandBuffer().ok_or("no command buffer")?;
        let pass = MTLRenderPassDescriptor::new();
        let ca = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        ca.setTexture(Some(&target));
        ca.setLoadAction(MTLLoadAction::DontCare);
        ca.setStoreAction(MTLStoreAction::Store);
        let enc = cb.renderCommandEncoderWithDescriptor(&pass).ok_or("no encoder")?;
        enc.setRenderPipelineState(if self.hdr { &self.gpu.pipe_hdr } else { &self.gpu.pipe_sdr });
        unsafe {
            enc.setFragmentBytes_length_atIndex(NonNull::new(consts as *const Consts as *mut _).unwrap(),
                                                std::mem::size_of::<Consts>(), 0);
            enc.setFragmentTexture_atIndex(Some(cells), 0);
            enc.setFragmentTexture_atIndex(Some(atlas), 1);
            enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        enc.endEncoding();
        let bpp = if self.hdr { 8 } else { 4 };
        let pitch = self.width as usize * bpp;
        let buf = if readback {
            let len = pitch * self.height as usize;
            let buf = self.gpu.device.newBufferWithLength_options(len, MTLResourceOptions::StorageModeShared)
                .ok_or("no readback buffer")?;
            let blit = cb.blitCommandEncoder().ok_or("no blit encoder")?;
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &target, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width: self.width as usize, height: self.height as usize, depth: 1 }, &buf, 0, pitch,
                    len);
            }
            blit.endEncoding();
            Some((buf, len))
        } else {
            None
        };
        let d: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
        cb.presentDrawable(d);
        cb.commit();
        if let Some((buf, len)) = buf {
            cb.waitUntilCompleted();
            let src = unsafe { std::slice::from_raw_parts(buf.contents().as_ptr() as *const u8, len) };
            // BGRA8 is what the dump decoder expects for SDR; RGBA16F matches the FP16 layout
            return Ok(Some((src.to_vec(), pitch)));
        }
        Ok(None)
    }

    fn ready(&self) -> bool {
        self.width > 0 && self.cells.is_some() && self.atlas.is_some()
    }
}
