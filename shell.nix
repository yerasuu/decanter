{ pkgs ? import <nixpkgs> {} }:

let
  # Every shared library achv-tray's desktop build (webkitgtk + gtk3 +
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
in
pkgs.mkShell {
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = runtimeLibs;

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

  # gdk-pixbuf's own loaders.cache doesn't know about librsvg's SVG loader
  # (separate derivation) -- regenerate a merged one each time the shell
  # is entered, using the exact same gdk-pixbuf/librsvg this shell already
  # pins, so there's no version-mismatch risk like there would be reusing
  # anything from ~/.nix-profile.
  shellHook = ''
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
