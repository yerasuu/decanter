# Khronos' Vulkan validation layer, realised only when the port is asked to
# validate, from the same nixpkgs as the ports so it shares their glibc.
(import ./pkgs.nix).vulkan-validation-layers
