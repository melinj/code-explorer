import { useEffect, useRef } from "react";

/**
 * Call `onClose` on Escape. Subscribed once: re-subscribing on every render (callers pass a new
 * arrow each time) can drop the key, when another keydown handler re-renders the tree mid-dispatch.
 */
export function useEscape(onClose: () => void) {
  const ref = useRef(onClose);
  ref.current = onClose;
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && ref.current();
    window.addEventListener("keydown", esc);
    return () => window.removeEventListener("keydown", esc);
  }, []);
}
