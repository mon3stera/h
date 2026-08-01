import { useState } from 'react';
import type {
  DiffLine,
  DisplayBlock,
  KeyValueEntry,
  ToolPresentation,
} from '../../protocol';

const STATUS_ICON = {
  running: '●',
  succeeded: '✓',
  failed: '✗',
} as const;

/** One tool execution: header (status/label/name) + collapsible blocks. */
export function ToolCard({ tool }: { tool: ToolPresentation }) {
  const [collapsed, setCollapsed] = useState(false);
  const status = tool.status.type;
  const failed = status === 'failed' && tool.status.data ? tool.status.data.message : null;

  return (
    <div className={`tool-card status-${status}`}>
      <button className="tool-card-header" onClick={() => setCollapsed((value) => !value)}>
        <span className="tool-status">{STATUS_ICON[status]}</span>
        <span className="tool-label">{tool.label}</span>
        <span className="tool-name">
          {tool.name}
          {tool.target ? ` ${tool.target}` : ''}
        </span>
        <span className="tool-chevron">{collapsed ? '▸' : '▾'}</span>
      </button>
      {failed && <div className="tool-error">{failed}</div>}
      {!collapsed && (
        <div className="tool-blocks">
          {tool.blocks.map((block, index) => (
            <BlockView key={index} block={block} />
          ))}
        </div>
      )}
    </div>
  );
}

function BlockView({ block }: { block: DisplayBlock }) {
  switch (block.type) {
    case 'summary':
      return <pre className="block-summary">{block.data}</pre>;
    case 'code_block':
      return <CodeBlockView data={block.data} />;
    case 'diff':
      return <DiffView lines={block.data.lines} />;
    case 'table':
      return <TableView headers={block.data.headers} rows={block.data.rows} />;
    case 'key_value':
      return <KeyValueView entries={block.data.entries} />;
    case 'text_output':
      return (
        <pre className="block-text-output">
          {block.data.content}
          {block.data.truncated_lines > 0 && (
            <span className="truncation-note">… {block.data.truncated_lines} lines truncated</span>
          )}
        </pre>
      );
  }
}

function CodeBlockView({ data }: { data: Extract<DisplayBlock, { type: 'code_block' }>['data'] }) {
  const lines = data.content.split('\n');
  return (
    <div className="code-block">
      <div className="code-block-header">
        {data.language && <span className="code-language">{data.language}</span>}
        {data.truncated_lines > 0 && (
          <span className="truncation-note">… {data.truncated_lines} lines truncated</span>
        )}
      </div>
      <pre className="code-content">
        {lines.map((line, index) => (
          <div key={index} className="code-line">
            {data.show_line_numbers && (
              <span className="code-line-number">{data.start_line_number + index}</span>
            )}
            <span className="code-line-text">{line || ' '}</span>
          </div>
        ))}
      </pre>
    </div>
  );
}

function DiffView({ lines }: { lines: DiffLine[] }) {
  return (
    <pre className="diff-block">
      {lines.map((line, index) => (
        <div key={index} className={`diff-line kind-${line.kind}`}>
          <span className="diff-number">{line.number}</span>
          <span className="diff-marker">{line.kind === 'removed' ? '-' : line.kind === 'added' ? '+' : ' '}</span>
          <span className="diff-text">{line.text}</span>
        </div>
      ))}
    </pre>
  );
}

function TableView({ headers, rows }: { headers: string[]; rows: string[][] }) {
  return (
    <table className="tool-table">
      <thead>
        <tr>
          {headers.map((header, index) => (
            <th key={index}>{header}</th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.map((row, rowIndex) => (
          <tr key={rowIndex}>
            {row.map((cell, cellIndex) => (
              <td key={cellIndex}>{cell}</td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function KeyValueView({ entries }: { entries: KeyValueEntry[] }) {
  return (
    <div className="key-value-block">
      {entries.map((entry, index) => (
        <div key={index} className="key-value-row">
          <span className="key-value-key">{entry.key}</span>
          <span className="key-value-value">{entry.value}</span>
        </div>
      ))}
    </div>
  );
}
