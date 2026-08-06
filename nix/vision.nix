# Vision model export toolchain and ONNX artifacts.
#
# Python exists here to *produce* artifacts and never to consume them: the
# runtime closure is the `ort` crate plus a `.onnx` file. Exports are ordinary
# pure derivations because the impurity (gated, authenticated HF fetches) is
# already quarantined in the weight FODs in python.nix, which are hash-pinned.
{
  pkgs,
  lib,
  sam3Cache,
}:

let
  # Matches python.nix: chosen for aarch64-darwin binary-cache coverage of
  # torch/torchvision. sam3 still declares `requires-python = ">=3.8"`; its
  # classifier list stopping at 3.12 is staleness, not a ceiling.
  python = pkgs.python313;

  # Upstream Meta SAM 3, not the macOS fork python.nix packages for the
  # runtime MPS path. Upstream is the right base for export specifically
  # because it has `use_rope_real` in vitdet.py, added so the complex RoPE
  # buffer can be traced; the fork predates it and would need a hand-written
  # complex-to-real patch to export at all.
  sam3 =
    assert lib.versionOlder python.pkgs.setuptools.version "81";
    python.pkgs.buildPythonPackage {
      pname = "sam3";
      version = "unstable-2026-07-30";
      pyproject = true;

      src = pkgs.fetchFromGitHub {
        owner = "facebookresearch";
        repo = "sam3";
        rev = "96914d2425f90a64f45ca977c2b5165418099543";
        hash = "sha256-1enI51bQfmgGhZ2Ra380syglBjBR/4kqqOdjLsdipec=";
      };

      # Five fixes, four of which are not macOS-specific:
      #   position_encoding.py, decoder.py  — hardcoded device="cuda", breaks
      #                                       any CUDA-less machine
      #   geometry_encoders.py              — pin_memory() pins against MPS
      #   perflib/fused.py                  — bf16 cast assuming ambient CUDA
      #                                       autocast; fp32 unfused off-GPU
      #   sam3_tracker_utils.py             — edt applies @triton.jit at import
      #                                       and triton has no macOS build, so
      #                                       the import moves into the one
      #                                       video function that needs it
      patches = [ ./patches/sam3-cpu-and-export.patch ];

      build-system = with python.pkgs; [
        setuptools
        wheel
      ];

      # numpy: upstream pins <2, nixpkgs ships 2.x. ftfy: upstream pins ==6.1.1.
      pythonRelaxDeps = [
        "numpy"
        "ftfy"
      ];

      dependencies = with python.pkgs; [
        torch
        torchvision
        numpy
        pillow
        timm
        tqdm
        ftfy
        regex
        iopath
        huggingface-hub
        typing-extensions
        einops
        pycocotools
        psutil
        setuptools # pkg_resources at runtime
      ];

      doCheck = false;
      pythonImportsCheck = [ "sam3" ];
    };

  # ONNX exporter. No tagged release carries the SAM 3 path, so it is packaged
  # from the revision the export was proven against.
  samexporter = python.pkgs.buildPythonPackage {
    pname = "samexporter";
    version = "0.4.6-unstable-2026-02-22";
    pyproject = true;

    src = pkgs.fetchFromGitHub {
      owner = "vietanhdev";
      repo = "samexporter";
      rev = "8d7844347a3aafc7f35cf4b1fa2536d7efb07d2c";
      hash = "sha256-MW7N3ihZaI5vTkY2MSrGXpYWDevaWvXnjedd/2b9UjA=";
    };

    # Seven fixes. The SAM 3 path appears never to have been run against
    # upstream facebookresearch/sam3: its RoPE monkeypatch targets buffer names
    # that exist in no upstream revision while ignoring upstream's own
    # `use_rope_real`. The rest cover a MagicMock triton shim that breaks
    # torch >= 2.9, a CUDA-default processor, a requires_grad constant-folding
    # failure, a missing no_grad, external-data filename collisions between the
    # three exports, and CoreML being auto-selected at inference.
    patches = [ ./patches/samexporter-sam3-export.patch ];

    build-system = with python.pkgs; [
      setuptools
      wheel
    ];

    # Every upstream pin is either absent from nixpkgs or a different version.
    pythonRelaxDeps = true;

    # segment-anything: SAM 1 exporter only.
    # osam: its CLIP tokenizer is imported by export_sam3 and never called —
    #   Sam3Processor tokenizes internally. Keeping it would drag in gdown,
    #   imgviz, pydantic, loguru, cmap, and beautifulsoup4 for a dead import.
    # onnxsim: only reachable under --simplify, which this export does not use.
    # opencv-python: only `sam3_onnx.py` uses cv2, and that is the Python
    #   *inference* wrapper we replace with `ort`. export_sam3.py imports none
    #   of it and `__init__.py` is empty, so dropping it avoids building the
    #   whole ffmpeg stack for a module we never load.
    pythonRemoveDeps = [
      "segment-anything"
      "osam"
      "onnxsim"
      "opencv-python"
    ];

    dependencies = with python.pkgs; [
      torch
      torchvision
      onnx
      onnxruntime
      onnxscript
      numpy
      timm
      sam3
    ];

    doCheck = false;
    # `samexporter/__init__.py` is empty, so the check has to name the submodule
    # to exercise the patches and the trimmed dependency set.
    pythonImportsCheck = [ "samexporter.export_sam3" ];
  };

  exportEnv = python.withPackages (ps: [
    ps.torch
    ps.torchvision
    ps.numpy
    ps.pillow
    ps.onnx
    ps.onnxruntime
    ps.onnxscript
    ps.pycocotools
    ps.setuptools
    sam3
    samexporter
  ]);

  # Two ONNX models plus baked language constants, each in its own directory.
  # The exporter emits three; the language encoder is consumed and deleted
  # below. Torch spills tensors past the 2 GB protobuf limit into sibling files
  # named after graph nodes (Constant_823_attr__value), and node numbering
  # restarts per export, so a shared directory silently overwrites weights: the
  # decoder exports last and loads fine while the image encoder is corrupt.
  sam3Onnx =
    pkgs.runCommand "sam3-onnx"
      {
        nativeBuildInputs = [ exportEnv ];
        # SAM 3 resolves its checkpoint through hf_hub_download, which the
        # reconstructed cache satisfies; OFFLINE keeps a sandboxed build from
        # attempting network HEAD requests before falling back to it.
        HF_HOME = sam3Cache;
        HF_HUB_OFFLINE = "1";
        # huggingface_hub constructs an httpx client before it consults
        # HF_HUB_OFFLINE, and httpx reads SSL_CERT_FILE unconditionally. The
        # sandbox sets that variable to a path it does not provide, so without a
        # real bundle here the export dies in ssl.create_default_context long
        # before it would have found the cached weights. Same fix as fetchHfRepo.
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      }
      ''
        export HOME="$TMPDIR"
        mkdir -p "$out"
        python -m samexporter.export_sam3 --output_dir "$out"

        # Replace the 1.3 GB language encoder with the ~32 KB of tensors the
        # decoder actually reads from it. Done here, in the derivation holding
        # the weights, because those tensors are a pure function of the
        # checkpoint and one fixed prompt: produced anywhere else they could
        # drift from the checkpoint with nothing to catch it.
        python ${./scripts/bake-language-constants.py} "$out"

        # The exporter exiting 0 says nothing about whether the artifacts load;
        # the corruption this guards against is silent and surfaces hours later
        # in whatever consumes them. Loading is also the only contract the Rust
        # side has with this derivation, so it is what the build should assert.
        python ${./scripts/verify-onnx.py} "$out" image_encoder decoder
      '';
in
{
  inherit
    sam3
    samexporter
    exportEnv
    sam3Onnx
    ;
}
