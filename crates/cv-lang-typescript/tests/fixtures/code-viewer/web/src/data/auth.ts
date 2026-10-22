/**
 * Bearer token for the local server (DESIGN §10). `cv serve` prints a URL with ?token=…; the token
 * is kept for the tab's session and removed from the address bar. Every /api request carries it.
 */
const KEY = "cv-token";

export function token(): string | undefined {
  try {
    const url = new URL(location.href);
    const t = url.searchParams.get("token");
    if (t) {
      sessionStorage.setItem(KEY, t);
      url.searchParams.delete("token");
      history.replaceState(null, "", url);
      return t;
    }
    return sessionStorage.getItem(KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

/** Add the Authorization header to same-origin /api requests made with fetch on the main thread. */
export function installAuth() {
  const t = token();
  if (!t) return;
  const orig = window.fetch.bind(window);
  window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const u = new URL(url, location.href);
    if (u.origin === location.origin && u.pathname.startsWith("/api/")) {
      const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
      headers.set("Authorization", `Bearer ${t}`);
      return orig(input, { ...init, headers });
    }
    return orig(input, init);
  };
}
