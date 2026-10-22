// Identifiers the client makes up itself. Creating an invoice or recording a payment sends an
// Idempotency-Key, so a POST retried after a timeout cannot bill anyone twice; an unsaved
// draft gets a local id so the editor can track it before the server knows about it.

/**
 * `count` random bytes as lowercase hex: Web Crypto where there is one, Math.random()
 * otherwise (some embedded browsers and test runners), which is weaker but unique enough here.
 */
function hexBytes(count: number): string {
  const bytes = new Uint8Array(count);
  try {
    crypto.getRandomValues(bytes);
  } catch {
    for (let i = 0; i < count; i++) {
      bytes[i] = Math.floor(Math.random() * 256);
    }
  }
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

let draftCounter = 0;

/** A fresh key for one logical POST: reuse it for that POST's retries, never for another. */
export function newIdempotencyKey(): string {
  return `idem-${Date.now().toString(36)}-${hexBytes(8)}`;
}

/** A local id for an unsaved draft, unique within this tab. */
export function newDraftId(): string {
  draftCounter += 1;
  return `draft-${draftCounter}-${hexBytes(4)}`;
}

/** Test hook: numbers drafts from 1 again. */
export function resetDraftIds(): void {
  draftCounter = 0;
}
