import type { TokenUsage } from '../../protocol';

/** Compact token estimates from `token_usage` events, e.g. `ctx 12.3k · turn 1.2k`. */
export function StatusBar({ usage }: { usage: TokenUsage | null }) {
  if (!usage) return null;

  const parts: string[] = [];
  if (usage.context !== undefined) parts.push(`ctx ${format(usage.context)}`);
  if (usage.turn !== undefined) parts.push(`turn ${format(usage.turn)}`);
  if (parts.length === 0) return null;

  return <span className="token-usage">{parts.join(' · ')}</span>;
}

function format(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return String(tokens);
}
