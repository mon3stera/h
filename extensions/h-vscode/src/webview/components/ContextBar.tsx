import type { ReactNode } from 'react';
import type { TokenUsage } from '../../protocol';

/**
 * The remaining-context indicator, matching the TUI's bottom status line:
 * `context 12.3k/200k ([██████░░░░] 55.0% left)`.
 *
 * The bar shows the *remaining* share (ten cells, filled to light shade). The
 * bar, the space and the percentage form one rainbow run with the closing
 * bracket inserted after the bar — exactly how the TUI's `context_spans`
 * builds it. `turn` stays alongside as a muted trailing estimate.
 */
export function ContextBar({ usage, window: limit }: { usage: TokenUsage | null; window: number | null }) {
  const context = usage?.context;
  const turn = usage?.turn;
  if (context === undefined && turn === undefined) return null;

  const current = context === undefined ? '?' : formatTokens(context);
  const limitText = limit === null ? '?' : formatTokens(limit);

  let percent = '?% left';
  let bar: string | null = null;
  if (context !== undefined && limit !== null && limit > 0) {
    const left = Math.max(0, limit - context);
    percent = `${((left / limit) * 100).toFixed(1)}% left`;
    bar = contextBar(left / limit);
  }

  const rainbow = bar === null ? rainbowText(percent) : rainbowText(`${bar} ${percent}`);

  return (
    <span className="context-bar">
      <span className="cb-muted">
        context {current}/{limitText} (
      </span>
      {bar !== null && <span className="cb-muted">[</span>}
      {bar !== null ? rainbow.slice(0, bar.length) : rainbow}
      {bar !== null && <span className="cb-muted">]</span>}
      {bar !== null ? rainbow.slice(bar.length) : null}
      <span className="cb-muted">)</span>
      {turn !== undefined && <span className="cb-turn"> · turn {formatTokens(turn)}</span>}
    </span>
  );
}

/** The remaining-context bar: ten cells, the filled share as solid blocks. */
function contextBar(fraction: number): string {
  const filled = Math.round(fraction * 10);
  return '█'.repeat(filled) + '░'.repeat(10 - filled);
}

/** Same K/M suffixes as the TUI's `format_tokens`, e.g. `2.4K`, `45K`, `1M`. */
function formatTokens(count: number): string {
  const KILO = 1_000;
  const MEGA = 1_000_000;

  const divisor = count >= MEGA - KILO / 20 ? MEGA : count >= KILO ? KILO : null;
  if (divisor === null) return String(count);

  let value = (count / divisor).toFixed(1);
  if (value.endsWith('.0')) value = value.slice(0, -2);
  return `${value}${divisor === MEGA ? 'M' : 'K'}`;
}

/**
 * One character at a time along a hue ramp, like the TUI's `rainbow_spans`
 * (6° per character, starting hue hashed from the text). The hash differs
 * from Rust's `DefaultHasher`; the stepped rainbow is what carries over.
 */
function rainbowText(text: string): ReactNode[] {
  const startHue = fnv1a(text) % 360;
  return [...text].map((character, index) => (
    <span key={index} style={{ color: `hsl(${(startHue + index * 6) % 360} 69% 67.5%)` }}>
      {character}
    </span>
  ));
}

/** FNV-1a 32-bit; small, deterministic, and stable across reloads. */
function fnv1a(text: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}
