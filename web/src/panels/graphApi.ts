/** Fetch JSON from /api/v1, throwing the server's message on a non-2xx answer. */
export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch("/api/v1" + path, init);
  const b = await r.json().catch(() => ({}));
  if (!r.ok) {
    const e = b.error ?? b;
    throw new Error(e.message ?? r.statusText);
  }
  return b as T;
}

export const post = <T,>(path: string, body: unknown) => api<T>(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
