import ReactMarkdown, { type Components } from 'react-markdown';
import { openExternal } from '../rpc';

/** Assistant replies render as markdown; links open in the system browser. */
export function Markdown({ text }: { text: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown components={components}>{text}</ReactMarkdown>
    </div>
  );
}

const components: Components = {
  a({ href, children }) {
    return (
      <a
        href={href}
        onClick={(event) => {
          if (!href) return;
          event.preventDefault();
          openExternal(href);
        }}
      >
        {children}
      </a>
    );
  },
  code({ className, children }) {
    const text = String(children).replace(/\n$/, '');
    const isBlock = className?.startsWith('language-') || text.includes('\n');
    if (isBlock) {
      return (
        <pre className="md-code-block">
          <code className={className}>{children}</code>
        </pre>
      );
    }
    return <code className="md-code-inline">{children}</code>;
  },
};
