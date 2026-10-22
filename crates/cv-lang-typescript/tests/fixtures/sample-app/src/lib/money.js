/**
 * Money helpers. Amounts travel as integer cents; only the UI turns them into text.
 */

/** What someone typed into a price field ("12,50", "12.5", " 3 ") → cents, or null. */
export function toCents(input) {
  const text = String(input ?? '').trim().replace(',', '.');
  if (text === '') return null;
  const value = Number(text);
  return Number.isFinite(value) ? Math.round(value * 100) : null;
}

/**
 * @param {number|null|undefined} cents
 * @param {string} [symbol='€'] - Currency symbol written before the amount
 * @param {string} [blank=''] - Text for an amount that is not known yet
 * @returns {string} e.g. "€12.50", "-€3.00", or `blank`
 */
export function formatCents(cents, symbol = '€', blank = '') {
  if (cents == null) return blank;
  const sign = cents < 0 ? '-' : '';
  return `${sign}${symbol}${(Math.abs(cents) / 100).toFixed(2)}`;
}
