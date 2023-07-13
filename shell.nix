with import <nixpkgs> {};

let
  pyPkgs = p: with p; [
    datasets
    pillow
    transformers
    pytorchWithCuda
    pytorch-lightning
    # Xformers is in https://github.com/NixOS/nixpkgs/pull/234557
    # Get that later.
    accelerate
    torchvision
    ftfy
    tensorboard
    jinja2
    omegaconf
    safetensors
    bitsandbytes
    invisible-watermark
    # Diffusers from git
    (python3.pkgs.buildPythonPackage rec {
      pname = "diffusers";
      version = "0.18.1";
      src = fetchFromGitHub {
        owner = "huggingface";
        repo = "diffusers";
        rev = "v${version}";
        hash = "sha256-if1lulnS5GjNma+vNhc5XSMLEKjTxdo+kNx28SuBzKA=";
      };
      propagatedBuildInputs = [
        huggingface-hub
        pytorchWithCuda
        regex
        importlib-metadata
      ];
      doCheck = false;
    })
    # FastAPI
    fastapi
    uvicorn
    # Jupyter
    ipykernel
    ipywidgets
    jupyterlab
    jupytext
  ];
  cudaPackages = cudaPackages_11_8;
  libraries = (with cudaPackages; [
    cudatoolkit
    cudnn
    cuda_nvcc
    cuda_cudart
    libcublas
  ]) ++ (with pkgs; [
    stdenv.cc.cc.lib
    libGL
    glib
    ninja
    zlib
    nodejs
    openssl
    pkg-config
    mold
  ]);
in


pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    (python3.withPackages pyPkgs)
  ];

  libPath = "/run/opengl-driver/lib:" + (lib.makeLibraryPath libraries);

  packages = libraries;

  shellHook = ''
    export LD_LIBRARY_PATH=$LD_LIBRARY_PATH:$libPath
    export LIBRARY_PATH=$LIBRARY_PATH:$libPath
    export LD_PRELOAD=${pkgs.gperftools}/lib/libtcmalloc.so
  '';
}
