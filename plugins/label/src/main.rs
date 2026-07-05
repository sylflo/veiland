// SPDX-License-Identifier: GPL-3.0-or-later

//! Demo plugin — renders a single styled text label.
//!
//! Reads its config from `VEILAND_PLUGIN_CONFIG` (JSON, per
//! `docs/protocol.md` §2 and `docs/config.md` §3), constructs a
//! `veiland_text::Label`, and draws it into a dmabuf each frame.
//!
//! Differs from the box plugins in two structural ways:
//!
//! 1. The dmabuf is allocated *after* the first `Configure`, sized to
//!    the region the host gives us. Text rasterized at one resolution
//!    and stretched to another by the compositor would look blurry —
//!    the plugin draws into a same-size buffer so there is no
//!    stretching at composition time.
//!
//! 2. `FontContext::new()` runs alongside GPU setup; it does the
//!    ~30–100ms fontdb system-font scan once. Eager init is fine for a
//!    long-lived plugin.

use serde::Deserialize;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError};
use veiland_text::{FontContext, HAlign, Label, Shadow, VAlign};

const PLUGIN_NAME: &str = "label";

/// Per-frame plugin state. `font_ctx` is reused across renders; the
/// `Label` is rebuilt each frame from `config` + the current surface
/// size because rebuilding a `Label` is cheap (it's plain data).
struct State {
    font_ctx: FontContext,
    config: Config,
}

/// Plugin-side config schema. Field defaults match an "obviously
/// visible" white label at the centre of a 1920×1080 surface so a
/// plugin entry with no `[plugin.config]` table at all still produces
/// something on screen for the user to debug from.
#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_text")]
    text: String,
    #[serde(default = "default_font_family")]
    font_family: String,
    /// Font size as a **fraction of surface height** (`0.0–1.0`).
    /// `0.067` ≈ 7% of screen height — ~72px on 1080p, ~145px on 4K.
    /// Resolution- and scale-independent: the same fraction looks the
    /// same size on every monitor.
    #[serde(default = "default_font_size")]
    font_size: f32,
    #[serde(default = "default_color")]
    color: [f32; 4],
    #[serde(default)]
    halign: HAlignCfg,
    #[serde(default)]
    valign: VAlignCfg,
    /// Anchor position as a **fraction of the surface**, `[x, y]` in `0.0..=1.0`:
    /// `[0.0, 0.0]` is the top-left corner, `[0.5, 0.5]` the centre, `[1.0, 1.0]`
    /// the bottom-right. Multiplied by the surface size at render time, so a
    /// label keeps its place across resolutions (1080p, 4K) and HiDPI scales —
    /// unlike absolute pixels, which assume one surface size. `halign`/`valign`
    /// then decide which edge of the text sits on this anchor.
    #[serde(default = "default_position")]
    position: [f32; 2],
    #[serde(default)]
    rotation: f32,
    /// `None` → no shadow. `Some([x, y])` enables the shadow pass;
    /// each component is a **fraction of surface height**, same unit as
    /// `font_size`. `[0.003, 0.003]` ≈ 3px offset on 1080p.
    #[serde(default)]
    shadow_offset: Option<[f32; 2]>,
    #[serde(default = "default_shadow_color")]
    shadow_color: [f32; 4],
    #[serde(default)]
    shadow_blur: f32,
    /// Extra inter-glyph spacing as a **fraction of the computed font size**.
    /// `0.0` is natural tracking; `0.5` adds half a font-height of space
    /// between glyphs. Scales automatically with `font_size`.
    #[serde(default)]
    letter_spacing: f32,
    /// CSS-style numeric weight (100 Thin … 300 Light … 400 Normal …
    /// 700 Bold). NOT scaled — it's a face selector, not a pixel measure.
    #[serde(default = "default_font_weight")]
    font_weight: u16,
    /// Render with the family's italic face. Selects a face; does not
    /// synthesize a slant. Requires an italic face installed for
    /// `font_family` (e.g. "Liberation Sans"); CJK families like
    /// "Noto Sans CJK JP" have none and render upright.
    #[serde(default)]
    italic: bool,
}

