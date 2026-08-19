// Mirror consumer: fetch one upstream image and store it in R2.
//
// JavaScript for simplicity while it stays this small. It is glue: every policy
// the fetch needs travels in the message (see MirrorRequest in
// integrations/src/mirror.rs), so this file is a fetch-check-put that carries no
// domain knowledge of its own. Move it to Rust/WASM via workers-rs if it grows
// past that.
//
// Conceptual twin: `consume` in dev/src/mirror_consumer.rs is the same
// fetch-check-put for the dev in-process queue (Rust, no Cloudflare). Two trivial
// implementations because one runs here in a Worker and one runs in-process;
// while both stay this small the duplication is cheaper than a shared core, so a
// change to one is a prompt to check the other. The dev twin retries in-job
// instead of re-injecting, and skips the redirect/RP-scope guards it has no
// boundary to defend.
//
// Bindings (declared in nix/infra.nix): MEDIA_BUCKET is the R2 bucket; the
// queue consumer config points this script at the mirror queue.

// The message shape this understands. A message stamped with anything else was
// produced by a different dispatcher version; the shape changed under it, so
// there is nothing safe to do but surface it.
const MESSAGE_VERSION = 1;

export default {
  async queue(batch, env) {
    // Per-message ack/retry: one poison message must not fail its batchmates.
    for (const message of batch.messages) {
      try {
        await mirror(message, env);
      } catch (err) {
        // An unexpected throw is treated as transient: retry, and Cloudflare
        // dead-letters it after the queue's max_retries.
        console.error(`mirror threw for ${describe(message)}: ${err}`);
        message.retry();
      }
    }
  },
};

async function mirror(message, env) {
  const req = message.body;

  if (req.v !== MESSAGE_VERSION) {
    // Retry so it reaches the dead-letter queue, where a version mismatch is
    // visible. It cannot succeed here, but a mismatch means a bad deploy
    // ordering worth seeing rather than silently dropping.
    console.error(`unknown message version ${req.v} for key ${req.key}`);
    message.retry();
    return;
  }

  // Already warmed: the key exists, so we are done without fetching. This is
  // what makes re-dispatching the whole corpus cheap.
  if (await env.MEDIA_BUCKET.head(req.key)) {
    message.ack();
    return;
  }

  const res = await fetch(req.url, {
    headers: { "User-Agent": req.user_agent },
    // Rust approved this exact URL. A redirect points somewhere it did not, so
    // never follow one; treat any 3xx as a failure below.
    redirect: "manual",
  });

  if (res.status === 429) {
    // Honor the upstream's own pace. Wikimedia answers a burst with a
    // Retry-After; delaying the retry maps onto that. A missing header reads as
    // null, whose Number() is 0, so guard against it: fall back to a minute
    // rather than retry instantly and burn every attempt in seconds. An
    // HTTP-date Retry-After parses to NaN and also falls back. Cap at the
    // queue's 12h delay ceiling so a huge value cannot throw.
    const header = res.headers.get("Retry-After");
    const parsed = header === null ? NaN : Number(header);
    const seconds = Number.isFinite(parsed) && parsed > 0 ? Math.min(parsed, 43200) : 60;
    message.retry({ delaySeconds: seconds });
    return;
  }

  if (res.status >= 300 && res.status < 400) {
    // A URL Rust approved now redirects. Do not chase it; log and drop, since
    // retrying the same URL will redirect again.
    console.error(`refusing redirect ${res.status} for ${req.url}`);
    message.ack();
    return;
  }

  if (res.status >= 500) {
    message.retry();
    return;
  }

  if (!res.ok) {
    // 4xx other than 429: the resource is gone or forbidden. Retrying will not
    // fix it, so log and drop rather than loop to the dead-letter queue.
    console.error(`upstream ${res.status} for ${req.url}`);
    message.ack();
    return;
  }

  const contentType = (res.headers.get("Content-Type") || "").split(";")[0].trim().toLowerCase();
  if (!req.accept.includes(contentType)) {
    // The extension was a claim; this is the true type, and it is not one we
    // serve. Refusing here is what keeps a mislabeled active document out of
    // the RP-scope host. A data problem, not a transient one: log and drop.
    console.error(`refusing content type ${contentType} for ${req.url}`);
    message.ack();
    return;
  }

  // A missing Content-Length reads as null, whose Number() is 0, so require an
  // honest positive length within the cap. Refuse the rest: a chunked response
  // R2.put could not bound, an empty body that would store zero bytes and then
  // be made permanent by the warm-skip above, and a scan over the cap. Commons
  // serves a length on every file, so this refuses only the pathological cases.
  const declared = Number(res.headers.get("Content-Length"));
  if (!Number.isFinite(declared) || declared <= 0 || declared > req.max_bytes) {
    console.error(
      `refusing length ${res.headers.get("Content-Length")} (cap ${req.max_bytes}) for ${req.url}`,
    );
    message.ack();
    return;
  }

  if (res.body === null) {
    // A 200 that cleared the length and type gates but carries no body to read
    // is a malformed response, not a transient one. Drop it like the other
    // bad-response branches rather than letting the put below throw and retry.
    console.error(`no body despite length ${declared} for ${req.url}`);
    message.ack();
    return;
  }

  // First write wins: If-None-Match "*" stores only when the key is absent, so
  // two deliveries of one key cannot overwrite each other. The Headers form is
  // the documented wildcard; the R2Conditional struct matches an etag literally
  // and would not. A null return means the precondition failed (a concurrent
  // delivery won the race), which is success from our side.
  //
  // The body is held to exactly the declared length as it streams, not only
  // bounded above by it: an upstream that understates its Content-Length would
  // otherwise stream an oversized object into paid storage, and one that closes
  // short would store a truncated one the warm-skip then locks in. Either
  // mismatch errors the put's source, and R2 writes no object from a put whose
  // body errored, so only a body whose byte count matches its length lands.
  await env.MEDIA_BUCKET.put(req.key, checkedBody(res.body, declared), {
    onlyIf: new Headers({ "If-None-Match": "*" }),
    httpMetadata: { contentType },
  });
  message.ack();
}

// Hold the streamed bytes to exactly `declared`: a FixedLengthStream errors on a
// run past that count or a close short of it, and R2.put writes no object when its
// source errors, so a truncated or oversized response never lands. It is the
// fixed-length stream specifically because R2.put requires a body whose length is
// known up front, which its readable half carries. The length gate above already
// bounded `declared` to the cap, so exactness enforces the cap too.
function checkedBody(body, declared) {
  return body.pipeThrough(new FixedLengthStream(declared));
}

function describe(message) {
  const key = message.body && message.body.key;
  return key ? `key ${key}` : `message ${message.id}`;
}
