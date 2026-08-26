+++
title = "shader"
description = "Runs a Shadertoy-convention GLSL fragment shader, either one of seven shipped presets or your own file, as a full-region animated layer."
weight = 14
template = "docs-page.html"

[extra]
one_liner = "your own GLSL, running live"
category = "backgrounds"
example = "shader_nebula.toml"

[[extra.props]]
key = "preset"
type = "string"
default = "`\"nebula\"`"
meaning = "One of the shaders compiled into the binary: `nebula`, `deepfield`, `starfield`, `moon`, `meteor`, `warp`, `plasma`. Ignored when `path` is set."

[[extra.props]]
key = "path"
type = "string"
default = "unset"
meaning = "A `.frag` file of your own to run instead of a preset. Wins over `preset` when both are set. Files above 1 MiB, unreadable, empty, or not valid UTF-8 are refused."

[[extra.props]]
key = "opacity"
type = "float"
default = "`1.0`"
meaning = "At `1.0` the layer is opaque and the shader's alpha is ignored. Below `1.0` the shader's own alpha is honoured and scaled by this value, so the layer composites over whatever sits under it. Clamped to 0 to 1."

[[extra.props]]
key = "render_scale"
type = "float"
default = "`1.0`"
meaning = "Internal resolution as a fraction of the region; the host upscales. `0.5` costs a quarter of the pixels. Clamped to 0.1 to 1."

[[extra.props]]
key = "max_fps"
type = "float"
default = "`30.0`"
meaning = "Frame-rate cap. `0` means uncapped (compositor rate). Clamped to 0 to 240."

[[extra.props]]
key = "loop_seconds"
type = "float"
default = "`0`"
meaning = "When above 0, `iTime` wraps at this many seconds instead of counting up forever. Set it to a shader's natural period to make it loop seamlessly, or to any few-minute value if a long lock makes the animation stutter (`iTime` is a float, and it loses precision once it reaches the tens of thousands). `0` leaves it running freely, as on Shadertoy. Unrelated to `max_fps`, which caps frames rather than time. Clamped to 0 to 86400."
+++

Seven shaders ship in the binary, so `preset` needs nothing installed. The
interesting key is `path`: point it at a `.frag` file and the plugin runs that
instead.

## Writing your own

A shader here is a Shadertoy Image shader. The entry point is the same:

```glsl
void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    fragColor = vec4(uv, 0.5 + 0.5 * sin(iTime), 1.0);
}
```

Write only that function. The version directive, the precision qualifiers, the
uniform declarations and the `main()` that calls your `mainImage` are all
prepended for you, so a single-pass shader copied from Shadertoy usually runs
unchanged. `fragCoord` arrives with a bottom-left origin, as on Shadertoy.

Every uniform Shadertoy exposes for a single-pass shader is declared, so a
shader mentioning one compiles even where the value is not meaningful here:

| uniform | what you get |
|---|---|
| `iResolution` | buffer size in pixels; `.z` is 1 |
| `iTime`, `iTimeDelta`, `iFrame`, `iFrameRate` | live |
| `iDate` | only `.w` is filled, with seconds since local midnight; the date components are 0 |
| `iMouse` | always zero, since plugins receive no pointer events |
| `iChannel0` | a 256x256 RGBA noise texture, tiling and unfiltered |
| `iChannel1`, `iChannel2`, `iChannel3` | 1x1 black |
| `iChannelResolution`, `iChannelTime`, `iSampleRate` | declared; sized to the above |

Shaders are compiled as GLSL ES 3.00. `texture2D` is defined as an alias for
`texture` so older WebGL1 shaders keep working; larger dialect differences are
not papered over.

Multi-pass shaders (Shadertoy's Buffer A-D tabs) and audio or video channels
have no equivalent, so shaders built on those will not port.

## Checking a shader before you lock

`--check` compiles a shader and prints the compiler's own output, so you do not
have to lock the screen to find a syntax error:

```sh
veiland-shader --check ~/shaders/mine.frag
```

It exits 0 when the shader compiles. With no file argument it checks whatever
your config selects, which is a quick way to confirm a scene will render before
you rely on it.

## When a shader fails

Anything wrong with the content leaves the plugin running and draws a near-black
fill: an unknown preset name, a file that cannot be read, a compile error. The
reason is logged with the compiler's full output, so `journalctl` tells you what
happened after the fact. The lock is never at risk from a bad shader.

## Transparency

Shadertoy shaders write undefined alpha, which is why `opacity = 1.0` ignores it
by default. Set `opacity` below 1 only for a shader written with alpha in mind:
its own alpha is then honoured, premultiplied, and composited over the layers
below. The Deep Field astronomy scene
([`deepfield-astronomy.toml`](https://github.com/sylflo/veiland/blob/master/docs/examples/deepfield-astronomy.toml))
stacks four shader layers this way, three of them transparent.

## Cost

A full-screen fragment shader runs your maths on every pixel, every frame, and
some Shadertoy shaders are heavy. `max_fps` defaults to 30 rather than the
compositor rate for that reason, and `render_scale` below 1 is the other lever
if a shader is more than your GPU wants to do while the screen is locked.
