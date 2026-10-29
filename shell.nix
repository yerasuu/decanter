{ pkgs ? import <nixpkgs> {} }:

let
  # What decanter needs on the dynamic loader's search path at run time —
  # GPUI dlopens its graphics, windowing and font libraries rather than
  # linking them, and nix-shell doesn't put a package's `lib` dir there
  # just because it's a buildInput (nor is `makeLibraryPath` below
  # transitive), so each one is listed explicitly.
  runtimeLibs = with pkgs; [
    vulkan-loader
    wayland
    libxkbcommon
    libx11
    libxcb
    libxcursor
    libxi
    libxrandr
    fontconfig
    freetype
    # The host registers Mesa's `device_select` implicit Vulkan layer by
    # bare library name; without nix's Mesa here the loader can't find it.
    mesa
    # keyring's secret-service backend and the tray talk D-Bus.
    dbus
    openssl
  ];
  # Cross-compiling `crates/hook` into `Galaxy64.dll` -- a real Windows
  # PE, not anything this shell otherwise touches. Kept apart from
  # `runtimeLibs`/`buildInputs` above, which are all about running
  # decanter itself on Linux.
  mingw = pkgs.pkgsCross.mingwW64;
  host = pkgs.stdenv.hostPlatform;
  # The FHS path of the host's own loader, which every non-Nix build of
  # that architecture already gets (see the RUSTFLAGS below).
  fhsLoader = {
    x86_64 = "/lib64/ld-linux-x86-64.so.2";
    aarch64 = "/lib/ld-linux-aarch64.so.1";
  }.${host.parsed.cpu.name};
in
pkgs.mkShell {
  # squashfsTools only for `scripts/appimage`, which packs the AppDir by
  # hand (nixpkgs has no appimagetool).
  nativeBuildInputs = [
    pkgs.pkg-config
    mingw.buildPackages.gcc
    pkgs.squashfsTools
  ];
  # `umu-launcher` is a runtime CLI dependency (decanter_runtime spawns
  # `umu-run` by bare name, off PATH), not a linked library, so it's kept
  # out of `runtimeLibs` above -- that list is specifically what needs to
  # land on `LD_LIBRARY_PATH`.
  #
  # `mingw.windows.pthreads` is deliberately *not* here: only its store
  # path (below, as a plain string for `RUSTFLAGS`) is needed. Making it
  # an actual buildInput puts its headers on this shell's generic C
  # search paths too -- confirmed the hard way, it broke the *native*
  # build's own OpenSSL (`curl`'s `static-ssl` feature) by handing its
  # build script mingw's `pthread.h` instead of glibc's.
  #
  # umu-launcher only evaluates on x86_64 (it pulls in the i686 package
  # set), and Proton runs nothing but x86 anyway, so an aarch64 shell
  # builds and packages decanter without it.
  buildInputs = runtimeLibs ++ pkgs.lib.optional host.isx86_64 pkgs.umu-launcher;

  # `rustup target add x86_64-pc-windows-gnu` only installs the target's
  # std/core; it still needs a real cross linker, which is what the
  # mingw toolchain above provides.
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw.buildPackages.gcc}/bin/x86_64-w64-mingw32-gcc";
  # `windows-gnu` links `-lpthread` unconditionally (it's the threading
  # model the target expects), but nixpkgs' own mingw gcc doesn't bundle
  # `libpthread.a` -- confirmed the hard way: linking `decanter-hook`
  # without this failed with "cannot find -l:libpthread.a". `windows.pthreads`
  # is nixpkgs' separate winpthreads package that actually has it.
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L ${mingw.windows.pthreads}/lib";
  # aws-lc-sys (rustls' crypto) compiles C for the target through the `cc`
  # crate, which reads the target-suffixed variables. Without them it
  # looks for the host's `gcc`/`ar`, and its C sources need winpthreads'
  # `pthread.h`, which is passed here by flag for the same reason as above:
  # a buildInput would leak it into the native build.
  CC_x86_64_pc_windows_gnu = "${mingw.buildPackages.gcc}/bin/x86_64-w64-mingw32-gcc";
  AR_x86_64_pc_windows_gnu = "${mingw.buildPackages.gcc.bintools.bintools}/bin/x86_64-w64-mingw32-ar";
  CFLAGS_x86_64_pc_windows_gnu = "-I${mingw.windows.pthreads}/include";

  # `mkShell` pulls in its own stdenv gcc even though nothing above lists
  # it as a buildInput, so the native Linux build links against
  # *this* glibc too -- its ELF interpreter ends up hardcoded to this
  # store path, e.g. `/nix/store/.../glibc-.../lib/ld-linux-x86-64.so.2`.
  # That path only exists on this machine: a `cargo build --release`
  # binary, or the same binary once linuxdeploy bundles it into an
  # AppImage, fails to even exec elsewhere (glibc's own .so files are
  # never bundled -- linuxdeploy's own exclude-list drops them on
  # purpose, expecting the host's). Forcing the standard FHS loader path
  # here instead is what every non-Nix build already gets for free, and
  # is safe to run under this shell too: the host's own ld.so pairs with
  # its own libc.so.6, and glibc keeps forward compatibility, so a host
  # glibc equal to or newer than this one still satisfies every symbol
  # version the binary was linked against.
  "CARGO_TARGET_${host.rust.cargoEnvVarTarget}_RUSTFLAGS" = "-C link-arg=-Wl,--dynamic-linker=${fhsLoader}";

  # `cargo run` starts the binary as a child of this shell, and it
  # inherits LD_LIBRARY_PATH — without it, none of the above are on the
  # loader's default search path.
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;

  LOCALE_ARCHIVE = "${pkgs.glibcLocales}/lib/locale/locale-archive";

  # nix-shell sets no CA bundle of its own, and rustls's system verifier
  # looks for `SSL_CERT_FILE` before anything host-specific, so pointing
  # it at nix's keeps every HTTPS request (GOG's API, cover art) verifying
  # the same way inside the shell as on the host.
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

  # Same allocator setting the AppImage's AppRun uses (see scripts/appimage),
  # so memory measured from `cargo run` matches what ships.
  GLIBC_TUNABLES = "glibc.malloc.arena_max=2";

  # `VK_DRIVER_FILES`: the host's Mesa Vulkan ICDs can't load into a
  # nix-built binary, so GPUI's renderer is pointed at nix's own Mesa
  # instead. Only defaulted, so an explicit override from the caller still
  # wins.
  shellHook = ''
    export VK_DRIVER_FILES=''${VK_DRIVER_FILES:-${pkgs.mesa}/share/vulkan/icd.d/radeon_icd.${host.parsed.cpu.name}.json:${pkgs.mesa}/share/vulkan/icd.d/intel_icd.${host.parsed.cpu.name}.json}
  '';
}
