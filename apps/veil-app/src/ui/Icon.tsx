import type { ReactNode } from "react";

export function Icon({ name, size = 20 }: { name: string; size?: number }) {
  const paths: Record<string, ReactNode> = {
    shield: (
      <>
        <path d="M12 2.8 19 5.5v5.7c0 4.4-2.8 7.7-7 9.8-4.2-2.1-7-5.4-7-9.8V5.5z" />
        <path d="m9.2 11.8 1.9 1.9 3.8-4" />
      </>
    ),
    grid: (
      <>
        <rect x="3" y="3" width="7" height="7" rx="1.5" />
        <rect x="14" y="3" width="7" height="7" rx="1.5" />
        <rect x="3" y="14" width="7" height="7" rx="1.5" />
        <rect x="14" y="14" width="7" height="7" rx="1.5" />
      </>
    ),
    plus: <path d="M12 5v14M5 12h14" />,
    folder: (
      <>
        <path d="M3 6.5A2.5 2.5 0 0 1 5.5 4H10l2 2h6.5A2.5 2.5 0 0 1 21 8.5v9A2.5 2.5 0 0 1 18.5 20h-13A2.5 2.5 0 0 1 3 17.5z" />
      </>
    ),
    arrow: <path d="M5 12h14m-6-6 6 6-6 6" />,
    left: <path d="M19 12H5m6-6-6 6 6 6" />,
    lock: (
      <>
        <rect x="5.5" y="10" width="13" height="10.5" rx="2" />
        <path d="M8.5 10V7a3.5 3.5 0 0 1 7 0v3M12 14v3" />
      </>
    ),
    unlock: (
      <>
        <rect x="5.5" y="10" width="13" height="10.5" rx="2" />
        <path d="M8.5 10V7a3.5 3.5 0 0 1 6.8-1.1M12 14v3" />
      </>
    ),
    file: (
      <>
        <path d="M6 3h8l4 4v14H6z" />
        <path d="M14 3v5h4M9 12h6M9 16h6" />
      </>
    ),
    refresh: (
      <>
        <path d="M20 11a8 8 0 0 0-14-5L4 8" />
        <path d="M4 4v4h4M4 13a8 8 0 0 0 14 5l2-2" />
        <path d="M20 20v-4h-4" />
      </>
    ),
    check: <path d="m5 12 4 4L19 6" />,
    list: <><path d="M9 6h11M9 12h11M9 18h11"/><path d="M4 6h.1M4 12h.1M4 18h.1"/></>,
    image: <><rect x="3" y="3" width="18" height="18" rx="3"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="m4 18 5-5 3 3 3-4 5 6"/></>,
    video: <><rect x="3" y="5" width="14" height="14" rx="2"/><path d="m17 10 4-2v8l-4-2z"/></>,
    archive: <><rect x="4" y="5" width="16" height="15" rx="2"/><path d="M3 5h18M10 10h4"/></>,
    download: <><path d="M12 3v12m-4-4 4 4 4-4"/><path d="M4 16v4h16v-4"/></>,
    trash: <><path d="M4 7h16M9 7V4h6v3M7 7l1 13h8l1-13M10 11v5m4-5v5"/></>,
    close: <path d="M5 5l14 14M19 5 5 19"/>,
    chevron: <path d="m9 6 6 6-6 6"/>,
    clock: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l3 2" />
      </>
    ),
  };
  return (
    <svg
      aria-hidden="true"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {paths[name]}
    </svg>
  );
}

