import type { ButtonHTMLAttributes } from "react";

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "danger";
};

export function Button({ variant = "secondary", className = "", ...props }: ButtonProps) {
  return <button className={`button ${variant} ${className}`.trim()} {...props} />;
}
