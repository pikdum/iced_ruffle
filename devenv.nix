{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

let
  # Native libs winit (Wayland/X11) + wgpu (Vulkan/GL) dlopen at runtime.
  runtimeLibs = with pkgs; [
    vulkan-loader
    wayland
    libxkbcommon
    libGL
    libx11
    libxcursor
    libxi
    libxrandr
  ];
in
{
  packages = [
    pkgs.git
    # Ruffle's `ruffle_core` build script compiles the AVM2 playerglobal
    # (ActionScript 3 standard library) with a Java-based compiler at build time.
    pkgs.jdk
  ];

  languages.rust = {
    enable = true;
    # Ruffle's `master` uses `if let` guards, stabilized in Rust 1.95.
    # devenv's pinned nixpkgs ships 1.94, so pull a recent stable via rust-overlay.
    channel = "stable";
  };

  # Must be on the loader path or `cargo run` panics with NoWaylandLib etc.
  env.LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;

  git-hooks.hooks = {
    clippy.enable = true;
    rustfmt.enable = true;
    nixfmt.enable = true;
  };
}
