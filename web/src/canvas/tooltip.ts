/** One reusable DOM tooltip over the canvas; content changes only when the hover target does. */
export class Tooltip {
  readonly el = document.createElement("div");
  private key = "";

  constructor() {
    this.el.className = "cv-tip";
    this.el.hidden = true;
  }

  show(key: string, html: () => string, x: number, y: number) {
    if (key !== this.key) {
      this.key = key;
      this.el.innerHTML = html();
    }
    this.el.hidden = false;
    this.move(x, y);
  }

  move(x: number, y: number) {
    if (this.el.hidden) return;
    const host = this.el.parentElement!;
    // Keep the tip inside the canvas: flip left/up near the right/bottom edges.
    const w = this.el.offsetWidth;
    const h = this.el.offsetHeight;
    const left = x + 16 + w > host.clientWidth ? x - 12 - w : x + 16;
    const top = y + 18 + h > host.clientHeight ? y - 12 - h : y + 18;
    this.el.style.transform = `translate(${Math.max(4, left)}px, ${Math.max(4, top)}px)`;
  }

  hide() {
    this.el.hidden = true;
    this.key = "";
  }
}
