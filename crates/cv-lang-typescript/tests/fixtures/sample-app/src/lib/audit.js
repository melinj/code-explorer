// Audit trail of billing actions: who sent, voided or paid what. Entries are echoed to the
// console in development and beaconed to VITE_AUDIT_URL when that is set.

const AUDIT_URL = import.meta.env.VITE_AUDIT_URL ?? '';
const ECHO = import.meta.env.DEV || import.meta.env.VITE_AUDIT_ECHO === '1';

function record(action, subjectId, ...details) {
  const entry = { action, subjectId, details, at: new Date().toISOString() };
  if (ECHO) console.info(`[audit] ${action} ${subjectId}`, ...details);
  if (AUDIT_URL) navigator.sendBeacon(AUDIT_URL, JSON.stringify(entry));
  return entry;
}

export const audit = {
  invoiceSent: (invoiceId, tone) => record('invoice.sent', invoiceId, tone),
  invoiceVoided: (invoiceId, reason) => record('invoice.voided', invoiceId, reason),
  paymentRecorded: (invoiceId, cents) => record('payment.recorded', invoiceId, cents),
  remindersQueued: (count) => record('reminders.queued', '*', count),
};
