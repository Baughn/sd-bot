with import <nixpkgs> {};

let
  libraries = with pkgs; [
    openssl
    pkg-config
    mold
  ];
in


pkgs.mkShell {
  libPath = "/run/opengl-driver/lib:" + (lib.makeLibraryPath libraries);

  packages = libraries ++ (with pkgs; [
    cargo rustc openssh
  ]);

  shellHook = ''
    export LD_LIBRARY_PATH=$LD_LIBRARY_PATH:$libPath
    export LIBRARY_PATH=$LIBRARY_PATH:$libPath
  '';
}