fn default_text() -> String {
    "veiland-label (no [plugin.config] set)".to_string()
}
fn default_font_family() -> String {
    "Sans".to_string()
}
fn default_font_size() -> f32 {
    0.030
}
fn default_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 1.0]
}
fn default_position() -> [f32; 2] {
    // Centre of the surface. Fractions, not pixels — see the `position`
    // field doc.
    [0.5, 0.5]
}
fn default_shadow_color() -> [f32; 4] {
    [0.0, 0.0, 0.0, 0.6]
}
fn default_font_weight() -> u16 {
    400
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HAlignCfg {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VAlignCfg {
    Top,
    #[default]
    Middle,
    Bottom,
}

impl From<HAlignCfg> for HAlign {
    fn from(c: HAlignCfg) -> Self {
        match c {
            HAlignCfg::Left => HAlign::Left,
            HAlignCfg::Center => HAlign::Center,
            HAlignCfg::Right => HAlign::Right,
        }
    }
}

impl From<VAlignCfg> for VAlign {
    fn from(c: VAlignCfg) -> Self {
        match c {
            VAlignCfg::Top => VAlign::Top,
            VAlignCfg::Middle => VAlign::Middle,
            VAlignCfg::Bottom => VAlign::Bottom,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            text: default_text(),
            font_family: default_font_family(),
            font_size: default_font_size(),
            color: default_color(),
            halign: HAlignCfg::default(),
            valign: VAlignCfg::default(),
            position: default_position(),
            rotation: 0.0,
            shadow_offset: None,
            shadow_color: default_shadow_color(),
            shadow_blur: 0.0,
            letter_spacing: 0.0,
            font_weight: default_font_weight(),
            italic: false,
        }
    }
}

/// Build a `veiland_text::Label` for the current frame.
///
/// All size-like config values are **fractions of surface height** and are
/// converted to physical pixels here by multiplying by `sh`:
///
///   * `font_size * sh` → physical font size. A fraction tracks native
///     resolution automatically: 0.067 is ~72px on 1080p and ~145px on 4K.
///   * `shadow.offset[i] * sh` → same unit as font_size.
///   * `letter_spacing * font_size_px` → pixels, since letter_spacing is
///     a fraction of the font size (em-ish multiplier).
///   * `position` is already a fraction of the surface and is multiplied by
///     `sw`/`sh` directly; it does not go through the same path.
fn build_label(config: &Config, surface_size: (u32, u32)) -> Label {
    let (sw, sh) = (surface_size.0 as f32, surface_size.1 as f32);
    let font_size_px = config.font_size * sh;
    Label {
        text: config.text.clone(),
        font_family: config.font_family.clone(),
        font_size: font_size_px,
        color: config.color,
        halign: config.halign.into(),
        valign: config.valign.into(),
        position: (config.position[0] * sw, config.position[1] * sh),
        rotation: config.rotation,
        shadow: config.shadow_offset.map(|off| Shadow {
            offset: (off[0] * sh, off[1] * sh),
            color: config.shadow_color,
            blur: config.shadow_blur,
        }),
        letter_spacing: config.letter_spacing * font_size_px,
        font_weight: config.font_weight,
        italic: config.italic,
    }
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let config = veiland_plugin::load_config::<Config>(PLUGIN_NAME);
    eprintln!(
        "veiland-{}: config text={:?}, font={:?}, size={}, position={:?}",
        PLUGIN_NAME, config.text, config.font_family, config.font_size, config.position
    );

    // GPU context first so the FontContext lazy-init in `render` has a
    // current EGL context to compile shaders against on first frame.
    let gbm_egl = GbmEgl::new()?;

    // Handshake before allocating the dmabuf: we need the first
    // Configure's region size to pick allocation dimensions. Drawing
    // text into a hardcoded-size buffer and letting the host stretch
    // it would blur every glyph.
    // Connect preamble (from_env + handshake + hello) in one call.
    let mut conn = Connection::connect(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;
    eprintln!("connected to host, hello sent");

    eprintln!(
        "sync model: {} (host_cap={}, plugin_cap={})",
        if conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd() {
            "fast (fence fd)"
        } else {
            "slow (glFinish)"
        },
        conn.host_supports_fence_fd(),
        gbm_egl.supports_fence_fd(),
    );

    // Wait for the first Configure to learn the region size.
    let first_configure = match conn.wait_for_configure()? {
        Some(c) => c,
        None => {
            eprintln!("veiland-{}: shutdown before first configure", PLUGIN_NAME);
            return Ok(());
        }
    };
    eprintln!(
        "veiland-{}: first configure region=({},{}) {}x{} scale_120={}",
        PLUGIN_NAME,
        first_configure.region_x,
        first_configure.region_y,
        first_configure.region_w,
        first_configure.region_h,
        first_configure.scale_120,
    );

    let mut dma = DmaBuffer::new(&gbm_egl, first_configure.region_w, first_configure.region_h)?;
    eprintln!(
        "allocated {}x{} {:?}, modifier=0x{:016x}, stride={}",
        dma.width(),
        dma.height(),
        dma.format(),
        u64::from(dma.modifier()),
        dma.stride(),
    );

    dma.bind_for_rendering()?;

    // FontContext eager init: fontdb scan (~30–100ms) runs now so the
    // first FrameDone doesn't pay it. The atlas + shader inside
    // FontContext are still lazy — they wait until `render` is called.
    let mut state = State {
        font_ctx: FontContext::new(),
        config,
    };

    // On-demand: a static label only redraws when the host asks
    // (FrameDone). FramePacer owns the deferral state machine.
    let mut pacer = FramePacer::on_demand();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(&dma, &mut conn, &gbm_egl, &mut state)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME);
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

fn render_and_send(
    dma: &DmaBuffer,
    conn: &mut Connection,
    gbm_egl: &GbmEgl,
    state: &mut State,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let surface_size = (dma.width(), dma.height());
    let label = build_label(&state.config, surface_size);

    // SAFETY: bind_for_rendering left an FBO current on this thread;
    // the gl crate's function pointers were loaded by GbmEgl::new.
    unsafe {
        // Clear to transparent black. The host composites our buffer on
        // top of lower-z plugins using straight-alpha blending
        // (docs/protocol.md §12.1); transparent pixels let the layer
        // below show through. The label fragment shader writes
        // straight-alpha as well, so the maths matches.
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
    }
    state.font_ctx.render(&label, surface_size);

    conn.submit_frame(dma, gbm_egl)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
