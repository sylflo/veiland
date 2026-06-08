// SPDX-License-Identifier: GPL-3.0-or-later

//! M11 reference plugin — renders the time and date as two labels.
//!
//! Builds two `veiland_text::Label`s every `FrameDone` from
//! `Configure.time_unix_seconds` + `time_tz_offset_seconds`, formatted
//! via `chrono`'s `strftime` patterns from the user's config.
//!
//! The plugin never reads system time itself — the host's periodic
//! Configure tick (M11 step 2, every ~30s) advances `current_time`,
//! and the next render uses it. Per the architecture, plugins are
//! pure functions of host events.

use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::Buffer;
use veiland_text::{FontContext, HAlign, Label, Shadow, VAlign};

const PLUGIN_NAME: &str = "clock";

/// Latest time received from a Configure. Defaults to the Unix epoch
/// at UTC if no Configure has carried real time yet — a pre-step-2
/// host would send zeros and we'd render "00:00 / January 01, 1970",
/// which is an obvious "host hasn't told me the time" tell.
struct CurrentTime {
    unix_seconds: i64,
    tz_offset_seconds: i32,
}

impl CurrentTime {
    fn as_datetime(&self) -> DateTime<FixedOffset> {
        let offset = FixedOffset::east_opt(self.tz_offset_seconds).unwrap_or_else(|| {
            // chrono rejects offsets outside ±86400; clamp to UTC if
            // the host sends something pathological. We log so the
            // bad value surfaces.
            eprintln!(
                "veiland-{}: tz_offset_seconds={} out of range, falling back to UTC",
                PLUGIN_NAME, self.tz_offset_seconds
            );
            FixedOffset::east_opt(0).unwrap()
        });
        DateTime::from_timestamp(self.unix_seconds, 0)
            .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap())
            .with_timezone(&offset)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_time_format")]
    time_format: String,
    #[serde(default = "default_date_format")]
    date_format: String,
    #[serde(default = "default_font_family")]
    font_family: String,
    #[serde(default = "default_time_font_size")]
    time_font_size: f32,
    #[serde(default = "default_date_font_size")]
    date_font_size: f32,
    #[serde(default = "default_time_color")]
    time_color: [f32; 4],
    #[serde(default = "default_date_color")]
    date_color: [f32; 4],
    /// Anchor as a **fraction of the surface**, `[x, y]` in `0.0..=1.0`
    /// (`[0.0, 0.0]` top-left, `[0.5, 0.5]` centre). Multiplied by the surface
    /// size at render time, so the clock keeps its place across resolutions
    /// and HiDPI scales. `halign`/`valign` pick which edge sits on the anchor.
    #[serde(default = "default_time_position")]
    time_position: [f32; 2],
    /// Date anchor, same fraction-of-surface semantics as `time_position`.
    #[serde(default = "default_date_position")]
    date_position: [f32; 2],
    #[serde(default)]
    halign: HAlignCfg,
    #[serde(default)]
    valign: VAlignCfg,
    /// Optional shadow applied to both labels (same offset / colour
    /// for time and date, KISS). `None` → no shadow.
    #[serde(default)]
    shadow_offset: Option<[f32; 2]>,
    #[serde(default = "default_shadow_color")]
    shadow_color: [f32; 4],
    #[serde(default)]
    shadow_blur: f32,
    /// Extra inter-glyph spacing in logical pixels (scaled like font_size).
    /// 0.0 is natural tracking. Separate per label so the big time and the
    /// small date can track independently.
    #[serde(default)]
    time_letter_spacing: f32,
    #[serde(default)]
    date_letter_spacing: f32,
    /// CSS-style numeric weight (100 Thin … 300 Light … 400 Normal …
    /// 700 Bold), applied to both labels. NOT scaled — it's a face
    /// selector, not a pixel measure.
    #[serde(default = "default_font_weight")]
    font_weight: u16,
}

