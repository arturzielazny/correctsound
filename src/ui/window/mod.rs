//! The app in a window of its own (`--window`): the same screens and keys as in the
//! terminal, drawn on the GPU. ratatui lays the screen out in cells as always; the text and
//! the charts are then drawn by vello into one frame, straight from the app, with no
//! terminal to send images to.

pub mod fonts;
pub mod gpu;
pub mod grid;
pub mod keys;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use vello::Scene;
use vello::kurbo::Affine;
use vello::wgpu;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::app::App;
use crate::ui::graphics::Graphics;
use crate::ui::theme::Theme;
use fonts::{Fonts, Metrics};
use gpu::Gpu;

/// Text size, in points, before zooming (Ctrl with +, − or 0).
pub const TEXT_PT: f32 = 10.0;
/// How often the app looks at the audio and its background work, as in the terminal.
const TICK: Duration = Duration::from_millis(50);

/// The screen as the app draws it, ready for the GPU: the text grid's scene and the
/// charts', each made again only when it changes.
pub struct Screen {
    terminal: Terminal<TestBackend>,
    pub gfx: Graphics,
    fonts: Fonts,
    theme: Theme,
    metrics: Metrics,
    /// Size in pixels.
    size: (u32, u32),
    /// The text grid last drawn and its scene.
    grid: Option<(Buffer, Scene)>,
    /// The charts last drawn: where, in cells, and their scenes.
    charts: Vec<(Rect, Arc<Scene>)>,
}

impl Screen {
    /// A screen `size` pixels large, with text `pt` points large at display `scale`.
    ///
    /// # Errors
    ///
    /// When no monospace font is found.
    pub fn new(theme: Theme, size: (u32, u32), pt: f32, scale: f32) -> Result<Self> {
        let fonts = Fonts::load().context("no monospace font found")?;
        let metrics = fonts.metrics(pt, scale);
        let gfx = Graphics::window(theme.clone(), Some(fonts.regular_bytes()), metrics.cell());
        let (cols, rows) = grid_size(size, &metrics);
        let terminal = Terminal::new(TestBackend::new(cols, rows))
            .map_err(|e| anyhow!("laying out the screen: {e}"))?;
        Ok(Self {
            terminal,
            gfx,
            fonts,
            theme,
            metrics,
            size,
            grid: None,
            charts: Vec::new(),
        })
    }

    /// The screen is now `size` pixels large.
    pub fn resize(&mut self, size: (u32, u32)) {
        self.size = size;
        let (cols, rows) = grid_size(size, &self.metrics);
        self.terminal.backend_mut().resize(cols, rows);
        self.grid = None;
    }

    /// Text is now `pt` points large at display `scale`.
    pub fn set_text_size(&mut self, pt: f32, scale: f32) {
        self.metrics = self.fonts.metrics(pt, scale);
        self.gfx.set_cell_size(self.metrics.cell());
        self.resize(self.size);
    }

    /// Size in pixels.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Columns and rows.
    #[must_use]
    pub fn cells(&self) -> (u16, u16) {
        grid_size(self.size, &self.metrics)
    }

    /// The theme's background, behind everything.
    #[must_use]
    pub fn background(&self) -> crate::ui::theme::Rgb {
        self.theme.background
    }

    /// Lays the app's screen out again; true when what it shows changed.
    ///
    /// # Errors
    ///
    /// Never in practice: drawing into memory does not fail.
    pub fn update(&mut self, app: &App) -> Result<bool> {
        self.gfx.begin_frame();
        let gfx = &mut self.gfx;
        self.terminal
            .draw(|f| crate::ui::draw(f, app, gfx))
            .map_err(|e| anyhow!("laying out the screen: {e}"))?;
        let charts = self.gfx.take_placed();
        let buffer = self.terminal.backend().buffer();
        // A symbol's font found in the background is drawn as soon as it arrives.
        let fonts_arrived = self.fonts.collect();
        let grid_changed = fonts_arrived || self.grid.as_ref().is_none_or(|(b, _)| b != buffer);
        if grid_changed {
            let scene = grid::scene(buffer, &self.theme, &mut self.fonts, &self.metrics);
            self.grid = Some((buffer.clone(), scene));
        }
        let charts_changed = charts.len() != self.charts.len()
            || charts
                .iter()
                .zip(&self.charts)
                .any(|((a, s), (b, t))| a != b || !Arc::ptr_eq(s, t));
        self.charts = charts;
        Ok(grid_changed || charts_changed)
    }

    /// The whole frame: the text, and the charts over their cells.
    #[must_use]
    pub fn scene(&self) -> Scene {
        let mut scene = Scene::new();
        if let Some((_, grid)) = &self.grid {
            scene.append(grid, None);
        }
        let (cw, ch) = (
            f64::from(self.metrics.cell_w),
            f64::from(self.metrics.cell_h),
        );
        for (area, chart) in &self.charts {
            let at = Affine::translate((f64::from(area.x) * cw, f64::from(area.y) * ch));
            scene.append(chart, Some(at));
        }
        scene
    }
}

/// The columns and rows of cells `m` large that fit in `size` pixels.
fn grid_size(size: (u32, u32), m: &Metrics) -> (u16, u16) {
    let fit = |px: u32, cell: f32| ((px as f32 / cell) as u32).clamp(1, u32::from(u16::MAX)) as u16;
    (fit(size.0, m.cell_w), fit(size.1, m.cell_h))
}

