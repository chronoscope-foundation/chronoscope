# Cloud infrastructure as terranix modules, compiled to the config.tf.json
# OpenTofu reads.
#
# Nix rather than HCL so the declarations and the deploy read one definition of
# the project's coordinates (./infra-settings.nix) instead of restating it in
# .tf files. Each provider gets its own module in `modules` below, so adding
# one is a new binding and one more entry in that list.
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
  cloudflareProvider = pkgs.terraform-providers.cloudflare_cloudflare;

  tofu = pkgs.opentofu.withPlugins (_: [
    googleProvider
    randomProvider
    cloudflareProvider
  ]);

  # Keyed by the resource name dependents reference. Turning on another API is
  # a line here and the `depends_on` that names it.
  googleApis = {
    artifactregistry = "artifactregistry.googleapis.com";
    orgpolicy = "orgpolicy.googleapis.com";
    run = "run.googleapis.com";
    secretmanager = "secretmanager.googleapis.com";
    sqladmin = "sqladmin.googleapis.com";
  };

  # WebAuthn binds a credential to the origin that created it, so a server told
  # the wrong one collects registrations nothing can authenticate against. Read
  # from the organization, which is spelled with the domain the site serves.
  webOrigin = "https://${settings.organization}";

  # A Worker's script, read at apply time via Terraform's file(), which returns
  # file contents raw. A JS template literal like ${req.url} then survives
  # instead of reading as an OpenTofu interpolation. The nix path coerces to the
  # script's store path, which config.tf.json references so nix keeps it realized.
  workerContent = path: "\${file(\"${path}\")}";

  # The three transform option strings the API mints, duplicated from
  # Rendition::cloudflare_options in api/src/cdn.rs across the Rust/terranix
  # boundary. The firewall rule below allows exactly these and blocks the rest,
  # so a rendition-size change must land on both sides or the new size 403s.
  cdnRenditionOptions = [
    "width=320,format=auto,fit=scale-down"
    "width=640,format=auto,fit=scale-down"
    "width=1600,format=auto,fit=scale-down"
  ];

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

      # Secrets generated here so no human mints or pastes one, and a later apply
      # finds them in state and leaves them alone. Their values live in that
      # state, the trade taken: whoever can read the state can already read the
      # secret it holds. The JWT key signs sessions; the sweep token is the shared
      # secret `/mirror/sweep` matches its header against.
      random_password = {
        jwt = {
          # The server refuses anything under 32 bytes. Alphanumeric only, since
          # this travels through environment plumbing and the odd copy-paste, and
          # 64 of those clear the floor either way.
          length = 64;
          special = false;
        };
        mirror_sweep_token = {
          length = 48;
          special = false;
        };
      };

      # The service runs as its own identity rather than the default compute
      # account, which carries project editor and would let a compromised
      # handler rewrite the project.
      google_service_account.api = {
        inherit (settings) project;
        account_id = "${settings.cloudRunService}-runtime";
        display_name = "Chronoscope API runtime";
      };

      # The three secrets the API server reads at startup: the JWT signing key,
      # the Queues Write token minted in the cloudflare module, and the sweep
      # endpoint's shared secret.
      google_secret_manager_secret = {
        jwt = {
          inherit (settings) project;
          secret_id = "${settings.cloudRunService}-jwt";
          replication.auto = { };
          depends_on = [ "google_project_service.secretmanager" ];
        };
        mirror_cf_token = {
          inherit (settings) project;
          secret_id = "${settings.cloudRunService}-mirror-cf-token";
          replication.auto = { };
          depends_on = [ "google_project_service.secretmanager" ];
        };
        mirror_sweep_token = {
          inherit (settings) project;
          secret_id = "${settings.cloudRunService}-mirror-sweep-token";
          replication.auto = { };
          depends_on = [ "google_project_service.secretmanager" ];
        };
      };

      google_secret_manager_secret_version = {
        jwt = {
          secret = "\${google_secret_manager_secret.jwt.id}";
          secret_data = "\${random_password.jwt.result}";
        };
        mirror_cf_token = {
          secret = "\${google_secret_manager_secret.mirror_cf_token.id}";
          secret_data = "\${cloudflare_api_token.mirror.value}";
        };
        mirror_sweep_token = {
          secret = "\${google_secret_manager_secret.mirror_sweep_token.id}";
          secret_data = "\${random_password.mirror_sweep_token.result}";
        };
      };

      google_secret_manager_secret_iam_member = {
        api_jwt = {
          inherit (settings) project;
          secret_id = "\${google_secret_manager_secret.jwt.secret_id}";
          role = "roles/secretmanager.secretAccessor";
          member = "serviceAccount:\${google_service_account.api.email}";
        };
        api_mirror_cf_token = {
          inherit (settings) project;
          secret_id = "\${google_secret_manager_secret.mirror_cf_token.secret_id}";
          role = "roles/secretmanager.secretAccessor";
          member = "serviceAccount:\${google_service_account.api.email}";
        };
        api_mirror_sweep_token = {
          inherit (settings) project;
          secret_id = "\${google_secret_manager_secret.mirror_sweep_token.secret_id}";
          role = "roles/secretmanager.secretAccessor";
          member = "serviceAccount:\${google_service_account.api.email}";
        };
      };

      # Everything the API serves comes out of here, so the instance is sized
      # for what serving costs: a recursive retraction CTE per tile request, and
      # GiST index maintenance in whatever heap is left while a load runs. The
      # shared-core tier below this one offers 0.6 GB for both and caps
      # connections under what a service and a load hold open together.
      google_sql_database_instance.primary = {
        inherit (settings) project region;
        name = settings.sqlInstance;

        # What `postgres-smoke` and `api-postgres` bring up: flake.nix's
        # postgresWithPostgis is this same major, so a query the gate exercised
        # meets the planner it will meet here. The two move together.
        database_version = "POSTGRES_18";

        # The Cloud Run service turns this off because its declaration describes
        # it completely and it keeps nothing. Both halves fail here: this holds
        # every submit anyone has made, and nothing in the repository can
        # reproduce them. Terraform reads the flag out of *state* when it decides
        # whether it may destroy, so clearing it is an apply of its own, ahead of
        # any apply that would.
        deletion_protection = true;

        settings = {
          # POSTGRES_18 defaults to the ENTERPRISE_PLUS edition, which admits
          # only perf-optimized machine types. The shared-core tier below is an
          # ENTERPRISE machine, so the edition is named to match the tier.
          edition = "ENTERPRISE";
          tier = "db-g1-small";

          # Storage only ever grows, so this asks for what the curated set needs
          # and lets a larger load extend it unattended.
          disk_type = "PD_SSD";
          disk_size = 10;
          disk_autoresize = true;

          # Nothing on the internet may open a database session here. REQUIRED
          # admits only the Cloud SQL Auth Proxy and the language connectors,
          # which prove `cloudsql.instances.connect` against the Admin API
          # before the handshake, so admission is an IAM decision. Cloud Run's
          # built-in /cloudsql socket is exactly that proxy, and
          # authorized_networks stays empty because the proxy never consulted
          # it.
          connector_enforcement = "REQUIRED";

          # The public address it refuses on is still allocated and still
          # completes a TCP connection, so the instance is reachable and
          # rejecting. Unreachable is a private IP, and that brings a VPC, a
          # reserved peering range, a service-networking connection and Direct
          # VPC egress on the service: four networking components in front of a
          # database with one client. Weighed at that price, and deferred.
          ip_configuration.ipv4_enabled = true;

          # Lets a connection authenticate as an IAM principal, which is what
          # makes the server's credential a short-lived access token and leaves
          # no password anywhere to store or rotate.
          database_flags = [
            {
              name = "cloudsql.iam_authentication";
              value = "on";
            }
          ];

          # A corpus a job can rebuild needs no backups, and the first user
          # submit ends that. Point-in-time recovery is what puts a bad write
          # inside a window someone can wind back out of.
          backup_configuration = {
            enabled = true;
            point_in_time_recovery_enabled = true;
            # UTC, and early enough that the backup window has closed before
            # maintenance opens below.
            start_time = "04:00";
          };

          # Pinned to a known hour because shared-core carries no SLA and
          # maintenance restarts the instance. Cloud Run holds no warm instance,
          # so a request arriving in the window meets a cold start whose
          # `connect` fails, spending the startup probe's budget.
          maintenance_window = {
            day = 7;
            hour = 9;
            update_track = "stable";
          };
        };

        # The race the registry above names: nothing in these arguments
        # mentions the API, so the first apply would reach it unenabled.
        depends_on = [ "google_project_service.sqladmin" ];
      };

      # An instance carries several databases and a DSN selects one; this is
      # where this deployment's data lives. Named for the deployment, since the
      # fact store is its first tenant and the app's own state lands beside it.
      google_sql_database.chronoscope = {
        inherit (settings) project;
        name = settings.sqlDatabase;
        instance = "\${google_sql_database_instance.primary.name}";
      };

      # The loader's identity, held apart from the API's because the two want
      # different power over the database. This one runs as a job, builds the
      # schema and exits; the API's serves the internet for as long as a
      # revision lives.
      google_service_account.loader = {
        inherit (settings) project;
        account_id = "chronoscope-facts-loader";
        display_name = "Chronoscope fact store loader";
      };

      # Postgres sees a service account as its email with `.gserviceaccount.com`
      # trimmed off, and that is the username each connection URL carries; taken
      # from the accounts here so the two spellings cannot drift.
      #
      # The database role is what separates them. The loader creates the PostGIS
      # extension, which on Cloud SQL only a member of `cloudsqlsuperuser` may
      # do, and it owns the schema it migrates; the field grants that at user
      # creation, so no password and no privileged session is involved. The
      # server reads and appends, and it is the process the internet reaches, so
      # it holds the default role here and takes its privileges from grants the
      # loader issues after migrating.
      google_sql_user.api = {
        inherit (settings) project;
        instance = "\${google_sql_database_instance.primary.name}";
        name = "\${trimsuffix(google_service_account.api.email, \".gserviceaccount.com\")}";
        type = "CLOUD_IAM_SERVICE_ACCOUNT";
      };

      google_sql_user.loader = {
        inherit (settings) project;
        instance = "\${google_sql_database_instance.primary.name}";
        name = "\${trimsuffix(google_service_account.loader.email, \".gserviceaccount.com\")}";
        type = "CLOUD_IAM_SERVICE_ACCOUNT";
        database_roles = [ "cloudsqlsuperuser" ];
      };

      # Reaching the instance and logging in to it are separate grants: `client`
      # admits the connection, `instanceUser` is what makes an access token
      # stand for this identity at the handshake. Both identities dial the same
      # way, so both carry both; the split between them lives in the database
      # role above.
      google_project_iam_member = {
        api_cloudsql_client = {
          inherit (settings) project;
          role = "roles/cloudsql.client";
          member = "serviceAccount:\${google_service_account.api.email}";
        };

        api_cloudsql_instance_user = {
          inherit (settings) project;
          role = "roles/cloudsql.instanceUser";
          member = "serviceAccount:\${google_service_account.api.email}";
        };

        loader_cloudsql_client = {
          inherit (settings) project;
          role = "roles/cloudsql.client";
          member = "serviceAccount:\${google_service_account.loader.email}";
        };

        loader_cloudsql_instance_user = {
          inherit (settings) project;
          role = "roles/cloudsql.instanceUser";
          member = "serviceAccount:\${google_service_account.loader.email}";
        };
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
            resources.limits = {
              memory = "2Gi";
              # Named because Cloud Run fills it in regardless, and a limit the
              # declaration does not mention reads as one to remove, so every
              # plan wants to roll a revision undoing the platform's default.
              # Spelled in millicores because that is the form the API returns,
              # and "1" would read as a change on every plan forever.
              cpu = "1000m";
            };

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
              # The mirror sweep's coordinates: the account and queue it enqueues
              # to (plain), and the two tokens (references, kept out of the
              # revision's readable config). Absent, the endpoint refuses.
              {
                name = "CLOUDFLARE_ACCOUNT_ID";
                value = settings.cloudflareAccount;
              }
              {
                name = "MIRROR_QUEUE_ID";
                value = "\${cloudflare_queue.mirror.id}";
              }
              {
                name = "CLOUDFLARE_API_TOKEN";
                value_source.secret_key_ref = {
                  secret = "\${google_secret_manager_secret.mirror_cf_token.secret_id}";
                  version = "latest";
                };
              }
              {
                name = "MIRROR_SWEEP_TOKEN";
                value_source.secret_key_ref = {
                  secret = "\${google_secret_manager_secret.mirror_sweep_token.secret_id}";
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
          "google_secret_manager_secret_version.mirror_cf_token"
          "google_secret_manager_secret_iam_member.api_mirror_cf_token"
          "google_secret_manager_secret_version.mirror_sweep_token"
          "google_secret_manager_secret_iam_member.api_mirror_sweep_token"
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

  # The edge in front of everything a browser touches. One Worker serves the
  # web bundle as static assets and proxies /api to Cloud Run, which is what
  # makes the site and the API one origin: no CORS in the app, no api.
  # subdomain, and a passkey bound to the one hostname the API is told about.
  #
  # Workers rather than Pages because the media pipeline wants Queue consumers,
  # Image Resizing and Cron Triggers, all of which are Workers-only. Serving the
  # frontend from the same product keeps that one deployment model.
  cloudflare = {
    terraform.required_providers.cloudflare = {
      source = "cloudflare/cloudflare";
      inherit (cloudflareProvider) version;
    };

    # No provider block: the token comes from CLOUDFLARE_API_TOKEN in the
    # environment tofu runs in.

    # The other half of what a deploy moves. Unlike the image, this is a
    # directory the provider reads while planning — it hashes every file to
    # decide what to upload — so it has to be a path that exists right now,
    # which is why the recipes build it rather than carrying the last one
    # forward out of the state.
    variable.web_dist = {
      type = "string";
      description = "Directory the Worker serves as static assets (a dist/ build of the web bundle)";
    };

    output = {
      web_dist.value = "\${var.web_dist}";

      # The queue's opaque id, which the send REST API takes (not the name). The
      # dispatcher recipe reads it back from state rather than the operator
      # copying it from the dashboard.
      mirror_queue_id.value = "\${cloudflare_queue.mirror.id}";
    };

    resource = {
      # A token scoped to Queues Write on the account and nothing else: the API
      # server presents it to enqueue mirror fetches. Minted by the tofu identity
      # (itself a user token with API Tokens Write), so no human pastes it; its
      # value goes to a Secret Manager secret and into the Cloud Run env. That
      # value lives in the state, the trade the JWT secret also takes.
      cloudflare_api_token.mirror = {
        name = "${settings.cloudRunService}-mirror-queues";
        policies = [
          {
            effect = "allow";
            # "Queues Write" permission group, a stable global id hardcoded rather
            # than looked up. The dashboard table labels it "Edit", but the API
            # name is "Queues Write", so a by-name lookup is easy to get wrong.
            permission_groups = [ { id = "366f57075ffc42689627bcf8242a1b6d"; } ];
            resources = builtins.toJSON {
              "com.cloudflare.api.account.${settings.cloudflareAccount}" = "*";
            };
          }
        ];
      };

      cloudflare_workers_script = {
        front_door = {
          account_id = settings.cloudflareAccount;
          script_name = settings.workerScript;

          # Read in rather than pointed at: a module's name and its file's name
          # have to agree, and a store path's basename carries a hash.
          main_module = "front-door.js";
          content = workerContent ./front-door.js;

          # Pinned so a change in runtime semantics arrives when someone moves
          # this line, rather than on whichever redeploy happens to follow one.
          compatibility_date = "2026-07-01";

          bindings = [
            # What the Worker answers asset misses from, so a 404 is the one the
            # assets config describes rather than a string this script invents.
            {
              name = "ASSETS";
              type = "assets";
            }
            # The API's hostname, taken from the service declared above. The
            # Worker never has it written down, and a Cloud Run recreate carries
            # a new URL into the Worker on the same apply.
            {
              name = "API_ORIGIN";
              type = "plain_text";
              text = "\${google_cloud_run_v2_service.api.uri}";
            }
          ];

          assets = {
            directory = "\${var.web_dist}";

            config = {
              # The frontend routes /about, /faq and /related-work in the client,
              # so a reload or a shared link on one of those has to arrive as
              # index.html instead of a 404.
              not_found_handling = "single-page-application";

              # Only /api/* runs the Worker before the assets are consulted, so
              # everything in the bundle is served with no code in the path.
              # Naming it keeps the line above from swallowing the API:
              # single-page-application answers *navigation* requests from
              # index.html, so without this, opening an /api URL in a browser
              # returns the app instead of the endpoint.
              run_worker_first = [ "/api/*" ];
            };
          };
        };

        # It fetches each upstream image and puts it in R2 through the binding,
        # so no R2 credential lives anywhere but the edge. A queue consumer
        # serves no HTTP, so it needs no route or custom domain.
        mirror_consumer = {
          account_id = settings.cloudflareAccount;
          script_name = settings.mirrorConsumerScript;

          main_module = "mirror-consumer.js";
          content = workerContent ../integrations/mirror-consumer.js;

          # Pinned like front_door: the runtime moves when this line does, not on
          # whichever redeploy happens to follow one.
          compatibility_date = "2026-07-01";

          bindings = [
            # The R2 sink, named off the bucket resource so tofu orders its
            # creation ahead of the script that writes to it.
            {
              name = "MEDIA_BUCKET";
              type = "r2_bucket";
              bucket_name = "\${cloudflare_r2_bucket.media.name}";
            }
          ];

          # A queue consumer has no HTTP request to leave a trace, and its only
          # record of an image it refused (a bad content type, an oversize) is a
          # console line. Persist those so a drop is diagnosable after the fact
          # rather than only during a live tail. Spelled out down to `persist`,
          # which is computed: left unset, the server decides whether a log
          # survives the tail it was written to.
          observability = {
            enabled = true;
            logs = {
              enabled = true;
              invocation_logs = true;
              persist = true;
            };
          };
        };
      };

      # A Worker is published on <script>.<account>.workers.dev by default.
      # That is a second public origin serving the same app, which the API
      # refuses to authenticate against (its RP_ORIGIN is the apex) and search
      # engines would happily index alongside the real one.
      cloudflare_workers_script_subdomain = {
        front_door = {
          account_id = settings.cloudflareAccount;
          script_name = "\${cloudflare_workers_script.front_door.script_name}";
          enabled = false;
        };

        # No workers.dev origin for the consumer either; it is reached only by
        # the queue, never over HTTP.
        mirror_consumer = {
          account_id = settings.cloudflareAccount;
          script_name = "\${cloudflare_workers_script.mirror_consumer.script_name}";
          enabled = false;
        };
      };

      # The mirror pipeline's transport. Two queues so a message that exhausts
      # its retries has somewhere to land: the consumer wiring below points the
      # main queue's dead letters at the dlq.
      cloudflare_queue = {
        mirror = {
          account_id = settings.cloudflareAccount;
          queue_name = settings.mirrorQueue;
        };

        mirror_dlq = {
          account_id = settings.cloudflareAccount;
          queue_name = settings.mirrorDlq;
        };
      };

      # Binds mirror_consumer to the mirror queue. Bounded max_concurrency is the
      # politeness lever against Wikimedia: the per-location rate-limit binding
      # would multiply it by PoP, so the ceiling is set here instead. A message
      # that exhausts max_retries is dead-lettered.
      #
      # dead_letter_queue takes the dlq's name, not its id (which is what
      # infra-validate accepts against this provider's schema).
      cloudflare_queue_consumer.mirror = {
        account_id = settings.cloudflareAccount;
        queue_id = "\${cloudflare_queue.mirror.id}";
        script_name = "\${cloudflare_workers_script.mirror_consumer.script_name}";
        type = "worker";
        dead_letter_queue = "\${cloudflare_queue.mirror_dlq.queue_name}";
        settings = {
          max_concurrency = 4;
          max_retries = 4;
          retry_delay = 30;
        };
      };

      # Creates the apex record and the certificate along with the binding, so
      # the zone holds no placeholder address whose only purpose is to be
      # proxied. Every path on the hostname is the Worker's.
      cloudflare_workers_custom_domain.apex = {
        account_id = settings.cloudflareAccount;
        zone_id = settings.cloudflareZone;
        hostname = settings.organization;
        service = "\${cloudflare_workers_script.front_door.script_name}";
        # Cloudflare fills in `environment` regardless, and an attribute the
        # declaration omits reads as one to remove. Removing this one is not an
        # in-place update: it forces replacement, so every deploy would destroy
        # and recreate the apex binding and take the site off the Worker while
        # it did. Ignored rather than assigned: both spellings warn that the
        # attribute is deprecated, so the warning is not what separates them.
        # Assigning a value claims we manage a field Cloudflare owns, while
        # ignoring it says we do not, and when the attribute goes away the diff
        # being suppressed goes with it.
        lifecycle = [ { ignore_changes = [ "environment" ]; } ];
      };

      # www exists to be redirected, not served: the API checks WebAuthn
      # origins against the apex exactly, so a passkey created on www would be
      # rejected on the next sign-in. Proxied at a reserved documentation
      # address (RFC 5737), which is never dialed — the rule below answers
      # first, and the record exists only so Cloudflare terminates TLS for the
      # name at all.
      cloudflare_dns_record.www = {
        zone_id = settings.cloudflareZone;
        name = "www.${settings.organization}";
        type = "A";
        content = "192.0.2.1";
        proxied = true;
        # Proxied records are answered from Cloudflare's own addresses, so the
        # record's TTL is not a thing a resolver ever sees. 1 is "automatic",
        # which is the only value the API accepts here.
        ttl = 1;
        comment = "Redirected to the apex; see the redirects ruleset";
      };

      # Mirrored fact-store media lives in R2, keyed by the content address the
      # API derives. Served under the zone rather than the r2.dev URL so the
      # transform path and the header rules below apply to it.
      cloudflare_r2_bucket.media = {
        account_id = settings.cloudflareAccount;
        name = settings.cdnBucket;
      };

      # Binds the CDN host to the bucket and mints the host's own DNS record and
      # TLS certificate. There is deliberately no cloudflare_dns_record for the
      # name: this resource owns it, and a hand-made record would collide.
      # bucket_name references the bucket resource rather than the settings
      # literal, so tofu orders the bucket's creation ahead of the binding.
      cloudflare_r2_custom_domain.media = {
        account_id = settings.cloudflareAccount;
        zone_id = settings.cloudflareZone;
        domain = settings.cdnHost;
        enabled = true;
        bucket_name = "\${cloudflare_r2_bucket.media.name}";
      };

      # Enables Image Resizing (transformations) for the zone, which is what
      # makes the /cdn-cgi/image/ path render a rendition instead of 404ing.
      # The provider does not validate setting_id, so a typo passes validate and
      # fails only at apply; and whether it took is confirmed by rendering a
      # transform URL, not by a clean apply. The newer lever may be
      # `transformations`.
      cloudflare_zone_setting.image_resizing = {
        zone_id = settings.cloudflareZone;
        setting_id = "image_resizing";
        value = "on";
      };

      # One ruleset entrypoint per phase, so the three zone rulesets share one
      # key: www redirects, CDN response headers, and the transform guard each
      # own a different phase.
      cloudflare_ruleset = {
        redirects = {
          zone_id = settings.cloudflareZone;
          name = "redirects";
          kind = "zone";
          phase = "http_request_dynamic_redirect";
          rules = [
            {
              description = "www to the apex";
              expression = "http.host eq \"www.${settings.organization}\"";
              action = "redirect";
              action_parameters.from_value = {
                status_code = 301;
                # The path carries over, so a link someone wrote with www still
                # lands where it meant to.
                target_url.expression = "concat(\"https://${settings.organization}\", http.request.uri.path)";
                preserve_query_string = true;
              };
            }
          ];
        };

        # Response headers on the CDN host, split by path so each lands where
        # it belongs. Response-phase rules are non-terminating, so all three
        # apply their ops in order, and they read request fields to tell the
        # transform path from the raw-master path.
        cdn_response_headers = {
          zone_id = settings.cloudflareZone;
          name = "cdn-response-headers";
          kind = "zone";
          phase = "http_response_headers_transform";
          rules = [
            # Defense in depth against active content in the WebAuthn RP scope,
            # on originals and transforms alike.
            {
              description = "nosniff on every CDN response";
              expression = "http.host eq \"${settings.cdnHost}\"";
              action = "rewrite";
              action_parameters.headers."X-Content-Type-Options" = {
                operation = "set";
                value = "nosniff";
              };
            }
            # Only the map canvas readback needs CORS, and it reads only
            # transform URLs. Narrowing the wildcard off the raw-master path
            # keeps it here rather than on a bucket-CORS resource, which also
            # sidesteps R2's Origin-conditional cache behavior.
            {
              description = "CORS on the transform path only";
              expression = "http.host eq \"${settings.cdnHost}\" and starts_with(http.request.uri.path, \"/cdn-cgi/image/\")";
              action = "rewrite";
              action_parameters.headers."Access-Control-Allow-Origin" = {
                operation = "set";
                value = "*";
              };
            }
            # Direct navigation to a raw master downloads instead of rendering,
            # so a scriptable master (SVG/PDF) that ever slipped the write-time
            # gate cannot execute in the RP scope. Analysis fetches originals
            # server-side, which ignores this, and no browser surface loads an
            # original inline (the lightbox uses the Detail transform).
            {
              description = "Force download on the original path";
              expression = "http.host eq \"${settings.cdnHost}\" and not starts_with(http.request.uri.path, \"/cdn-cgi/image/\")";
              action = "rewrite";
              action_parameters.headers."Content-Disposition" = {
                operation = "set";
                value = "attachment";
              };
            }
          ];
        };

        # Caps the billable-transformation surface: a /cdn-cgi/image/ request
        # whose options segment is not one of our fixed renditions is blocked,
        # so no caller can mint an unbounded set of sizes. Whether this firewall
        # phase intercepts /cdn-cgi/image/ ahead of the transformation is a
        # runtime check at apply, not something validate confirms.
        #
        # No host predicate here, unlike the response-header rules, and the
        # asymmetry is deliberate: this is cost and abuse protection that must
        # apply wherever transforms are enabled in the zone, the apex included.
        # The header rules scope to the cdn host because they describe the
        # images we serve, which live only there.
        cdn_transform_guard = {
          zone_id = settings.cloudflareZone;
          name = "cdn-transform-guard";
          kind = "zone";
          phase = "http_request_firewall_custom";
          rules = [
            {
              description = "Block transform options outside our renditions";
              expression =
                let
                  allowed = lib.concatMapStringsSep " or " (
                    opts: "starts_with(http.request.uri.path, \"/cdn-cgi/image/${opts}/\")"
                  ) cdnRenditionOptions;
                in
                "starts_with(http.request.uri.path, \"/cdn-cgi/image/\") and not (${allowed})";
              action = "block";
            }
          ];
        };
      };
    };
  };

  tfConfig = terranix.lib.terranixConfiguration {
    inherit pkgs;
    modules = [
      gcp
      cloudflare
    ];
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
