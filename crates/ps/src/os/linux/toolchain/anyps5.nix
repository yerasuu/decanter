# AnyPS5's build environment (its docs/dev/BUILD.md), on the pinned nixpkgs.
# SDL2 is built from the submodule (X11 only) and picks its backends from
# these headers. `checkout` is unused: it is taken so every environment is
# entered with the same arguments.
{ checkout }:
let
  pkgs = import ./pkgs.nix;
in
pkgs.mkShell {
  packages = with pkgs; [
    gcc cmake ninja git python3 pkg-config
    libx11 libxcb libxext libxcursor libxi libxrandr libxscrnsaver libxfixes libxkbcommon
    vulkan-headers vulkan-loader alsa-lib pipewire libpulseaudio dbus
  ];
  # Format-security warnings are errors under nix's hardening; AnyPS5
  # builds without them.
  hardeningDisable = [ "format" ];
  # The root CMakeLists links executables with -static. glibc's static
  # archives must reach only those links: on the global library path they
  # shadow libc.so and the .prx libraries fail. -no-pie because nix's gcc
  # adds -pie, and static glibc inside a PIE crashes at start.
  # The converted game runs in here too, so it gets nix's Vulkan drivers
  # for the same reason bbport.nix gives them to bbport.
  shellHook = ''
    export ANYPS5_EXE_LDFLAGS="-L${pkgs.glibc.static}/lib -no-pie"
    export VK_LOADER_LAYERS_DISABLE=VK_LAYER_MESA_device_select
    export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/radeon_icd.x86_64.json:${pkgs.mesa}/share/vulkan/icd.d/intel_icd.x86_64.json
  '';
}
