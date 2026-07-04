# SPDX-License-Identifier: GPL-3.0-or-later
#
# NixOS module for veiland. Enabling it installs the locker + reference
# plugins and registers the `veiland` PAM service, so users don't hand-
# write /etc/pam.d/veiland. Config (the TOML) is still the user's job.
#
# `self` is closed over from flake.nix so `package` can default to this
# flake's build for the *user's* system.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.veiland;
in
{
  options.services.veiland = {
    enable = lib.mkEnableOption "the veiland screen locker";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression
        "veiland.packages.\${system}.default";
      description = "The veiland package to install.";
    };
  };

  config = lib.mkIf cfg.enable {
    # veiland-core + reference plugins onto PATH.
    environment.systemPackages = [ cfg.package ];

    # Register the `veiland` PAM service. veiland only runs the auth and
    # account phases, so the default generated stack is exactly right.
    security.pam.services.veiland = { };
  };
}
