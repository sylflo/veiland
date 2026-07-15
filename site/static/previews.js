// SPDX-License-Identifier: GPL-3.0-or-later
// Live canvas previews of the plugin scenes. Shared by the landing
// page and the per-plugin docs pages; placeholders until real
// captures are recorded.
(() => {
  "use strict";
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const DPR = Math.min(window.devicePixelRatio || 1, 1.5);
  const TAU = Math.PI * 2;
  const R = Math.random;

  /* ---------- canvas plumbing ---------- */
  const mounted = [];
  const io = new IntersectionObserver(
    (entries) => entries.forEach((en) => { en.target._visible = en.isIntersecting; }),
    { rootMargin: "80px" }
  );

  function mount(canvas, scene, scale) {
    canvas._visible = false;
    const m = { canvas, scene, ctx: canvas.getContext("2d"), state: {}, w: 0, h: 0, scale };
    function resize() {
      const r = canvas.getBoundingClientRect();
      if (!r.width) return;
      m.w = canvas.width = Math.round(r.width * scale);
      m.h = canvas.height = Math.round(r.height * scale);
      scene.init(m.state, m.w, m.h);
      if (reduced) { scene.draw(m.ctx, m.w, m.h, 4.2, m.state); }
    }
    resize();
    let rt;
    window.addEventListener("resize", () => { clearTimeout(rt); rt = setTimeout(resize, 180); });
    io.observe(canvas);
    mounted.push(m);
  }

  const t0 = performance.now();
  function frame() {
    const t = (performance.now() - t0) / 1000;
    for (const m of mounted) {
      if (!m.canvas._visible || !m.w) continue;
      m.scene.draw(m.ctx, m.w, m.h, t, m.state);
    }
    requestAnimationFrame(frame);
  }
  if (!reduced) requestAnimationFrame(frame);

  /* soft glow sprite factory */
  function glowSprite(rgb, size) {
    const c = document.createElement("canvas");
    c.width = c.height = size;
    const g = c.getContext("2d");
    const grad = g.createRadialGradient(size / 2, size / 2, 0, size / 2, size / 2, size / 2);
    grad.addColorStop(0, `rgba(${rgb},1)`);
    grad.addColorStop(0.35, `rgba(${rgb},0.55)`);
    grad.addColorStop(1, `rgba(${rgb},0)`);
    g.fillStyle = grad;
    g.fillRect(0, 0, size, size);
    return c;
  }
  function vgrad(ctx, h, stops) {
    const g = ctx.createLinearGradient(0, 0, 0, h);
    for (const [p, c] of stops) g.addColorStop(p, c);
    return g;
  }

  /* ---------- hero: raymarched-tunnel homage (per-pixel, low-res upscale) ---------- */
  const heroScene = {
    init(s, w, h) {
      s.lw = 240;
      s.lh = Math.max(2, Math.round((s.lw * h) / w));
      s.buf = document.createElement("canvas");
      s.buf.width = s.lw; s.buf.height = s.lh;
      s.bctx = s.buf.getContext("2d");
      s.img = s.bctx.createImageData(s.lw, s.lh);
    },
    draw(ctx, w, h, t, s) {
      const { lw, lh, img } = s;
      const d = img.data;
      const cx = lw / 2, cy = lh * 0.44;
      let i = 0;
      for (let y = 0; y < lh; y++) {
        for (let x = 0; x < lw; x++, i += 4) {
          const dx = (x - cx) / lh, dy = (y - cy) / lh;
          const r = Math.sqrt(dx * dx + dy * dy) + 1e-4;
          const a = Math.atan2(dy, dx);
          const u = 0.32 / (r + 0.05) + t * 0.55;
          const v = (a / Math.PI) * 3 + Math.sin(t * 0.22) * 0.6 + Math.sin(u * 0.7) * 0.35;
          let s1 = Math.sin(u * TAU * 0.5) * 0.5 + 0.5;
          let s2 = Math.sin(v * TAU * 0.5) * 0.5 + 0.5;
          let m = s1 * 0.65 + s2 * 0.35;
          m = m * m;
          const depth = Math.min(1, r * 2.1);           // fade to dark at center
          const glow = Math.max(0, 1 - r * 1.35);       // rose bloom near center rim
          const L = m * depth;
          d[i]     = 22 + L * 165 + glow * 55;          // R: violet -> rose
          d[i + 1] = 12 + L * 78 + glow * 26;
          d[i + 2] = 34 + L * 128 + glow * 48;
          d[i + 3] = 255;
        }
      }
      s.bctx.putImageData(img, 0, 0);
      ctx.imageSmoothingEnabled = true;
      ctx.drawImage(s.buf, 0, 0, w, h);
    },
  };

  /* ---------- gallery scenes ---------- */
  const roseSprite = glowSprite("244,166,192", 48);
  const emberSprite = glowSprite("255,150,90", 48);
  const flySprite = glowSprite("216,242,160", 64);
  const bokehSprite = glowSprite("235,205,230", 64);

  function petalField(s, w, h, n) {
    s.p = Array.from({ length: n }, () => ({
      x: R() * w, y: R() * h, sp: 0.35 + R() * 0.65,
      sz: (3 + R() * 4) * (w / 480), ph: R() * TAU, vr: (R() - 0.5) * 2,
    }));
  }
  function drawPetals(ctx, w, h, t, s, alpha) {
    for (const p of s.p) {
      const y = (p.y + t * p.sp * h * 0.14) % (h + 40) - 20;
      const x = (p.x + Math.sin(t * 0.7 + p.ph) * w * 0.03 + t * p.sp * w * 0.02 + w) % w;
      ctx.save();
      ctx.translate(x, y);
      ctx.rotate(t * p.vr + p.ph);
      ctx.fillStyle = `rgba(248,182,204,${alpha})`;
      ctx.beginPath();
      ctx.ellipse(0, 0, p.sz, p.sz * 0.55, 0, 0, TAU);
      ctx.fill();
      ctx.restore();
    }
  }

  const scenes = {
    raymarcher: heroScene,
    wallpaperprev: {
      init() {},
      draw(ctx, w, h) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#2a1a3e"], [0.55, "#7a3c5a"], [0.8, "#d98a6a"], [1, "#eab08a"]]);
        ctx.fillRect(0, 0, w, h);
        const g = ctx.createRadialGradient(w * 0.5, h * 0.78, 0, w * 0.5, h * 0.78, h * 0.5);
        g.addColorStop(0, "rgba(255,220,170,0.8)"); g.addColorStop(1, "rgba(255,220,170,0)");
        ctx.fillStyle = g; ctx.fillRect(0, 0, w, h);
        ctx.fillStyle = "#160d20";
        ctx.beginPath(); ctx.moveTo(0, h);
        for (let x = 0; x <= w; x += w / 32)
          ctx.lineTo(x, h * 0.9 - Math.sin((x / w) * Math.PI) * h * 0.09 - Math.sin((x / w) * 9) * h * 0.02);
        ctx.lineTo(w, h); ctx.fill();
      },
    },
    vignetteprev: {
      init() {},
      draw(ctx, w, h) {
        ctx.fillStyle = "#4a4258"; ctx.fillRect(0, 0, w, h);
        for (const [cx, cy, o] of [[0, 0, 0.6], [w, 0, 0.6], [0, h, 0.7], [w, h, 0.7]]) {
          const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, Math.hypot(w, h) * 0.5);
          g.addColorStop(0, `rgba(10,8,18,${o})`); g.addColorStop(1, "rgba(10,8,18,0)");
          ctx.fillStyle = g; ctx.fillRect(0, 0, w, h);
        }
      },
    },
    particlesprev: {
      init(s, w, h) {
        s.p = Array.from({ length: 30 }, () => ({
          x: R(), life: R(), sp: 0.05 + R() * 0.1, sz: (1.5 + R() * 2.5) * (w / 480), ph: R() * TAU,
        }));
      },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#141021"], [1, "#241833"]]);
        ctx.fillRect(0, 0, w, h);
        ctx.globalCompositeOperation = "lighter";
        for (const p of s.p) {
          const prog = (p.life + t * p.sp) % 1;
          const y = h - prog * h;
          const x = (p.x * w + Math.sin(t * 0.6 + p.ph) * w * 0.02 + w) % w;
          ctx.globalAlpha = Math.sin(prog * Math.PI) * 0.7;
          ctx.drawImage(bokehSprite, x - p.sz, y - p.sz, p.sz * 2, p.sz * 2);
        }
        ctx.globalAlpha = 1;
        ctx.globalCompositeOperation = "source-over";
      },
    },
    clockprev: {
      init() {},
      draw(ctx, w, h) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#101426"], [1, "#1c1430"]]);
        ctx.fillRect(0, 0, w, h);
        const d = new Date();
        const hh = String(d.getHours()).padStart(2, "0"), mm = String(d.getMinutes()).padStart(2, "0");
        ctx.textAlign = "left"; ctx.textBaseline = "top";
        ctx.fillStyle = "rgba(232,245,248,0.9)";
        ctx.font = `300 ${h * 0.22}px system-ui, sans-serif`;
        ctx.fillText(`${hh}:${mm}`, w * 0.05, h * 0.08);
        ctx.font = `${h * 0.06}px system-ui, sans-serif`;
        ctx.fillStyle = "rgba(168,214,232,0.6)";
        ctx.fillText(d.toLocaleDateString(undefined, { month: "long", day: "numeric", year: "numeric" }), w * 0.055, h * 0.36);
      },
    },
    labelprev: {
      init() {},
      draw(ctx, w, h) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#1a1128"], [1, "#2c1a38"]]);
        ctx.fillRect(0, 0, w, h);
        ctx.textAlign = "center"; ctx.textBaseline = "middle";
        ctx.fillStyle = "rgba(244,226,240,0.95)";
        ctx.font = `500 ${h * 0.13}px system-ui, sans-serif`;
        ctx.fillText("\u541b\u306e\u540d\u306f\u3002", w / 2, h * 0.42);
        ctx.font = `${h * 0.055}px system-ui, sans-serif`;
        ctx.fillStyle = "rgba(244,166,192,0.7)";
        ctx.fillText("any text, any font, any angle", w / 2, h * 0.62);
      },
    },
    sakura: {
      init(s, w, h) { petalField(s, w, h, 26); },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#241333"], [0.55, "#4a2547"], [1, "#a15570"]]);
        ctx.fillRect(0, 0, w, h);
        drawPetals(ctx, w, h, t, s, 0.9);
      },
    },
    shinkai: {
      init(s, w, h) { petalField(s, w, h, 14); },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#1b1230"], [0.6, "#3d2144"], [1, "#8a4a63"]]);
        ctx.fillRect(0, 0, w, h);
        drawPetals(ctx, w, h, t, s, 0.75);
        const d = new Date();
        const hh = String(d.getHours()).padStart(2, "0"), mm = String(d.getMinutes()).padStart(2, "0");
        ctx.fillStyle = "rgba(255,250,252,0.95)";
        ctx.font = `200 ${h * 0.3}px system-ui, sans-serif`;
        ctx.textAlign = "center"; ctx.textBaseline = "middle";
        ctx.fillText(`${hh}:${mm}`, w / 2, h * 0.42);
        ctx.font = `${h * 0.055}px ${getComputedStyle(document.documentElement).getPropertyValue("--mono")}`;
        ctx.fillStyle = "rgba(255,240,246,0.6)";
        ctx.fillText("the world is beautiful", w / 2, h * 0.62);
      },
    },
    snow: {
      init(s, w, h) {
        s.p = Array.from({ length: 70 }, () => ({
          x: R() * w, y: R() * h, r: (0.6 + R() * 1.8) * (w / 480),
          sp: 0.25 + R() * 0.7, ph: R() * TAU,
        }));
      },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#0d1322"], [1, "#1c2438"]]);
        ctx.fillRect(0, 0, w, h);
        for (const p of s.p) {
          const y = (p.y + t * p.sp * h * 0.1) % h;
          const x = (p.x + Math.sin(t * 0.5 + p.ph) * w * 0.02 + w) % w;
          ctx.fillStyle = `rgba(230,240,255,${0.35 + p.r / (w / 480) * 0.3})`;
          ctx.beginPath(); ctx.arc(x, y, p.r, 0, TAU); ctx.fill();
        }
      },
    },
    rain: {
      init(s, w, h) {
        s.p = Array.from({ length: 60 }, () => ({
          x: R() * w * 1.2, y: R() * h, sp: 1.6 + R() * 1.4, len: 0.5 + R() * 0.8,
        }));
      },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#0a0d15"], [1, "#131a26"]]);
        ctx.fillRect(0, 0, w, h);
        const slant = 0.28;
        ctx.lineWidth = Math.max(1, w / 480);
        for (const p of s.p) {
          const y = (p.y + t * p.sp * h * 0.6) % (h + 30) - 15;
          const x = (p.x - y * slant + w * 2) % (w * 1.2) - w * 0.1;
          const L = p.len * h * 0.06 * p.sp;
          ctx.strokeStyle = `rgba(170,195,225,${0.1 + p.sp * 0.12})`;
          ctx.beginPath();
          ctx.moveTo(x, y); ctx.lineTo(x - L * slant, y - L);
          ctx.stroke();
        }
      },
    },
    embers: {
      init(s, w, h) {
        s.p = Array.from({ length: 34 }, () => ({
          x: R(), life: R(), sp: 0.12 + R() * 0.2, sz: (2.5 + R() * 4) * (w / 480), ph: R() * TAU,
        }));
      },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = "#0c0705";
        ctx.fillRect(0, 0, w, h);
        const g = ctx.createRadialGradient(w / 2, h * 1.15, 0, w / 2, h * 1.15, h * 0.9);
        g.addColorStop(0, "rgba(255,110,50,0.35)"); g.addColorStop(1, "rgba(255,110,50,0)");
        ctx.fillStyle = g; ctx.fillRect(0, 0, w, h);
        ctx.globalCompositeOperation = "lighter";
        for (const p of s.p) {
          const prog = (p.life + t * p.sp) % 1;
          const y = h - prog * h * 1.1;
          const x = (p.x * w + Math.sin(t * 1.2 + p.ph + prog * 5) * w * 0.03 + w) % w;
          const a = Math.sin(prog * Math.PI) * (0.5 + 0.5 * Math.sin(t * 7 + p.ph));
          ctx.globalAlpha = Math.max(0, a);
          const sz = p.sz * (1.6 - prog);
          ctx.drawImage(emberSprite, x - sz, y - sz, sz * 2, sz * 2);
        }
        ctx.globalAlpha = 1;
        ctx.globalCompositeOperation = "source-over";
      },
    },
    fireflies: {
      init(s, w, h) {
        s.p = Array.from({ length: 11 }, () => ({
          ax: R() * TAU, ay: R() * TAU, fa: 0.1 + R() * 0.16, fb: 0.08 + R() * 0.14,
          pw: 0.6 + R() * 1.6, ph: R() * TAU, sz: (3 + R() * 5) * (w / 480),
        }));
      },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#060a08"], [1, "#0c1410"]]);
        ctx.fillRect(0, 0, w, h);
        ctx.globalCompositeOperation = "lighter";
        for (const p of s.p) {
          const x = w * (0.5 + 0.42 * Math.sin(t * p.fa * TAU + p.ax));
          const y = h * (0.5 + 0.4 * Math.sin(t * p.fb * TAU + p.ay));
          const pulse = Math.pow(0.5 + 0.5 * Math.sin(t * p.pw + p.ph), 2.2);
          ctx.globalAlpha = 0.15 + pulse * 0.85;
          const sz = p.sz * (1.2 + pulse);
          ctx.drawImage(flySprite, x - sz, y - sz, sz * 2, sz * 2);
        }
        ctx.globalAlpha = 1;
        ctx.globalCompositeOperation = "source-over";
      },
    },
    gradient: {
      init() {},
      draw(ctx, w, h, t) {
        const ph = t * 0.12;
        const hue = (base) => `hsl(${(base + Math.sin(ph) * 40 + 360) % 360} 55% 38%)`;
        const ang = ph * 0.7;
        const x = Math.cos(ang) * w, y = Math.sin(ang) * h;
        const g = ctx.createLinearGradient(w / 2 - x, h / 2 - y, w / 2 + x, h / 2 + y);
        g.addColorStop(0, hue(275)); g.addColorStop(0.5, hue(330)); g.addColorStop(1, hue(20));
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, w, h);
      },
    },
    blobs: {
      init(s, w, h) {
        s.buf = document.createElement("canvas");
        s.buf.width = Math.max(2, Math.round(w / 7));
        s.buf.height = Math.max(2, Math.round(h / 7));
        s.bctx = s.buf.getContext("2d");
        s.b = Array.from({ length: 6 }, (_, i) => ({
          fx: 0.06 + R() * 0.09, fy: 0.05 + R() * 0.08, px: R() * TAU, py: R() * TAU,
          r: 0.14 + R() * 0.16, warm: i % 2,
        }));
      },
      draw(ctx, w, h, t, s) {
        const b = s.bctx, bw = s.buf.width, bh = s.buf.height;
        b.fillStyle = "#120a1c";
        b.fillRect(0, 0, bw, bh);
        b.globalCompositeOperation = "lighter";
        for (const o of s.b) {
          const x = bw * (0.5 + 0.38 * Math.sin(t * o.fx * TAU + o.px));
          const y = bh * (0.5 + 0.38 * Math.sin(t * o.fy * TAU + o.py));
          const r = o.r * bw;
          const g = b.createRadialGradient(x, y, 0, x, y, r);
          const col = o.warm ? "255,120,110" : "200,110,220";
          g.addColorStop(0, `rgba(${col},0.85)`); g.addColorStop(1, `rgba(${col},0)`);
          b.fillStyle = g;
          b.beginPath(); b.arc(x, y, r, 0, TAU); b.fill();
        }
        b.globalCompositeOperation = "source-over";
        ctx.imageSmoothingEnabled = true;
        ctx.drawImage(s.buf, 0, 0, w, h);
      },
    },
    parallax: {
      init(s, w, h) {
        s.layers = [0.35, 0.6, 1].map((depth, li) => ({
          depth,
          p: Array.from({ length: 9 - li * 2 }, () => ({
            x: R() * w, y: R() * h, sz: (4 + li * 9 + R() * 8) * (w / 480),
          })),
        }));
      },
      draw(ctx, w, h, t, s) {
        ctx.fillStyle = vgrad(ctx, h, [[0, "#191028"], [1, "#3a2040"]]);
        ctx.fillRect(0, 0, w, h);
        ctx.globalCompositeOperation = "lighter";
        for (const L of s.layers) {
          ctx.globalAlpha = 0.1 + L.depth * 0.3;
          for (const p of L.p) {
            const x = (p.x + t * L.depth * w * 0.03) % (w + p.sz * 2) - p.sz;
            const y = p.y + Math.sin(t * 0.3 * L.depth + p.x) * h * 0.02;
            ctx.drawImage(bokehSprite, x - p.sz, y - p.sz, p.sz * 2, p.sz * 2);
          }
        }
        ctx.globalAlpha = 1;
        ctx.globalCompositeOperation = "source-over";
      },
    },
  };

  document.querySelectorAll("canvas[data-scene]").forEach((c) => {
    mount(c, scenes[c.dataset.scene], DPR);
  });
})();
