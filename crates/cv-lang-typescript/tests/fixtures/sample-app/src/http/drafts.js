import dayjs from 'dayjs';
import * as money from '../lib/money.js';
import { DEFAULT_PAYMENT_TERMS_DAYS } from '../lib/settings';
import { createInvoice } from './invoices';

/**
 * Saves the editor's draft as an invoice on the server.
 *
 * The draft is what the form holds: prices as typed, and possibly empty rows. Empty rows are
 * dropped here, so the server never sees them.
 *
 * @param {{ customer?: { id: string }, lines?: Array<object> }} draft
 * @param {Array<object>} [extraLines] - Rows added outside the form, such as a late fee
 * @returns {Promise<object>} The created invoice
 */
export async function submitDraftInvoice(draft, extraLines = []) {
  const customerId = draft?.customer?.id ?? '';
  const rows = [...(draft?.lines ?? []), ...extraLines];
  const lines = rows.filter((row) => row.description?.trim()).map(toLineItem);

  return createInvoice(customerId, lines);
}

/** One form row → the API's line shape. A price that does not parse counts as 0, not NaN. */
export function toLineItem(row) {
  return {
    description: row.description.trim(),
    quantity: Number(row.quantity) || 1,
    unitCents: money.toCents(row.unitPrice) ?? 0,
    taxRateId: row.taxRateId ?? null,
  };
}

/** The due date printed on an invoice issued on `issuedOn` (an ISO date). */
export function dueDate(issuedOn, termsDays = DEFAULT_PAYMENT_TERMS_DAYS) {
  return dayjs(issuedOn).add(termsDays, 'day').format('YYYY-MM-DD');
}
