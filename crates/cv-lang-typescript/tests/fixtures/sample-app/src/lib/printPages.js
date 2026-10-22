/**
 * Splits invoice lines into printed pages. A page has room for `rowsPerPage` rows; a line
 * whose description wraps takes more than one row, and no line is split across two pages.
 * Returns the pages in print order, each an array of lines.
 */
export function paginateLines(lines, rowsPerPage, charsPerRow = 48) {
  const pages = [];
  let page = [];
  let used = 0;

  for (const line of lines) {
    const rows = Math.max(1, Math.ceil(line.description.length / charsPerRow));
    if (used + rows > rowsPerPage && page.length > 0) {
      pages.push(page);
      page = [];
      used = 0;
    }
    page.push(line);
    used += rows;
  }

  if (page.length > 0) {
    pages.push(page);
  }

  return pages;
}
