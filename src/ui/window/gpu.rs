//! The GPU: vello draws each frame's scene into a texture, which is copied to the window
//! (or read back, for tests and screenshots).

use anyhow::{Context, Result, anyhow};
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

use crate::ui::raster::gpu::color;
use crate::ui::theme::Rgb;

/// A GPU device and vello on it, with the texture frames are drawn into.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    renderer: Renderer,
    /// The frame texture (vello draws only into storage textures, which windows are not):
    /// its view and size.
    target: Option<(wgpu::Texture, wgpu::TextureView)>,
    pub adapter: wgpu::Adapter,
}

impl Gpu {
    /// The best GPU, able to draw into `surface` when there is one.
    ///
    /// # Errors
    ///
    /// When there is no GPU vello can use, or it fails to start.
    pub fn new(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface,
            ..wgpu::RequestAdapterOptions::default()
        }))
        .context("no GPU found")?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .context("starting the GPU")?;
        let renderer = Renderer::new(
            &device,
            RendererOptions {
                antialiasing_support: AaSupport::area_only(),
                ..RendererOptions::default()
            },
        )
        .map_err(|e| anyhow!("starting vello: {e}"))?;
        Ok(Self {
            device,
            queue,
            renderer,
            target: None,
            adapter,
        })
    }

    /// A GPU without a window: for tests and screenshots.
    ///
    /// # Errors
    ///
    /// When there is no GPU vello can use.
    pub fn headless() -> Result<Self> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        Self::new(&instance, None)
    }

    /// The GPU's name and how it is driven, for reports.
    #[must_use]
    pub fn name(&self) -> String {
        let info = self.adapter.get_info();
        format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type)
    }

    /// True for a renderer in software, standing in where there is no GPU (as on build
    /// servers).
    #[must_use]
    pub fn is_software(&self) -> bool {
        self.adapter.get_info().device_type == wgpu::DeviceType::Cpu
    }

    /// The last frame drawn.
    #[must_use]
    pub fn frame(&self) -> Option<&wgpu::TextureView> {
        self.target.as_ref().map(|(_, v)| v)
    }

    /// Draws `scene` over `background` into the frame texture, `size` pixels large.
    ///
    /// # Errors
    ///
    /// When vello fails to draw.
    pub fn render(&mut self, scene: &Scene, size: (u32, u32), background: Rgb) -> Result<()> {
        let (w, h) = (size.0.max(1), size.1.max(1));
        if self
            .target
            .as_ref()
            .is_none_or(|(t, _)| (t.width(), t.height()) != (w, h))
        {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("frame"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.target = Some((texture, view));
        }
        let (_, view) = self
            .target
            .as_ref()
            .expect("the frame texture was just made");
        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                scene,
                view,
                &RenderParams {
                    base_color: color(background, 1.0),
                    width: w,
                    height: h,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| anyhow!("drawing the frame: {e}"))
    }

    /// Waits until the GPU has finished what it was given.
    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// The last frame drawn, as an image.
    ///
    /// # Errors
    ///
    /// When nothing was drawn yet, or the GPU fails to hand it over.
    pub fn read_back(&self) -> Result<image::RgbaImage> {
        let (texture, _) = self.target.as_ref().context("no frame drawn yet")?;
        let (w, h) = (texture.width(), texture.height());
        let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: u64::from(row) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.wait();
        let data = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| anyhow!("reading the frame: {e:?}"))?;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let start = y * row as usize;
            pixels.extend_from_slice(&data[start..start + w as usize * 4]);
        }
        image::RgbaImage::from_raw(w, h, pixels).context("frame size")
    }
}
