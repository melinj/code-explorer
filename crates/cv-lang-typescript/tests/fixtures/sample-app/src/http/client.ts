/// <reference types="vite/client" />

import { authorizedFetch, restoreSession } from '../auth/session';

// A token saved by an earlier tab has to be in place before the first request leaves.
restoreSession();

const BASE_URL: string = import.meta.env.VITE_BILLING_API ?? '/api/billing';

export const REQUEST_TIMEOUT_MS = 15_000;
export const RETRY_LIMIT = 2;

/** Requests started and not yet settled; the header spinner shows while this is non-zero. */
let inFlight = 0;

export function pendingRequests(): number {
  return inFlight;
}

/** Options for `request`: fetch's own, plus an idempotency key for POSTs that move money. */
export interface FetchOptions extends RequestInit {
  signal?: AbortSignal | null;
  idempotencyKey?: string;
}

/**
 * A non-2xx answer. Keeps the status and the path, so a screen can tell "not found" from
 * "conflict" without reading the message.
 */
export class HttpError extends Error {
  readonly status: number;
  readonly path: string;

  constructor(status: number, path: string, detail: string) {
    super(`${path} answered ${status}: ${detail}`);
    this.name = 'HttpError';
    this.status = status;
    this.path = path;
  }

  /** Throttling and server faults may pass on a later try; client errors will not. */
  get retryable(): boolean {
    return this.status === 429 || this.status >= 500;
  }
}

// Exposed for the retry tests only.
export { sendWithRetries, backoff };

/**
 * An abort signal that fires after `ms`, or as soon as `outer` does. `cancel()` stops the timer
 * and detaches from `outer`; call it once the response is in.
 */
function deadline(
  ms: number,
  outer?: AbortSignal | null,
): { signal: AbortSignal; cancel: () => void } {
  const controller = new AbortController();
  const timeout = new DOMException(`no answer within ${ms} ms`, 'TimeoutError');
  const timer = setTimeout(() => controller.abort(timeout), ms);
  const forward = () => controller.abort(outer?.reason);
  if (outer?.aborted) forward();
  outer?.addEventListener('abort', forward, { once: true });
  return {
    signal: controller.signal,
    cancel: () => {
      clearTimeout(timer);
      outer?.removeEventListener('abort', forward);
    },
  };
}

/** Exponential backoff with full jitter: 0–400 ms, then 0–800 ms, … */
function backoff(attempt: number): {
  delayMs: number;
  last: boolean;
} {
  return {
    delayMs: Math.random() * 400 * 2 ** attempt,
    last: attempt >= RETRY_LIMIT,
  };
}

/**
 * Sends `init` to `url`, retrying network failures, timeouts, 429 and 5xx answers up to
 * RETRY_LIMIT times. Resolves with the parsed JSON body, or null for a 204.
 */
async function sendWithRetries(
  url: string,
  init: RequestInit & { signal?: AbortSignal | null },
): Promise<unknown> {
  for (let attempt = 0; ; attempt++) {
    const { signal, cancel } = deadline(REQUEST_TIMEOUT_MS, init.signal);
    let failure: unknown;
    try {
      const res = await authorizedFetch(url, { ...init, signal });
      if (res.ok) return res.status === 204 ? null : await res.json();
      failure = new HttpError(res.status, url, await res.text());
    } catch (err) {
      failure = err;
    } finally {
      cancel();
    }
    // A caller that gave up is not retried, whatever went wrong.
    if (init.signal?.aborted) throw failure;
    const retryable = failure instanceof HttpError ? failure.retryable : true;
    const { delayMs, last } = backoff(attempt);
    if (last || !retryable) throw failure;
    await new Promise((wake) => setTimeout(wake, delayMs));
  }
}

/** Prefixes BASE_URL, sets the JSON and idempotency headers, and sends with retries. */
export async function request(path: string, options: FetchOptions = {}): Promise<unknown> {
  const { idempotencyKey, ...init } = options;
  const headers = new Headers(init.headers);
  if (init.body !== undefined && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (idempotencyKey) headers.set('Idempotency-Key', idempotencyKey);
  inFlight++;
  try {
    return await sendWithRetries(`${BASE_URL}${path}`, { ...init, headers });
  } finally {
    inFlight--;
  }
}
