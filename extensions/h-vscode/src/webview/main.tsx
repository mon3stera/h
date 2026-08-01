import { createRoot } from 'react-dom/client';
import App from './App';
import './style.css';

// Live `h.fontSize` updates from the extension host. The initial value is
// already baked into the document as `--h-font-size` by webviewHtml; this
// keeps an open panel in sync when the setting changes afterwards.
window.addEventListener('message', (event: MessageEvent<{ type?: string; fontSize?: number | null }>) => {
  if (event.data.type !== 'font-size') return;
  const { fontSize } = event.data;
  if (typeof fontSize === 'number' && fontSize > 0) {
    document.documentElement.style.setProperty('--h-font-size', `${fontSize}px`);
  } else {
    document.documentElement.style.removeProperty('--h-font-size');
  }
});

createRoot(document.getElementById('root')!).render(<App />);
