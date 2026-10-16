# Home-manager module for Trigo Whispr speech-to-text
#
# Provides a systemd user service for autostart.
# Usage: imports = [ trigo-whispr.homeManagerModules.default ];
#        services.trigo-whispr.enable = true;
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.trigo-whispr;
in
{
  options.services.trigo-whispr = {
    enable = lib.mkEnableOption "Trigo Whispr speech-to-text user service";

    package = lib.mkOption {
      type = lib.types.package;
      defaultText = lib.literalExpression "trigo-whispr.packages.\${system}.trigo-whispr";
      description = "The Trigo Whispr package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.user.services.trigo-whispr = {
      Unit = {
        Description = "Trigo Whispr speech-to-text";
        After = [ "graphical-session.target" ];
        PartOf = [ "graphical-session.target" ];
      };
      Service = {
        ExecStart = "${cfg.package}/bin/trigo-whispr";
        Restart = "on-failure";
        RestartSec = 5;
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };
  };
}
