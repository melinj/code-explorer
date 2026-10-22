/**
 * Invoice arithmetic shared by the editor preview and the PDF export, so that both round the
 * same way: tax is worked out per line and rounded to whole cents before anything is summed.
 *
 * @param {Array<{quantity: number, unitCents: number, taxPercent?: number}>} lines
 * @returns {{netCents: number, taxCents: number, grossCents: number}}
 */
export function invoiceTotals(lines) {
  let netCents = 0;
  let taxCents = 0;
  for (const line of lines) {
    const net = Math.round(line.quantity * line.unitCents);
    netCents += net;
    taxCents += Math.round((net * (line.taxPercent ?? 0)) / 100);
  }
  return { netCents, taxCents, grossCents: netCents + taxCents };
}

/**
 * Customer id → cents still owed, for the "outstanding" column of the customer list. Paid
 * and voided invoices owe nothing; part-paid ones owe what is left.
 */
export function outstandingByCustomer(invoices) {
  const owed = {};
  for (const inv of invoices) {
    if (inv.status === 'paid' || inv.status === 'void') continue;
    owed[inv.customerId] = (owed[inv.customerId] ?? 0) + inv.totalCents - inv.paidCents;
  }
  return owed;
}
