{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  packages = [ pkgs.git ];

  languages.rust.enable = true;

  git-hooks.hooks = {
    clippy.enable = true;
    rustfmt.enable = true;
    nixfmt.enable = true;
  };
}
