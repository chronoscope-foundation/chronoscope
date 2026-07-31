# Cloud infrastructure as terranix modules, compiled to the config.tf.json
# OpenTofu reads.
#
# Nix rather than HCL so the declarations and the deploy read one definition of
# the project's coordinates (./infra-settings.nix) instead of restating it in
# .tf files. Each provider gets its own module in `modules` below, so adding
# Cloudflare is a new binding and one more entry in that list.
{
  pkgs,
  terranix,
  settings,
}:

let
  inherit (pkgs) lib;

  # The providers the shell runs and the version constraints the config carries
  # come from one derivation each, so nixpkgs is the only pin and the two cannot
  # drift apart.
  googleProvider = pkgs.terraform-providers.hashicorp_google;
  randomProvider = pkgs.terraform-providers.hashicorp_random;

  tofu = pkgs.opentofu.withPlugins (_: [
    googleProvider
    randomProvider
  ]);

  # Keyed by the resource name dependents reference. Turning on another API is
  # a line here and the `depends_on` that names it.
  googleApis = {
    artifactregistry = "artifactregistry.googleapis.com";
    orgpolicy = "orgpolicy.googleapis.com";
    run = "run.googleapis.com";
    secretmanager = "secretmanager.googleapis.com";
  };

  # WebAuthn binds a credential to the origin that created it, so a server told
  # the wrong one collects registrations nothing can authenticate against. Read
  # from the organization, which is spelled with the domain the site serves.
  webOrigin = "https://${settings.organization}";

  gcp = {
    terraform = {
      required_providers = {
        google = {
          source = "hashicorp/google";
          inherit (googleProvider) version;
        };
        random = {
          source = "hashicorp/random";
          inherit (randomProvider) version;
        };
      };

      # State in GCS, not a local file: an apply from a second machine has to
      # see what the first one created.
      backend.gcs = {
        bucket = settings.stateBucket;
        # Names which environment this state describes; another one would take
        # its own prefix in the same bucket.
        prefix = "prod";
      };
    };

    provider.google = {
      inherit (settings) project region;
    };

    # The one part of the service a deploy moves. Everything else about it is
    # declared below, so `just deploy` is a build, a push, and an apply that
    # hands over the digest the push reported.
    variable.image = {
      type = "string";
      description = "Image the Cloud Run service runs, as repository@sha256:...";
    };

    output = {
      # Read back by `just infra-plan` and `just infra-apply`, so an
      # infrastructure-only change carries the running revision forward instead
      # of needing a fresh push to name one.
      image.value = "\${var.image}";

      service_url.value = "\${google_cloud_run_v2_service.api.uri}";
    };

    resource = {
      google_project_service = lib.mapAttrs (_: service: {
        inherit (settings) project;
        inherit service;
        # Destroying one of these would otherwise switch the API off underneath
        # everything else in the project that came to depend on it.
        disable_on_destroy = false;
      }) googleApis;

      google_artifact_registry_repository.chronoscope = {
        inherit (settings) project;
        location = settings.region;
        repository_id = settings.artifactRepository;
        format = "DOCKER";
        description = "Chronoscope container images";
        # Nothing in the arguments above names the API, so without this the
        # first apply races enablement and fails.
        depends_on = [ "google_project_service.artifactregistry" ];
      };

      # The key every session and challenge token is signed with. Generated
      # here so there is no bootstrap step where a human mints a secret and
      # pastes it somewhere, and so a later apply finds it in state and leaves
      # it alone. Its value lives in that state, which is the trade taken:
      # whoever can read the state can already read the secret it describes.
      random_password.jwt = {
        # The server refuses anything under 32 bytes. Alphanumeric only, since
        # this travels through environment plumbing and the odd copy-paste, and
        # 64 of those clear the floor either way.
        length = 64;
        special = false;
      };

      google_secret_manager_secret.jwt = {
        inherit (settings) project;
        secret_id = "${settings.cloudRunService}-jwt";
        replication.auto = { };
        depends_on = [ "google_project_service.secretmanager" ];
      };

      google_secret_manager_secret_version.jwt = {
        secret = "\${google_secret_manager_secret.jwt.id}";
        secret_data = "\${random_password.jwt.result}";
      };

      # The service runs as its own identity rather than the default compute
      # account, which carries project editor and would let a compromised
      # handler rewrite the project.
      google_service_account.api = {
        inherit (settings) project;
        account_id = "${settings.cloudRunService}-runtime";
        display_name = "Chronoscope API runtime";
      };

      google_secret_manager_secret_iam_member.api_jwt = {
        inherit (settings) project;
        secret_id = "\${google_secret_manager_secret.jwt.secret_id}";
        role = "roles/secretmanager.secretAccessor";
        member = "serviceAccount:\${google_service_account.api.email}";
      };

      google_cloud_run_v2_service.api = {
        inherit (settings) project;
        name = settings.cloudRunService;
        location = settings.region;

        # The provider defaults this on, which guards a service whose loss
        # would cost something. This one holds no state: the image digest and
        # the config below describe it completely, and its filesystem dies with
        # each instance regardless. Left on, it only blocks the recreate that
        # follows a failed create, which is how the first deploy here wedged.
        deletion_protection = false;

        template = {
          service_account = "\${google_service_account.api.email}";

          containers = {
            image = "\${var.image}";

            # The instance filesystem is memory-backed and charged here: the
            # app database and the fact-store overlay both land under /tmp.
            # Sized for the read path plus what a session accumulates, since
            # nothing written through the API outlives the instance anyway.
            resources.limits.memory = "2Gi";

            env = [
              # Both of these default to localhost in the server, which is
              # right for a dev machine and unusable in front of a browser.
              {
                name = "RP_ID";
                value = settings.organization;
              }
              {
                name = "RP_ORIGIN";
                value = webOrigin;
              }
              # A reference rather than a value: the secret stays out of the
              # revision's configuration, so reading the deployed service back
              # does not disclose it.
              {
                name = "JWT_SECRET";
                value_source.secret_key_ref = {
                  secret = "\${google_secret_manager_secret.jwt.secret_id}";
                  version = "latest";
                };
              }
            ];

            # The default TCP probe passes the moment the port is bound, which
            # says nothing about whether either store opened. /health answers
            # 204 or 503 on exactly that, so a revision that came up without a
            # usable store never takes traffic.
            startup_probe = {
              http_get.path = "/health";
              timeout_seconds = 4;
              period_seconds = 5;
              failure_threshold = 6;
            };
          };
        };

        # Cloud Run resolves the secret and checks the runtime identity can
        # read it while it is creating the revision, so both have to be in
        # place before the service is.
        depends_on = [
          "google_project_service.run"
          "google_secret_manager_secret_version.jwt"
          "google_secret_manager_secret_iam_member.api_jwt"
        ];
      };

      # A public API: every reader is anonymous, and authentication is the
      # server's own passkey flow rather than Google's.
      # Cloud Identity turns on domain-restricted sharing for the whole
      # organization, which refuses any IAM member outside the customer and so
      # refuses `allUsers`. Overridden for this project alone rather than at the
      # organization, so a project that has no business being world-readable
      # keeps the inherited default.
      #
      # This makes the run.app hostname reachable without Cloudflare in front
      # of it, which is a posture worth revisiting: cache, WAF and rate limiting
      # all live at the edge, and an origin anyone can address goes around them.
      google_org_policy_policy.public_iam_members = {
        name = "projects/${settings.project}/policies/iam.allowedPolicyMemberDomains";
        parent = "projects/${settings.project}";
        spec.rules = [ { allow_all = "TRUE"; } ];
        depends_on = [ "google_project_service.orgpolicy" ];
      };

      google_cloud_run_v2_service_iam_member.public = {
        inherit (settings) project;
        location = settings.region;
        name = "\${google_cloud_run_v2_service.api.name}";
        role = "roles/run.invoker";
        member = "allUsers";
        # The binding is rejected outright until the override above is live.
        depends_on = [ "google_org_policy_policy.public_iam_members" ];
      };
    };
  };

  tfConfig = terranix.lib.terranixConfiguration {
    inherit pkgs;
    modules = [ gcp ];
  };

  # Provider schema validation with no credentials and no network: -backend=false
  # leaves the GCS state alone, and the plugins come from the wrapper's own
  # directory. Catches a misspelled or missing resource argument at commit time
  # rather than partway through an apply.
  validate =
    pkgs.runCommand "infra-validate"
      {
        nativeBuildInputs = [ tofu ];
      }
      ''
        install -m 644 ${tfConfig} config.tf.json
        export HOME=$PWD
        tofu init -backend=false -input=false
        tofu validate
        touch $out
      '';
in
{
  inherit tofu tfConfig validate;
}
