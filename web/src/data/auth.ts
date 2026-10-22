/**
 * Bearer token for the local server (DESIGN §10). `cv serve` prints a URL with ?token=…; the token
 * is removed from the address bar and kept in this origin's localStorage, so new tabs and bookmarks
 * of the same server work. Storage is per origin (host and port), unlike a cookie, which the
 * browser would send to every other server on 127.0.0.1. Every /api request carries it.
 */
const KEY = "cv-token";

export function token(): string | undefined {
  try {
    const url = new URL(location.href);
    const t = url.searchParams.get("token");
    if (t) {
      localStorage.setItem(KEY, t);
      url.searchParams.delete("token");
      history.replaceState(null, "", url);
      return t;
    }
    return localStorage.getItem(KEY) ?? undefined;
  } catch {
    return new URL(location.href).searchParams.get("token") ?? undefined;
  }
}

const SESSION_KEY = "cv-session";
let memSession: string | undefined;

/**
 * A stable id for this browser tab (§10 per-session permissions): kept in sessionStorage, or in
 * memory where storage is unavailable. Sent as X-CV-Session and as ?session= on the WebSocket.
 */
export function session(): string {
  const fresh = () => (crypto.randomUUID?.() ?? `s${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`);
  try {
    let s = sessionStorage.getItem(SESSION_KEY);
    if (!s) sessionStorage.setItem(SESSION_KEY, (s = fresh()));
    return s;
  } catch {
    return (memSession ??= fresh());
  }
}

/** Add the Authorization (when a token is known) and X-CV-Session headers to same-origin /api requests. */
export function installAuth() {
  const t = token();
  const sid = session();
  const orig = window.fetch.bind(window);
  window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const u = new URL(url, location.href);
    if (u.origin === location.origin && u.pathname.startsWith("/api/")) {
      const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
      if (t) headers.set("Authorization", `Bearer ${t}`);
      headers.set("X-CV-Session", sid);
      return orig(input, { ...init, headers });
    }
    return orig(input, init);
  };
}