/// The window could not be opened at all (no display, or no GPU to draw with), so nothing
/// was shown in it yet: the terminal can stand in.
#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub struct Unavailable(anyhow::Error);

/// Runs the app in a window until it quits, the window is closed or `signalled` is set.
///
/// # Errors
///
/// When the window or the GPU cannot be started ([`Unavailable`]), or stop working.
pub fn run(app: &mut App, signalled: Arc<AtomicBool>) -> Result<()> {
    let event_loop = EventLoop::new()
        .context("opening a window")
        .map_err(Unavailable)?;
    let mut runner = Runner {
        app,
        signalled,
        live: None,
        mods: ModifiersState::empty(),
        pt: TEXT_PT,
        next_tick: Instant::now(),
        keyed: false,
        error: None,
    };
    event_loop
        .run_app(&mut runner)
        .context("running the window")?;
    match runner.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The window and what draws into it.
struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    blitter: wgpu::util::TextureBlitter,
    screen: Screen,
}

struct Runner<'a> {
    app: &'a mut App,
    signalled: Arc<AtomicBool>,
    live: Option<Live>,
    mods: ModifiersState,
    /// Text size in points.
    pt: f32,
    next_tick: Instant,
    /// A key was handled since the last tick.
    keyed: bool,
    error: Option<anyhow::Error>,
}

impl Live {
    fn open(event_loop: &ActiveEventLoop, pt: f32) -> Result<Self> {
        let attributes = Window::default_attributes()
            .with_title("CorrectSound")
            .with_inner_size(LogicalSize::new(1440.0, 900.0));
        // The app id window managers know it by (for window rules in Hyprland and others):
        // the one exception to keeping OS names out of shared code here, as AGENTS.md says.
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        ))]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name("correctsound", "correctsound")
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .context("opening a window")?,
        );
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
                Box::new(event_loop.owned_display_handle()),
            ));
        let surface = instance
            .create_surface(Arc::clone(&window))
            .context("drawing into the window")?;
        let gpu = Gpu::new(&instance, Some(&surface))?;
        let caps = surface.get_capabilities(&gpu.adapter);
        // vello's colours are already sRGB: a format that would encode them again is not
        // wanted.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .or_else(|| caps.formats.first().copied())
            .context("the window takes no picture format the GPU draws")?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: Vec::new(),
        };
        surface.configure(&gpu.device, &config);
        let blitter = wgpu::util::TextureBlitter::new(&gpu.device, format);
        let screen = Screen::new(
            Theme::load(),
            (config.width, config.height),
            pt,
            window.scale_factor() as f32,
        )?;
        Ok(Self {
            window,
            surface,
            config,
            gpu,
            blitter,
            screen,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.screen.resize((size.width, size.height));
    }

    /// Draws the screen into the window.
    fn present(&mut self) -> Result<()> {
        let scene = self.screen.scene();
        let background = self.screen.background();
        self.gpu
            .render(&scene, (self.config.width, self.config.height), background)?;
        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            // Hidden, or busy: the next frame comes when it is shown again.
            _ => return Ok(()),
        };
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        if let Some(frame) = self.gpu.frame() {
            self.blitter
                .copy(&self.gpu.device, &mut encoder, frame, &view);
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.gpu.queue.present(texture);
        Ok(())
    }
}

impl Runner<'_> {
    fn fail(&mut self, event_loop: &ActiveEventLoop, e: anyhow::Error) {
        self.error = Some(e);
        event_loop.exit();
    }
}

impl ApplicationHandler for Runner<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        match Live::open(event_loop, self.pt) {
            Ok(live) => {
                live.window.request_redraw();
                self.live = Some(live);
            }
            Err(e) => self.fail(event_loop, Unavailable(e).into()),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(live) = &mut self.live else { return };
        match event {
            WindowEvent::CloseRequested => self.app.quit = true,
            WindowEvent::Resized(size) => {
                live.resize(size);
                live.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                live.screen.set_text_size(self.pt, scale_factor as f32);
                live.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            // Synthetic presses come with focus, for keys already held: not the user's.
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => {
                if let Some(zoom) = keys::zoom(&event.logical_key, self.mods) {
                    self.pt = match zoom {
                        keys::Zoom::In => (self.pt + 1.0).min(40.0),
                        keys::Zoom::Out => (self.pt - 1.0).max(5.0),
                        keys::Zoom::Reset => TEXT_PT,
                    };
                    live.screen
                        .set_text_size(self.pt, live.window.scale_factor() as f32);
                } else if let Some(key) = keys::convert(&event.logical_key, self.mods) {
                    self.app.handle_key(key);
                    self.keyed = true;
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = live.present() {
                    self.fail(event_loop, e);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.signalled.load(Ordering::Relaxed) {
            self.app.quit = true;
        }
        if self.app.quit {
            event_loop.exit();
            return;
        }
        // Every key that came in is handled before the screen is drawn again, as in the
        // terminal; the app's background work is looked at right after them.
        let now = Instant::now();
        if self.keyed || now >= self.next_tick {
            self.app.tick();
            self.keyed = false;
            self.next_tick = now + TICK;
        }
        if let Some(live) = &mut self.live {
            match live.screen.update(self.app) {
                Ok(true) => live.window.request_redraw(),
                Ok(false) => {}
                Err(e) => return self.fail(event_loop, e),
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }
}
