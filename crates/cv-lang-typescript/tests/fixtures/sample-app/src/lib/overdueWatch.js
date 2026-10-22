// The overdue count behind the header badge. The store owns the invoices; this only watches.

import { useBillingStore } from '../state/billingStore';

const countOverdue = (invoices) => invoices.filter((inv) => inv.status === 'overdue').length;

/**
 * Calls `onChange(count)` now, and again whenever the number of overdue invoices changes.
 *
 * @param {(count: number) => void} onChange
 * @returns {() => void} Stops watching
 */
export function watchOverdue(onChange) {
  let last = -1;
  const check = () => {
    const count = countOverdue(useBillingStore.getState().invoices);
    if (count !== last) {
      last = count;
      onChange(count);
    }
  };
  check();
  return useBillingStore.subscribe(check);
}
