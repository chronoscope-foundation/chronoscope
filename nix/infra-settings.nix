# Chronoscope's cloud coordinates, defined once.
#
# nix/infra.nix declares the infrastructure from these values, and the justfile
# reads the same file back with `nix eval --file`, so the registry a deploy
# pushes to is the registry that was declared. There is no second copy to
# update.
#
# `rec` so a coordinate can be spelled from another: the CDN host is a
# subdomain of the served domain, and writing it as `cdn.${organization}`
# keeps the two from drifting.
rec {
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

  # The Cloud SQL instance, and the database on it holding this deployment's
  # data. The instance name is the last field of the connection name a client
  # dials (`project:region:instance`); the database name is what the DSN
  # selects, and an instance holds more than one of them.
  sqlInstance = "chronoscope-db";
  sqlDatabase = "chronoscope";

  # The Cloudflare account holding the zone, and the zone `organization` is
  # served from. Both are opaque ids the API addresses resources by.
  cloudflareAccount = "8eb131c87dedde28b0f5006b388ca800";
  cloudflareZone = "f2bfc494c8eb03fd4472a56b3f05598f";

  # Mirrored fact-store media. The bucket name is global to the R2 account;
  # the host is the subdomain a browser fetches an image from, where the
  # zone's transform path and header rules apply.
  cdnBucket = "chronoscope-media";
  cdnHost = "cdn.${organization}";

  # The mirror pipeline that fills that bucket. The mirror-dispatch binary POSTs
  # one fetch per image to the queue; the consumer Worker drains it into R2. A
  # message that exhausts its retries lands in the dead-letter queue, where a
  # persistent failure is visible until the queue's retention window elapses
  # (24h on the free plan) rather than silently lost.
  mirrorQueue = "chronoscope-mirror";
  mirrorDlq = "chronoscope-mirror-dlq";
  mirrorConsumerScript = "chronoscope-mirror-consumer";

  # Worker in front of everything: it serves the web bundle as static assets
  # and proxies /api to the Cloud Run service.
  workerScript = "chronoscope-front-door";

}
