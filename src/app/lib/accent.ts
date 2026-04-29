/**
 * Accent color helpers for the R2 Client redesign.
 *
 * The 5 hand-picked accents come from the design handoff. Each entry
 * carries the soft / hover / pressed color shades so we can flip every
 * accent CSS variable in lockstep when the user picks a new accent.
 */

export type AccentHex = string;

export interface AccentMeta {
  name: string;
  /** Translucent fill for selection backgrounds */
  soft: string;
  /** Translucent fill for hover-on-selected */
  softHover: string;
  /** Lighter shade for hover */
  hover: string;
  /** Darker shade for active/pressed */
  pressed: string;
  /** Text color that sits on the accent (button label) */
  ink: string;
}

export const ACCENTS: Record<AccentHex, AccentMeta> = {
  '#F38020': {
    name: 'Cloudflare orange',
    soft: 'rgba(243,128,32,0.12)',
    softHover: 'rgba(243,128,32,0.18)',
    hover: '#ff8f33',
    pressed: '#d96e15',
    ink: '#ffffff',
  },
  '#5B73FF': {
    name: 'Electric indigo',
    soft: 'rgba(91,115,255,0.12)',
    softHover: 'rgba(91,115,255,0.18)',
    hover: '#7088ff',
    pressed: '#4458d6',
    ink: '#ffffff',
  },
  '#10B981': {
    name: 'Emerald',
    soft: 'rgba(16,185,129,0.12)',
    softHover: 'rgba(16,185,129,0.18)',
    hover: '#22c898',
    pressed: '#0c9670',
    ink: '#ffffff',
  },
  '#E94584': {
    name: 'Magenta',
    soft: 'rgba(233,69,132,0.12)',
    softHover: 'rgba(233,69,132,0.18)',
    hover: '#f55896',
    pressed: '#c93669',
    ink: '#ffffff',
  },
  '#262626': {
    name: 'Monochrome',
    soft: 'rgba(20,18,16,0.08)',
    softHover: 'rgba(20,18,16,0.14)',
    hover: '#3a3a3a',
    pressed: '#0a0a0a',
    ink: '#ffffff',
  },
};

export const DEFAULT_ACCENT: AccentHex = '#F38020';

export function resolveAccent(hex: AccentHex | undefined | null): {
  hex: AccentHex;
  meta: AccentMeta;
} {
  const normalized = (hex ?? DEFAULT_ACCENT).toUpperCase();
  const meta = ACCENTS[normalized] ?? ACCENTS[DEFAULT_ACCENT];
  return { hex: normalized, meta };
}

/**
 * Write the accent CSS variables onto :root so every component
 * styled against `--accent`, `--accent-hover`, etc. reflects the change.
 *
 * Also mirrors the value into the legacy `--color-accent` so the
 * pre-redesign chrome (which still reads `--color-accent`) stays in sync.
 */
export function applyAccent(hex: AccentHex | undefined | null): void {
  if (typeof document === 'undefined') return;
  const { hex: resolvedHex, meta } = resolveAccent(hex);
  const root = document.documentElement;
  root.style.setProperty('--accent', resolvedHex);
  root.style.setProperty('--accent-hover', meta.hover);
  root.style.setProperty('--accent-pressed', meta.pressed);
  root.style.setProperty('--accent-soft', meta.soft);
  root.style.setProperty('--accent-soft-hover', meta.softHover);
  root.style.setProperty('--accent-ink', meta.ink);
  // Mirror to legacy chrome so existing components stay coherent.
  root.style.setProperty('--color-accent', resolvedHex);
}

/** Convenience list of accents in display order (used by Settings UI). */
export const ACCENT_LIST: Array<{ hex: AccentHex; meta: AccentMeta }> = Object.entries(ACCENTS).map(
  ([hex, meta]) => ({ hex, meta })
);
