# The nixpkgs every PS toolchain builds against. Pinned rather than read from
# a channel: nix-portable has no channel, and a port built against one
# nixpkgs must keep loading the same Mesa drivers when it is run later.
import (fetchTarball {
  url = "https://github.com/NixOS/nixpkgs/archive/39ad350a0602fa0a58a544344e3e9187526ea45c.tar.gz";
  sha256 = "1ri8sf7jjis1paxa61xyy3k1ylqsdy1lx1mhadlqlf4jmf1phhx3";
}) { }
