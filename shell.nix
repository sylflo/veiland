# SPDX-License-Identifier: GPL-3.0-or-later
#
# Development environment for veiland.
#
# Scope: M0 only — the C proof-of-concept that validates cross-process
# DMA-BUF buffer sharing. M1+ will add the Rust toolchain and Wayland
# client crates; this file will grow at that point.

{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  name = "veiland-dev";

  nativeBuildInputs = with pkgs; [
    pkg-config
    gcc
    gnumake
  ];

  buildInputs = with pkgs; [
    # EGL comes from Mesa; GBM lives in its own package on recent nixpkgs.
    mesa
    libgbm
    libdrm

    # Consumer-side window for the M0 POC. The producer is headless
    # (renders into a GBM buffer); only the consumer needs a window.
    glfw

    # GLFW on a Wayland session links against these.
    wayland
    wayland-protocols
    libxkbcommon
  ];

  shellHook = ''
    echo "veiland dev shell — M0 (C POC for cross-process DMA-BUF)"
    echo "  EGL (Mesa) + GBM (libgbm), GLFW for the consumer window"
  '';
}
