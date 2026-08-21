export interface DialogTrapOptions {
  onEscape: () => void;
  escapeDisabled?: boolean;
}

/** Keep keyboard focus inside a modal, support Escape, and restore focus. */
export function trapDialog(node: HTMLElement, initial: DialogTrapOptions) {
  let options = initial;
  const previousFocus =
    document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const focusable = () =>
    Array.from(
      node.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((element) => !element.hasAttribute("hidden"));
  const focusInitial = () => {
    if (!node.isConnected || node.contains(document.activeElement)) return;
    (focusable()[0] ?? node).focus();
  };
  const onKeydown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      if (options.escapeDisabled) return;
      event.preventDefault();
      event.stopPropagation();
      options.onEscape();
      return;
    }
    if (event.key !== "Tab") return;
    const candidates = focusable();
    if (candidates.length === 0) {
      event.preventDefault();
      node.focus();
      return;
    }
    const first = candidates[0];
    const last = candidates[candidates.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  };
  node.addEventListener("keydown", onKeydown);
  queueMicrotask(focusInitial);
  return {
    update(next: DialogTrapOptions) {
      options = next;
    },
    destroy() {
      node.removeEventListener("keydown", onKeydown);
      if (previousFocus?.isConnected) previousFocus.focus();
    },
  };
}
