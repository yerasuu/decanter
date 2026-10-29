# Raven's build environment on the pinned nixpkgs, as #1067's spike built it:
# CMake and Ninja, Python for its generators, the libraries of its Wayland
# desktop, and the 64-bit MinGW compiler for its Windows-side parts. Without
# libffi wayland-client is not found and the Wayland desktop is left out
# without an error. OpenSSL is optional to Raven's CMake, but its crypt
# functions include it unconditionally since 3179949f14, so without it the
# runtime does not compile. `checkout` is unused: it is taken so every
# environment is entered with the same arguments.
{ checkout }:
let
  pkgs = import ./pkgs.nix;
in
pkgs.mkShell {
  packages = with pkgs; [
    gcc cmake ninja python3 pkg-config
    libffi wayland wayland-protocols wayland-scanner libxkbcommon pixman freetype libpng openssl
    pkgsCross.mingwW64.buildPackages.gcc
  ];
  # The cross compiler's setup hook points CC and CXX at MinGW, which then
  # builds Raven itself for Windows; Raven runs on the host, so its compilers
  # are the host's. A nix-built Raven can't load the host's Mesa either, so
  # it gets nix's Vulkan drivers, as psport's environments do.
  shellHook = ''
    export CC=${pkgs.gcc}/bin/gcc CXX=${pkgs.gcc}/bin/g++
    export VK_LOADER_LAYERS_DISABLE=VK_LAYER_MESA_device_select
    export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/radeon_icd.x86_64.json:${pkgs.mesa}/share/vulkan/icd.d/intel_icd.x86_64.json
  '';
}
