import type { schemas } from '../types/billing-api.d';
import { HttpError, request } from './client';
import { assertFields } from './assertFields';
import { newIdempotencyKey } from '../lib/ids';

// Every call the invoice screens make. Shapes come from the generated OpenAPI types; each read
// checks the fields the screens rely on, so contract drift fails here and not in a render.

type Organization = schemas['Organization'];
type Customer = schemas['Customer'];
type InvoicePage = schemas['InvoicePage'];
type Payment = schemas['Payment'];
type TaxRate = schemas['TaxRate'];
export type InvoiceStatus = schemas['InvoiceStatus'];
export type ReminderTone = 'gentle' | 'firm' | 'final';

/** A row of the invoice list. Amounts are integer cents in the organisation's currency. */
export interface InvoiceSummary {
  id: string;
  number: string;
  customerName: string;
  status: InvoiceStatus;
  totalCents: number;
  dueOn: string | null;
}

/** One line as the editor holds it; the server works out the line total. */
export interface LineItem {
  description: string;
  quantity: number;
  unitCents: number;
  taxRateId: string | null;
}

/** An invoice with everything the editor and the PDF preview need. */
export interface InvoiceDetail extends InvoiceSummary {
  lines: LineItem[];
  notes: string;
  payments: Payment[];
}

/** How many customers the search box offers at once. */
const SEARCH_LIMIT = 8;

/**
 * The customer page's invoice tab. Pages count from 1; `status` narrows the list to one
 * status, and its absence means every status.
 */
interface CustomerInvoicesQuery {
  page: number;
  status?: InvoiceStatus;
}

/** The letterhead: name, address and currency printed on every invoice. */
export async function fetchOrganization(signal?: AbortSignal): Promise<Organization> {
  const body = await request('/organization', { signal });
  assertFields(body, ['name', 'currency', 'address'], 'fetchOrganization');
  return body as Organization;
}

/** The first page of invoices in one status, newest first. */
export async function listInvoices(
  status: InvoiceStatus,
  signal?: AbortSignal,
): Promise<InvoicePage> {
  const body = await request(`/invoices?status=${status}`, { signal });
  assertFields(body, ['items', 'nextCursor'], 'listInvoices');
  return body as InvoicePage;
}

export async function listCustomerInvoices(
  customerId: string,
  query: CustomerInvoicesQuery,
  signal?: AbortSignal,
): Promise<InvoicePage> {
  const body = await request(`/customers/${customerId}/invoices?page=${query.page}`, {
    signal,
  });
  assertFields(body, ['items', 'nextCursor'], 'listCustomerInvoices');
  const page = body as InvoicePage;
  if (!query.status) return page;
  return { ...page, items: page.items.filter((i) => i.status === query.status) };
}

export async function searchCustomers(text: string, signal?: AbortSignal): Promise<Customer[]> {
  const q = encodeURIComponent(text.trim());
  const body = await request(`/customers?q=${q}&limit=${SEARCH_LIMIT}`, { signal });
  assertFields(body, ['customers'], 'searchCustomers');
  return (body as { customers: Customer[] }).customers;
}

/** Creates the invoice as a draft; it gets its number when it is first sent. */
export async function createInvoice(customerId: string, lines: LineItem[]): Promise<InvoiceDetail> {
  return request('/invoices', {
    method: 'POST',
    body: JSON.stringify({ customerId, lines }),
    idempotencyKey: newIdempotencyKey(),
  }) as Promise<InvoiceDetail>;
}

export async function updateLines(invoiceId: string, lines: LineItem[]): Promise<InvoiceDetail> {
  return request(`/invoices/${invoiceId}/lines`, {
    method: 'PUT',
    body: JSON.stringify({ lines }),
  }) as Promise<InvoiceDetail>;
}

/** Only drafts can be deleted; a sent invoice is voided instead. */
export async function deleteDraft(invoiceId: string): Promise<void> {
  await request(`/invoices/${invoiceId}`, { method: 'DELETE' });
}

export async function sendInvoice(
  invoiceId: string,
  tone: ReminderTone = 'gentle',
): Promise<InvoiceSummary> {
  return request(`/invoices/${invoiceId}/send`, {
    method: 'POST',
    body: JSON.stringify({ tone }),
  }) as Promise<InvoiceSummary>;
}

/**
 * Queues a reminder for every overdue invoice whose customer has not had one this week.
 * Resolves with how many were queued and the numbers of the invoices skipped.
 */
export async function remindOverdue(): Promise<{ queued: number; skipped: string[] }> {
  return request('/invoices/remind-overdue', {
    method: 'POST',
  }) as Promise<{ queued: number; skipped: string[] }>;
}

export async function recordPayment(
  invoiceId: string,
  payment: Omit<Payment, 'id'>,
): Promise<Payment> {
  return request(`/invoices/${invoiceId}/payments`, {
    method: 'POST',
    body: JSON.stringify(payment),
    idempotencyKey: newIdempotencyKey(),
  }) as Promise<Payment>;
}

export async function fetchTaxRates(signal?: AbortSignal): Promise<TaxRate[]> {
  const body = await request('/tax-rates', { signal });
  assertFields(body, ['rates'], 'fetchTaxRates');
  return (body as { rates: TaxRate[] }).rates;
}

/**
 * One invoice with its lines, or null when there is no such invoice: links in old
 * notifications can outlive a deleted draft.
 */
export async function findInvoice(
  invoiceId: string,
  expand: 'payments' | 'none',
  signal?: AbortSignal,
): Promise<InvoiceDetail | null> {
  try {
    const body = await request(`/invoices/${invoiceId}?expand=${expand}`, {
      signal,
    });
    assertFields(body, ['id', 'number', 'status', 'lines'], 'findInvoice');
    return body as InvoiceDetail;
  } catch (err) {
    if (err instanceof HttpError && err.status === 404) return null;
    throw err;
  }
}
