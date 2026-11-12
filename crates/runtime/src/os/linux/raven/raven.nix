# Raven's build environment on the pinned nixpkgs, as #1067's spike built it:
# CMake and Ninja, Python for its generators, the libraries of its Wayland
# desktop, and the 64-bit and 32-bit MinGW compilers for its Windows-side
# parts: its provisioning scripts compile the guest components for both widths,
# and a 32-bit program finds no C runtime without the i686 one. Without
# libffi wayland-client is not found and the Wayland desktop is left out
# without an error. OpenSSL is optional to Raven's CMake, but its crypt
# functions include it unconditionally since 3179949f14, so without it the
# runtime does not compile. `checkout` is unused: it is taken so every
# environment is entered with the same arguments.
{ checkout }:
let
  pkgs = import ./pkgs.nix;
  headers = pkgs.pkgsCross.mingwW64.windows.mingw_w64_headers;
  # provision-installer.py compiles type libraries with a plain `widl` that
  # finds the interface definitions of the MinGW headers by itself. nixpkgs
  # ships widl only inside Wine (1.6 GiB with what it needs), so it is built
  # from the mingw-w64 release the headers come from. The headers package
  # leaves that release's IDL files out, and they import its C headers, so
  # widl's include folder holds both. It is not `include/`, which the shell
  # would hand the host's gcc, so the MinGW headers would take the host's
  # place. -m32 and -m64 cover both widths.
  widl = pkgs.stdenv.mkDerivation {
    pname = "widl";
    inherit (headers) version src;
    sourceRoot = "mingw-w64-v${headers.version}/mingw-w64-tools/widl";
    configureFlags = [ "--target=x86_64-w64-mingw32" "--with-widl-includedir=${placeholder "out"}/share/widl/include" ];
    postInstall = ''
      ln -s x86_64-w64-mingw32-widl $out/bin/widl
      mkdir -p $out/share/widl/include
      ln -s ${headers}/include/* $out/share/widl/include/
      cp -n ../../mingw-w64-headers/include/*.idl $out/share/widl/include/
    '';
  };
in
pkgs.mkShell {
  # bison is only asked for by provision-installer.py's tool check; patch
  # applies Raven's corrections to libxml2.
  packages = with pkgs; [
    gcc cmake ninja python3 pkg-config bison gnupatch widl
    libffi wayland wayland-protocols wayland-scanner libxkbcommon pixman freetype libpng openssl
    pkgsCross.mingwW64.buildPackages.gcc pkgsCross.mingw32.buildPackages.gcc
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
