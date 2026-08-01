// The front door for chronoscope.io: one origin serving both the web bundle
// and the API.
//
// Cloudflare serves anything matching a file in the bundle before this code
// runs, so the wasm, the JS glue, the CSS and the fonts never pay for a Worker
// invocation. What reaches here is /api/*, which the assets config routes to
// the Worker first, plus the asset misses the SPA fallback did not absorb.
//
// One origin is what removes CORS from the browser, and it is why there is no
// api. subdomain. The API owns its routes at root, so the /api mount comes off
// here — the same rewrite trunk performs with --proxy-rewrite for the dev
// server, and the browser tests' proxy_api handler for the test harness.

const MOUNT = "/api";

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname !== MOUNT && !url.pathname.startsWith(MOUNT + "/")) {
      return env.ASSETS.fetch(request);
    }

    // API_ORIGIN is bound by terraform from the Cloud Run service it declares,
    // so the API's hostname is never written down a second time.
    const target = new URL(env.API_ORIGIN);
    target.pathname = url.pathname.slice(MOUNT.length) || "/";
    target.search = url.search;
    // The subrequest's Host follows its URL, which is how Cloud Run recognizes
    // the service being addressed.
    return fetch(new Request(target, request));
  },
};
