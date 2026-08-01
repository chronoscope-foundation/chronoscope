# Chronoscope's cloud coordinates, defined once.
#
# nix/infra.nix declares the infrastructure from these values, and the justfile
# reads the same file back with `nix eval --file`, so the registry a deploy
# pushes to is the registry that was declared. There is no second copy to
# update.
{
  # The id is what gcloud and the console take; the number is how IAM
  # principals and service agent addresses are spelled.
  project = "chronoscope-io-prod";
  projectNumber = "379979227839";

  organization = "chronoscope.io";
  organizationId = "183691898804";

  # One region for everything regional: the registry, and the Cloud Run service
  # that pulls from it.
  region = "us-central1";

  # Docker repository the API image is pushed to.
  artifactRepository = "chronoscope";

  # Holds the OpenTofu state. Created by hand, since state describing the
  # bucket would have to live in the bucket. See the justfile's infra section.
  stateBucket = "chronoscope-io-tfstate";

  # Cloud Run service `just deploy` rolls. Also names the runtime service
  # account and the JWT secret, which exist only to serve it.
  cloudRunService = "chronoscope-api";

  # The Cloudflare account holding the zone, and the zone `organization` is
  # served from. Both are opaque ids the API addresses resources by.
  cloudflareAccount = "8eb131c87dedde28b0f5006b388ca800";
  cloudflareZone = "f2bfc494c8eb03fd4472a56b3f05598f";

  # Worker in front of everything: it serves the web bundle as static assets
  # and proxies /api to the Cloud Run service.
  workerScript = "chronoscope-front-door";

  # Secret Manager secret holding the Cloudflare API token. Created by hand,
  # like the state bucket, since the credential that declares infrastructure
  # cannot be declared by it. The infra recipes read it at run time and hand it
  # to the provider through the environment, so it stays out of the state and
  # off every command line.
  cloudflareTokenSecret = "cloudflare-api-token";
}
