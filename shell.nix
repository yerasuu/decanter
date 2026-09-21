{ pkgs ? import <nixpkgs> {} }:

let
  # Every shared library decanter's desktop build (webkitgtk + gtk3 +
  # xdotool + wayland + TLS) links against, needed again at runtime —
  # nix-shell doesn't put a package's `lib` dir on the dynamic loader's
  # search path just because it's a buildInput, and `makeLibraryPath`
  # below isn't transitive, so each real link-time dependency is listed
  # explicitly rather than relying on gtk3/webkitgtk_4_1 to pull it in.
  runtimeLibs = with pkgs; [
    webkitgtk_4_1
    gtk3
    xdotool
    wayland
    openssl
    cairo
    pango
    gdk-pixbuf
    atk
    harfbuzz
    glib
    glib-networking # provides GIO's TLS backend -- without it libsoup (and
                     # so WebKitGTK's network process) has no TLS
                     # implementation at all to use `SSL_CERT_FILE` with,
                     # and every https:// request the webview itself makes
                     # (cover art, any other remote <img>) fails outright,
                     # independently of the cert-file and sandbox fixes
                     # below, which only matter once a backend exists.
    libsoup_3
    at-spi2-atk
    at-spi2-core
    dbus
    fontconfig
    freetype
    libGL
    mesa
    libxkbcommon
    curl
    gst_all_1.gst-plugins-base   # appsink, para <video>/<audio> del webview
    librsvg                      # loader de SVG para gdk-pixbuf (iconos breeze-dark)
  ];
  # Cross-compiling `crates/hook` into `Galaxy64.dll` -- a real Windows
  # PE, not anything this shell otherwise touches. Kept apart from
  # `runtimeLibs`/`buildInputs` above, which are all about running
  # decanter itself on Linux.
  mingw = pkgs.pkgsCross.mingwW64;
in
pkgs.mkShell {
  nativeBuildInputs = [ pkgs.pkg-config mingw.buildPackages.gcc ];
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
  buildInputs = runtimeLibs ++ [ pkgs.umu-launcher ];

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

  # `mkShell` pulls in its own stdenv gcc even though nothing above lists
  # it as a buildInput, so the native x86_64-linux build links against
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
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-Wl,--dynamic-linker=/lib64/ld-linux-x86-64.so.2";

  # `libxdo` (from xdotool) is linked via a plain `-lxdo` with no matching
  # `-L` from its build script, unlike webkitgtk's, which pkg-config
  # already resolves — so its lib dir has to land on LIBRARY_PATH by hand
  # for the linker to find it.
  LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.xdotool ];

  # Same gap at runtime, but for the whole stack: `dx serve` runs the
  # built binary as a child process of this shell, and it inherits
  # LD_LIBRARY_PATH — without it, none of the above are on the loader's
  # default search path either.
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;

  GST_PLUGIN_SYSTEM_PATH_1_0 = "${pkgs.gst_all_1.gst-plugins-base}/lib/gstreamer-1.0";
  LOCALE_ARCHIVE = "${pkgs.glibcLocales}/lib/locale/locale-archive";

  # WebKitGTK's network stack (glib-networking, on GnuTLS) looks for
  # `SSL_CERT_FILE` specifically -- it doesn't fall back to the host's
  # `/etc/ssl/certs` the way a normal FHS distro's TLS libraries do, and
  # nix-shell doesn't set it for anything by default. Without this, every
  # HTTPS request the webview itself makes (cover art, any other <img>/
  # fetch straight from the page) fails TLS validation silently: no error
  # in the app's own logs, since it's `reqwest` -- a separate TLS stack
  # that resolves certs its own way -- that fetches and caches the URLs,
  # not the webview. Only rendering them back out breaks.
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

  # `SSL_CERT_FILE` above only matters once something actually implements
  # TLS for GIO -- `glib-networking` ships that (`libgiognutls.so`), but
  # unlike e.g. dconf's GIO module (which lands in `GIO_EXTRA_MODULES`
  # through the ambient desktop session, not this shell) it has no
  # nixpkgs setup-hook of its own, so being a buildInput alone doesn't put
  # it on GIO's module search path. Without it libsoup has no TLS backend
  # at all, and every https:// request the webview itself makes fails
  # outright -- same symptom as the missing cert file, but the cert fix
  # can't do anything about it. Prepended, not replacing, so whatever the
  # session already exports (e.g. dconf's module) is kept.
  #
  # gdk-pixbuf's own loaders.cache doesn't know about librsvg's SVG loader
  # (separate derivation) -- regenerate a merged one each time the shell
  # is entered, using the exact same gdk-pixbuf/librsvg this shell already
  # pins, so there's no version-mismatch risk like there would be reusing
  # anything from ~/.nix-profile.
  shellHook = ''
    export GIO_EXTRA_MODULES="${pkgs.glib-networking}/lib/gio/modules''${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}"
    export GDK_PIXBUF_MODULE_FILE="$(mktemp --suffix=-gdk-pixbuf-loaders.cache)"
    ${pkgs.gdk-pixbuf.dev}/bin/gdk-pixbuf-query-loaders \
      ${pkgs.gdk-pixbuf.out}/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.so \
      ${pkgs.librsvg}/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.so \
      > "$GDK_PIXBUF_MODULE_FILE"
  '';

  # WebKitGTK 2.5x defaults to a DMA-BUF/EGL renderer that aborts with
  # "Could not create default EGL display: EGL_BAD_PARAMETER" wherever
  # direct GPU access isn't set up for it (sandboxes, some Wayland/Nvidia
  # setups) — fall back to software rendering instead of crashing.
  WEBKIT_DISABLE_DMABUF_RENDERER = "1";
  WEBKIT_DISABLE_COMPOSITING_MODE = "1";

  # WebKitGTK isolates its network process in a bubblewrap sandbox by
  # default, which needs unprivileged user-namespace support from the
  # kernel — not guaranteed inside a nix-shell running in a container.
  # When bwrap can't set up the sandbox, the network process fails
  # silently rather than falling back: page navigation and local assets
  # (served from the dev server on localhost) keep working, but every
  # `<img>` pointed at a real remote host never loads, with nothing
  # logged anywhere in reach of the app itself. Disabling the sandbox
  # here trades an isolation layer we don't get anyway for actually
  # working network requests.
  WEBKIT_DISABLE_SANDBOX = "1";

  # `LIBGL_ALWAYS_SOFTWARE` alone still asks EGL to negotiate against
  # whatever the *host's* driver is first, and nix's mesa doesn't match a
  # foreign distro's own AMDGPU driver (e.g. SteamOS) — that mismatch is
  # what `EGL_BAD_PARAMETER` on display creation usually means here.
  # Pointing straight at nix's own llvmpipe (software) Gallium driver
  # sidesteps the host driver entirely instead of trying to reconcile it.
  LIBGL_ALWAYS_SOFTWARE = "1";
  GALLIUM_DRIVER = "llvmpipe";
  # 'mesa.drivers' es el nombre viejo del attribute -- nixpkgs lo fusiono
  # en 'mesa' directamente (mismo path de todos modos), y el nombre viejo
  # tira un evaluation warning de deprecado.
  LIBGL_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
}
