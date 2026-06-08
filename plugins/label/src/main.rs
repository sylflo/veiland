// SPDX-License-Identifier: GPL-3.0-or-later

//! M10 demo plugin — renders a single styled text label.
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
//!    long-lived plugin per `docs/m10-plan.md` Q8.

use serde::Deserialize;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::Buffer;
use veiland_text::{FontContext, HAlign, Label, Shadow, VAlign};

const PLUGIN_NAME: &str = "label";

/// Per-frame plugin state. `font_ctx` is reused across renders; the
/// `Label` is rebuilt each frame from `config` + the latest `scale`
/// because rebuilding a `Label` is cheap (it's plain data).
struct State {
    font_ctx: FontContext,
    config: Config,
    /// Current output scale factor from the most recent `Configure`.
    /// Multiplied into `font_size`, `position`, and `shadow.offset`
    /// before constructing the Label — these are logical-pixel values
    /// in the user's config; the rendered output needs physical pixels.
    /// See `docs/protocol.md` §7.1.
    scale: u32,
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
    #[serde(default = "default_font_size")]
    font_size: f32,
    #[serde(default = "default_color")]
    color: [f32; 4],
    #[serde(default)]
    halign: HAlignCfg,
    #[serde(default)]
    valign: VAlignCfg,
    #[serde(default = "default_position")]
    position: [f32; 2],
    #[serde(default)]
    rotation: f32,
    /// `None` → no shadow. `Some` enables the shadow pass with
    /// `shadow_color` + `shadow_blur` (blur ignored in M10).
    #[serde(default)]
    shadow_offset: Option<[f32; 2]>,
    #[serde(default = "default_shadow_color")]
    shadow_color: [f32; 4],
    #[serde(default)]
    shadow_blur: f32,
    /// Extra inter-glyph spacing in logical pixels (scaled like font_size).
    /// 0.0 is natural tracking.
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
    32.0
}
fn default_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 1.0]
}
fn default_position() -> [f32; 2] {
    [960.0, 540.0]
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

/// Load the plugin's config from `VEILAND_PLUGIN_CONFIG`. Missing env
/// var or unparseable JSON both fall back to defaults — a malformed
/// config should produce a visible fallback label rather than no
/// plugin at all (lockscreen-grade).
fn load_config() -> Config {
    match std::env::var("VEILAND_PLUGIN_CONFIG") {
        Ok(s) => match serde_json::from_str::<Config>(&s) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "veiland-{}: failed to parse VEILAND_PLUGIN_CONFIG as JSON: {} \
                     — falling back to defaults",
                    PLUGIN_NAME, e
                );
                default_config()
            }
        },
        Err(_) => {
            eprintln!(
                "veiland-{}: VEILAND_PLUGIN_CONFIG unset — using defaults",
                PLUGIN_NAME
            );
            default_config()
        }
    }
}

fn default_config() -> Config {
    Config {
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

/// Build a `veiland_text::Label` for the current frame. Every field
/// that is in *logical pixels* in the user's config gets multiplied by
/// `scale` here so the rendered output is in physical pixels.
fn build_label(config: &Config, scale: u32) -> Label {
    let s = scale as f32;
    Label {
        text: config.text.clone(),
        font_family: config.font_family.clone(),
        font_size: config.font_size * s,
        color: config.color,
        halign: config.halign.into(),
        valign: config.valign.into(),
        position: (config.position[0] * s, config.position[1] * s),
        rotation: config.rotation,
        shadow: config.shadow_offset.map(|off| Shadow {
            offset: (off[0] * s, off[1] * s),
            color: config.shadow_color,
            blur: config.shadow_blur,
        }),
        letter_spacing: config.letter_spacing * s,
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

    let config = load_config();
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

    let fast_path = conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd();
    eprintln!(
        "sync model: {} (host_cap={}, plugin_cap={})",
        if fast_path {
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
        "veiland-{}: first configure region=({},{}) {}x{} scale={}",
        PLUGIN_NAME,
        first_configure.region_x,
        first_configure.region_y,
        first_configure.region_w,
        first_configure.region_h,
        first_configure.scale,
    );

    let dma = DmaBuffer::new(&gbm_egl, first_configure.region_w, first_configure.region_h)?;
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
        scale: first_configure.scale,
    };

    let buf_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };

    // On-demand: a static label only redraws when the host asks
    // (FrameDone). FramePacer owns the deferral state machine.
    let mut pacer = FramePacer::on_demand();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(&dma, &gbm_egl, &mut conn, &buf_msg, &mut state, fast_path)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                if c.region_w != dma.width() || c.region_h != dma.height() {
                    // We don't reallocate the dmabuf in M10. Log so the
                    // user sees the mismatch; the host will stretch our
                    // output and text will be slightly off-scale until
                    // the plugin is restarted. M11+ may add live
                    // realloc on region change.
                    eprintln!(
                        "veiland-{}: configure region {}x{} differs from initial {}x{}; \
                         keeping initial buffer size, text may stretch",
                        PLUGIN_NAME,
                        c.region_w,
                        c.region_h,
                        dma.width(),
                        dma.height(),
                    );
                }
                state.scale = c.scale;
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
    gbm_egl: &GbmEgl,
    conn: &mut Connection,
    buf_msg: &Buffer,
    state: &mut State,
    fast_path: bool,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let surface_size = (dma.width(), dma.height());
    let label = build_label(&state.config, state.scale);

    // SAFETY: bind_for_rendering left an FBO current on this thread;
    // the gl crate's function pointers were loaded by GbmEgl::new.
    unsafe {
        gl::Viewport(0, 0, surface_size.0 as i32, surface_size.1 as i32);
        // Clear to transparent black. The host composites our buffer on
        // top of lower-z plugins using straight-alpha blending
        // (docs/protocol.md §12.1); transparent pixels let the layer
        // below show through. The label fragment shader writes
        // straight-alpha as well, so the maths matches.
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
    }
    state.font_ctx.render(&label, surface_size);

    if fast_path {
        unsafe {
            gl::Flush();
        }
        let fence = SyncFence::create(gbm_egl)?;
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), Some(fence.as_fd()))?;
    } else {
        dma.finish();
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), None)?;
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
