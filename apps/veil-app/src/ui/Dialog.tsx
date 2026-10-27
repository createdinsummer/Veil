import type { FormEventHandler, ReactNode } from "react";

type DialogProps = {
  label: string;
  children: ReactNode;
  className?: string;
} & ({ onSubmit: FormEventHandler<HTMLFormElement> } | { onSubmit?: never });

export function Dialog({ label, children, className = "", onSubmit }: DialogProps) {
  const props = { className: `modal ${className}`.trim(), role: "dialog", "aria-modal": true as const, "aria-label": label };
  return <div className="modal-backdrop">
    {onSubmit ? <form {...props} onSubmit={onSubmit}>{children}</form> : <div {...props}>{children}</div>}
  </div>;
}
