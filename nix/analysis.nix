# Vision model export toolchain and ONNX artifacts for the analysis crate.
#
# Python exists here to *produce* artifacts and never to consume them: the
# runtime closure is the `ort` crate plus a `.onnx` file. Exports are ordinary
# pure derivations because the impurity (gated, authenticated HF fetches) is
# already quarantined in the weight FODs in python.nix, which are hash-pinned.
{
  pkgs,
  lib,
  sam3Cache,
  dinov3Repo,
  # Entry ID → image file, from nix/corpus.nix. Only the reference set below is
  # ever selected from it, so the other FODs are never realized.
  corpusImageFiles,
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
  sam3 = python.pkgs.buildPythonPackage {
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

    # setuptools 81 removed pkg_resources; sam3 imports it only for
    # resource_filename, so shim that one call onto importlib.resources rather
    # than pin an ancient setuptools the rest of the toolchain has moved past.
    postPatch = ''
      substituteInPlace sam3/model_builder.py --replace-fail \
        'import pkg_resources' \
        'import importlib.resources; pkg_resources = type("_pr", (), {"resource_filename": staticmethod(lambda p, r: str(importlib.resources.files(p) / r))})()'
    '';

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
    ps.onnx
    ps.onnxruntime
    ps.onnxscript
    ps.pycocotools
    ps.setuptools
    # DINOv3 loads through transformers; the nixpkgs pin is chosen partly for
    # its version, since dinov3_vit needs >= 4.56 (see flake.nix).
    ps.transformers
    sam3
    samexporter
  ]);

  # The full SAM 3 image model, every head kept, each graph in its own
  # directory. One merged image encoder emits both the grounding feature pyramid
  # and the SAM-style interactive features from a single backbone pass; the
  # language encoder, the grounding decoder, and the interactive single-object
  # decoder all stay. Keeping it whole makes the prompt style a runtime choice
  # (concept text, box exemplar, or interactive box/point) rather than a
  # re-export. The per-graph directories are because torch spills tensors past
  # the 2 GB protobuf limit into sibling files named after graph nodes, and node
  # numbering restarts per export, so a shared directory silently overwrites
  # weights.
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
        python ${./scripts/export-sam3.py} "$out"

        # The export exiting 0 says nothing about whether the artifacts load; the
        # corruption this guards against is silent and surfaces hours later in
        # whatever consumes them. Loading is the only contract the Rust side has
        # with this derivation, so it is what the build asserts.
        python ${./scripts/verify-onnx.py} "$out" \
          image_encoder language_encoder decoder decoder_interactive
      '';

  # DINOv3 has no exporter to lean on, so the graph is ours. Normalization is
  # baked in because the Rust side has no AutoImageProcessor to reproduce it;
  # the rescale and resize ahead of it stay with the caller, which is where the
  # checkpoint's own processor puts them.
  #
  # Resolution is the parameter because it is the only lever on patch-grid
  # density, and the grid is what masked pooling reads: 224px gives 14x14, so an
  # entity covering 5% of the frame lands on ~10 patches. Patch size is the
  # 16x16 kernel of the patch-embedding conv and belongs to the checkpoint.
  # Cost is linear in tokens, roughly 3.4x from 224 to 448.
  mkDinov3Onnx =
    resolution:
    pkgs.runCommand "dinov3-onnx-${toString resolution}"
      {
        nativeBuildInputs = [ exportEnv ];
        HF_HUB_OFFLINE = "1";
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      }
      ''
        export HOME="$TMPDIR"
        mkdir -p "$out"
        python ${./scripts/export-dinov3.py} ${dinov3Repo} "$out" ${toString resolution}
        python ${./scripts/verify-onnx.py} "$out" dinov3
      '';

  # The images the ingest path is compared on. Chosen against three criteria,
  # in priority order:
  #
  #   Hosting stability. These are build inputs, so a rotted URL breaks the
  #   model pipeline rather than just `just fetch-corpus`. Reddit-resolved
  #   entries are disqualified; these are Wikimedia, the Library of Congress
  #   and NYPL.
  #
  #   A spread of reduction factors. Against a 224px square these run from
  #   2.2x/1.3x to 40.9x/18.5x, and angkor-wat-moat's 3.3:1 frame reduces its
  #   two axes by 12.4x and 3.8x, so a squash applied to the wrong axis shows.
  #
  #   Colour. The corpus is dominated by monochrome 1880s-1900s photographs
  #   where an RGB/BGR swap barely moves the embedding; round-window-church is
  #   the most saturated image in it. guggenheim-construction is the opposite
  #   end and a single-channel JPEG besides, so grayscale-to-RGB expansion is
  #   exercised rather than assumed.
  #
  # Whole-image upscaling is out of scope: nothing this pipeline receives will
  # be under 224px. Per-axis upscaling is not, since the resize squashes to a
  # square rather than letterboxing, and shekar-dzong-1921 is what carries it —
  # at 448 its 300px short axis upsamples by 1.5x while its long axis reduces.
  # guggenheim-construction crosses the same line at 1.07x, too near unity to
  # rest on.
  referenceImages = [
    "shekar-dzong-1921" # 500x300, near-monochrome, upsamples its short axis at 448
    "guggenheim-construction" # 532x420, single-channel JPEG
    "round-window-church" # 1280x960, most saturated in the corpus
    "angkor-wat-moat" # 2777x843, 3.3:1
    "nyc-1909-balloon" # 9155x4136, progressive JPEG
  ];

  # The reference embeddings the cordoned tests compare against. A package
  # rather than a check: its closure reaches the HF-token weight FODs, and
  # `nix flake check` runs pure.
  #
  # The reference runs the checkpoint's own AutoImageProcessor and AutoModel, so
  # the weights are an input here as much as the graph is; the graph comes along
  # to fix the resolution and to sit in the fixture's closure.
  mkDinov3Fixture =
    resolution:
    pkgs.runCommand "dinov3-fixture-${toString resolution}"
      {
        nativeBuildInputs = [ exportEnv ];
        # Same offline discipline as the export: the weights are already in the
        # store, and a sandboxed build must not reach for the hub.
        HF_HUB_OFFLINE = "1";
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        images = builtins.toJSON (
          map (id: {
            inherit id;
            path = corpusImageFiles.${id};
          }) referenceImages
        );
        passAsFile = [ "images" ];
      }
      ''
        export HOME="$TMPDIR"
        mkdir -p "$out"
        python ${./scripts/dinov3-fixture.py} \
          ${dinov3Repo} ${mkDinov3Onnx resolution} "$imagesPath" "$out"
      '';

  # The interactive box prompts the cordoned SAM 3 test compares against, one per
  # corpus image. Seeded from the text-prompted grounding head and judged by hand:
  # a tight, unambiguous box is a clean prompt for the interactive decoder, so the
  # mask it returns is a meaningful reference rather than a degenerate one. Each is
  # normalized xyxy over the frame, chosen across a spread of aspect ratios and
  # object scales so a squash or an off-frame upscale has somewhere to show.
  sam3ReferenceBoxes = [
    {
      id = "guggenheim-construction";
      prompt = "car";
      box = [
        0.340
        0.687
        0.980
        0.969
      ];
    }
    {
      id = "cape-hatteras-lighthouse";
      prompt = "lighthouse";
      box = [
        0.510
        0.170
        0.703
        0.786
      ];
    }
    {
      id = "itsukushima-torii";
      prompt = "torii gate";
      box = [
        0.294
        0.078
        0.827
        0.823
      ];
    }
    {
      id = "fire-hydrant-scene";
      prompt = "fire hydrant";
      box = [
        0.261
        0.332
        0.710
        0.909
      ];
    }
    {
      id = "bicycle-amsterdam";
      prompt = "bicycle";
      box = [
        0.303
        0.307
        0.992
        0.995
      ];
    }
    {
      id = "arc-de-triomphe";
      prompt = "car";
      box = [
        0.729
        0.830
        0.866
        0.904
      ];
    }
    {
      id = "st-basils-wide";
      prompt = "building";
      box = [
        0.366
        0.165
        0.976
        0.952
      ];
    }
    {
      id = "taj-mahal-front";
      prompt = "building";
      box = [
        0.204
        0.056
        0.777
        0.490
      ];
    }
  ];

  # The reference masks the cordoned SAM 3 test compares against. A package rather
  # than a check for the same reason as the DINOv3 fixture: its closure reaches the
  # HF-token weight FOD, and `nix flake check` runs pure. Holds the export in its
  # closure, so realizing the fixture realizes the graph it describes.
  sam3Fixture =
    pkgs.runCommand "sam3-fixture"
      {
        nativeBuildInputs = [ exportEnv ];
        HF_HOME = sam3Cache;
        HF_HUB_OFFLINE = "1";
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        boxes = builtins.toJSON (
          map (entry: {
            inherit (entry) id prompt;
            box_xyxy_norm = entry.box;
            path = corpusImageFiles.${entry.id};
          }) sam3ReferenceBoxes
        );
        passAsFile = [ "boxes" ];
      }
      ''
        export HOME="$TMPDIR"
        mkdir -p "$out"
        python ${./scripts/sam3-fixture.py} ${sam3Onnx} "$boxesPath" "$out"
      '';
in
{
  inherit
    sam3
    samexporter
    exportEnv
    sam3Onnx
    sam3Fixture
    mkDinov3Onnx
    mkDinov3Fixture
    ;
}
