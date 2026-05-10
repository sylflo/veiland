# M0 — DMA-BUF cross-process sharing POC

Throwaway C proof-of-concept that validates the architecturally critical mechanism behind veiland: a GPU buffer rendered in one process, sampled as a texture in another, with no CPU readback.

This code is **archival**. It exists to prove the path works on real hardware before veiland-core is built. M1 onward is Rust, written from scratch. Do not build on top of this.

## What it validates

Two standalone processes, each with its own EGL/OpenGL ES context:

- **`producer`** — headless. Opens `/dev/dri/renderD128`, creates a GBM device, brings up EGL + GLES 3 with no window. Allocates a GBM buffer (`GBM_FORMAT_ARGB8888`, linear), wraps it in an `EGLImage` (`EGL_NATIVE_PIXMAP_KHR`), attaches it to an FBO, and renders into it. Exports the buffer's dmabuf fd via `gbm_bo_get_fd` and sends it to the consumer over a Unix socket using `SCM_RIGHTS`, alongside a small metadata struct (width, height, fourcc, stride, modifier).
- **`consumer`** — has a GLFW window with its own GLES 3 context. Listens on `/tmp/veiland-poc.sock`, accepts the producer's connection, pulls the fd back out of the cmsg, and imports it via `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT, ...)`. The resulting `EGLImage` is bound as a normal `GL_TEXTURE_2D` via `glEGLImageTargetTexture2DOES` and sampled onto a textured quad.

End-to-end: the producer renders an animated gradient into a GPU buffer; the consumer displays that exact buffer's contents on screen. No `glReadPixels`, no shared memory, no copy.

The IPC layer (Unix socket + `SCM_RIGHTS`) and the GPU layer (GBM allocation, EGL dmabuf import) are orthogonal. The socket has no idea the fd is GPU memory. The GPU has no idea the buffer crossed processes.

## Tested on

| | |
|---|---|
| GPU | Intel Arc(tm) Graphics (MTL) — Meteor Lake iGPU |
| Driver | Mesa 25.2.6 |
| GL | OpenGL ES 3.2 |
| GLSL | GLSL ES 3.20 |
| EGL | 1.5 |
| Format | `DRM_FORMAT_ARGB8888` (`0x34325241`), linear modifier (`0x0`) |
| Buffer size | 800×600, stride 3200 |

Other Mesa-supported GPUs are expected to work; nothing in this code is Intel-specific. Nvidia proprietary has historically had different dmabuf import behavior — untested.

## How to run

```sh
make
./consumer &   # opens a window, waits for the producer
./producer     # connects, sends the fd, sleeps 60s to keep the buffer alive
```

Expected output: the consumer window shows an orange quad (the final frame of the producer's 60-frame gradient ends near `(255, 128, 0)`).

The producer holds the GBM buffer alive with a `sleep(60)` after sending, because the consumer references it by fd; if the producer exits and frees the bo, the consumer's `EGLImage` becomes invalid.

## Steps

Each step is one commit, each adds one piece. Useful as a reading order:

1. **skeleton** — empty `producer` and `consumer` binaries, Makefile.
2. **socket** — consumer creates `AF_UNIX` socket, listens. Producer connects.
3. **`SCM_RIGHTS`** — producer sends an arbitrary file fd over the socket; consumer receives a different fd number pointing at the same kernel object. Confirms ancillary-data fd passing works.
4. **GLFW window** — consumer opens a GLFW window with a GLES 3 context, clears it.
5. **CPU texture** — consumer generates a CPU-side gradient and samples it onto a quad. Validates the consumer's full GL pipeline (shaders, VAO, EBO, sampler) before any GPU sharing is involved.
6. **producer EGL/GLES context** — producer brings up GBM + EGL + GLES headlessly. No window, no surface — `eglMakeCurrent` with `EGL_NO_SURFACE`.
7. **producer FBO** — producer allocates a GBM buffer, wraps it in an `EGLImage` (`EGL_NATIVE_PIXMAP_KHR`), attaches it to an FBO, renders an animated gradient. `glReadPixels` on the producer side confirms the render.
8. **dmabuf hand-off** — producer ships the fd + metadata over the socket; consumer imports via `EGL_LINUX_DMA_BUF_EXT` and replaces its CPU-side gradient with the producer's buffer.

## What's deliberately missing

- **Sync.** The producer calls `glFinish()` before sending the fd. The consumer assumes the buffer is ready when the message arrives. Production code uses `EGL_KHR_fence_sync` fence fds carried over `SCM_RIGHTS`. That's an M5 problem.
- **Animation.** The producer renders one batch of 60 frames and stops. The consumer samples whatever was last drawn. A real plugin loop (continuous rendering, frame-ready messages, buffer release) is M2+.
- **Multiple buffers.** Single-buffer, no pool, no `RELEASE` message. Fine for a POC; not fine for production. Buffer pool lands at M5.
- **Format negotiation.** Hardcoded `ARGB8888` linear on both ends. Real protocol negotiates formats at `HELLO` time. Hardcoded formats are acceptable through M3.
- **Error recovery.** Most failures `exit(EXIT_FAILURE)`. The real core never trusts plugin-side input.
- **Rust.** This is C because every reference codebase for `EGL` + `GBM` + `SCM_RIGHTS` is C, and we wanted to validate the architecture without also fighting Rust FFI on day one.

## Status

✓ M0 complete on the target hardware. Cross-process DMA-BUF sharing works.

Next: M1 — veiland-core in Rust, `ext-session-lock-v1` lock surface, no plugins yet. See [`../CLAUDE.md`](../CLAUDE.md) for the milestone plan.
