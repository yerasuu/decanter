# bbport's own shell.nix, so its build dependencies follow upstream, on the
# pinned nixpkgs. A nix-built probe cannot load the host's Mesa, so the
# Vulkan drivers are nix's too: set outright, not defaulted, because a
# host value inherited from the caller would point at drivers it can't load.
{ checkout }:
let
  pkgs = import ./pkgs.nix;
  shell = import (/. + checkout + "/shell.nix") { inherit pkgs; };
in
shell.overrideAttrs (old: {
  shellHook = (old.shellHook or "") + ''
    # PS_ for psport, BB_ for its releases before the rename, which the stable channel can run.
    export PS_IN_NIX_SHELL=1 BB_IN_NIX_SHELL=1
    export VK_LOADER_LAYERS_DISABLE=VK_LAYER_MESA_device_select
    export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/radeon_icd.x86_64.json:${pkgs.mesa}/share/vulkan/icd.d/intel_icd.x86_64.json
  '';
})