fn default_time_format() -> String {
    "%H:%M".to_string()
}
fn default_date_format() -> String {
    "%B %d, %Y".to_string()
}
fn default_font_family() -> String {
    "Sans".to_string()
}
fn default_time_font_size() -> f32 {
    72.0
}
fn default_date_font_size() -> f32 {
    14.0
}
fn default_time_color() -> [f32; 4] {
    [0.91, 0.96, 0.97, 0.9]
}
fn default_date_color() -> [f32; 4] {
    [0.66, 0.84, 0.91, 0.6]
}
// Fractions of the surface (see the field docs). These keep the old
// top-left placement: ~2.6% in from the left, ~4.6%/12% down — i.e. the
// former [50,50]/[50,130] pixels on a 1080p reference, now resolution-free.
fn default_time_position() -> [f32; 2] {
    [0.026, 0.046]
}
fn default_date_position() -> [f32; 2] {
    [0.026, 0.150]
}
fn default_shadow_color() -> [f32; 4] {
    [0.0, 0.0, 0.0, 0.9]
}
fn default_font_weight() -> u16 {
    400
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HAlignCfg {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VAlignCfg {
    #[default]
    Top,
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

fn default_config() -> Config {
    Config {
        time_format: default_time_format(),
        date_format: default_date_format(),
        font_family: default_font_family(),
        time_font_size: default_time_font_size(),
        date_font_size: default_date_font_size(),
        time_color: default_time_color(),
        date_color: default_date_color(),
        time_position: default_time_position(),
        date_position: default_date_position(),
        halign: HAlignCfg::default(),
        valign: VAlignCfg::default(),
        shadow_offset: None,
        shadow_color: default_shadow_color(),
        shadow_blur: 0.0,
        time_letter_spacing: 0.0,
        date_letter_spacing: 0.0,
        font_weight: default_font_weight(),
    }
}

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

struct State {
    font_ctx: FontContext,
    config: Config,
    scale: u32,
    time: CurrentTime,
}

/// Build the two Labels for this frame. Same unit model as `veiland-label`:
/// `time_position`/`date_position` are **fractions of the surface** (`[0.5,
/// 0.5]` = centre), multiplied by `surface_size` so they track resolution;
/// font sizes, letter spacing and shadow offsets are *logical pixels*,
/// multiplied by `scale`.
fn build_labels(state: &State, surface_size: (u32, u32)) -> (Label, Label) {
    let s = state.scale as f32;
    let (sw, sh) = (surface_size.0 as f32, surface_size.1 as f32);
    let dt = state.time.as_datetime();
    let time_text = format!("{}", dt.format(&state.config.time_format));
    let date_text = format!("{}", dt.format(&state.config.date_format));

    let shadow = state.config.shadow_offset.map(|off| Shadow {
        offset: (off[0] * s, off[1] * s),
        color: state.config.shadow_color,
        blur: state.config.shadow_blur,
    });

    let time_label = Label {
        text: time_text,
        font_family: state.config.font_family.clone(),
        font_size: state.config.time_font_size * s,
        color: state.config.time_color,
        halign: state.config.halign.into(),
        valign: state.config.valign.into(),
        position: (
            state.config.time_position[0] * sw,
            state.config.time_position[1] * sh,
        ),
        rotation: 0.0,
        shadow: shadow.clone(),
        letter_spacing: state.config.time_letter_spacing * s,
        font_weight: state.config.font_weight,
        italic: false,
    };

    let date_label = Label {
        text: date_text,
        font_family: state.config.font_family.clone(),
        font_size: state.config.date_font_size * s,
        color: state.config.date_color,
        halign: state.config.halign.into(),
        valign: state.config.valign.into(),
        position: (
            state.config.date_position[0] * sw,
            state.config.date_position[1] * sh,
        ),
        rotation: 0.0,
        shadow,
        letter_spacing: state.config.date_letter_spacing * s,
        font_weight: state.config.font_weight,
        italic: false,
    };

    (time_label, date_label)
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let config = load_config();
    eprintln!(
        "veiland-{}: config time_format={:?} date_format={:?} font={:?}",
        PLUGIN_NAME, config.time_format, config.date_format, config.font_family,
    );

    let gbm_egl = GbmEgl::new()?;

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

    let first_configure = match conn.wait_for_configure()? {
        Some(c) => c,
        None => {
            eprintln!("veiland-{}: shutdown before first configure", PLUGIN_NAME);
            return Ok(());
        }
    };
    eprintln!(
        "veiland-{}: first configure region=({},{}) {}x{} scale={} time={} tz={}",
        PLUGIN_NAME,
        first_configure.region_x,
        first_configure.region_y,
        first_configure.region_w,
        first_configure.region_h,
        first_configure.scale,
        first_configure.time_unix_seconds,
        first_configure.time_tz_offset_seconds,
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

    let mut state = State {
        font_ctx: FontContext::new(),
        config,
        scale: first_configure.scale,
        time: CurrentTime {
            unix_seconds: first_configure.time_unix_seconds,
            tz_offset_seconds: first_configure.time_tz_offset_seconds,
        },
    };

    // Rebuilt whenever `dma` is reallocated (on a region change), since the
    // buffer carries the fd/stride/modifier the host needs to import it.
    let mut buf_msg = buffer_msg_for(&dma);

    // On-demand: render only when the host sends a FrameDone (the host
    // sends a periodic Configure tick + FrameDone to keep the clock
    // current). FramePacer owns the deferral state machine.
    let mut pacer = FramePacer::on_demand();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(&dma, &gbm_egl, &mut conn, &buf_msg, &mut state, fast_path)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                // Reallocate the dmabuf to the output's true size so text is
                // laid out at native resolution (render_and_send reads
                // surface_size from the buffer each frame) and the host's
                // composite is 1:1 instead of a stretch. Glyph atlas lives in
                // FontContext, untouched by the swap. Non-fatal on failure.
                match dma.resize_to(&gbm_egl, c.region_w, c.region_h) {
                    Ok(true) => {
                        buf_msg = buffer_msg_for(&dma);
                        eprintln!(
                            "veiland-{}: reallocated to {}x{}, stride={}",
                            PLUGIN_NAME,
                            dma.width(),
                            dma.height(),
                            dma.stride(),
                        );
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!(
                            "veiland-{}: reallocation to {}x{} failed: {} — \
                             keeping current buffer, text may stretch",
                            PLUGIN_NAME, c.region_w, c.region_h, e
                        );
                    }
                }
                state.scale = c.scale;
                state.time = CurrentTime {
                    unix_seconds: c.time_unix_seconds,
                    tz_offset_seconds: c.time_tz_offset_seconds,
                };
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

/// Build the wire `Buffer` message describing `dma`. Called at startup and
/// again after every reallocation, since the fd/stride/modifier the host
/// imports all move with the underlying GBM bo. `id` stays 0 across the
/// plugin's life — v1 is single-buffer, and a fresh `Buffer` with id 0
/// cleanly replaces the host's prior import.
fn buffer_msg_for(dma: &DmaBuffer) -> Buffer {
    Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
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
    let (time_label, date_label) = build_labels(state, surface_size);

    unsafe {
        gl::Viewport(0, 0, surface_size.0 as i32, surface_size.1 as i32);
        // Transparent black so the layers below (wallpaper, vignette,
        // particles) show through wherever we don't draw text.
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
    }

    state.font_ctx.render(&time_label, surface_size);
    state.font_ctx.render(&date_label, surface_size);

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
