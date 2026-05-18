# SPDX-License-Identifier: GPL-3.0-or-later
#
# Development environment for veiland.

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

    # PAM for veiland-core's auth path. pam-sys2's build script finds the
    # headers via pkg-config; the linker needs -lpam / -lpam_misc.
    linux-pam
  ];

  shellHook = ''
    echo "veiland dev shell"
  '';
}
