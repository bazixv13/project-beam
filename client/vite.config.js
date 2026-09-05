import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

function inlineCssPlugin() {
  return {
    name: 'inline-css-plugin',
    enforce: 'post',
    transformIndexHtml(html, { bundle }) {
      if (!bundle) return html;
      let newHtml = html;
      for (const [fileName, file] of Object.entries(bundle)) {
        if (fileName.endsWith('.css') && file.type === 'asset' && typeof file.source === 'string') {
          const cssContent = file.source;
          const linkRegex = new RegExp(`<link[^>]*href="[^"]*${fileName}"[^>]*>`, 'i');
          newHtml = newHtml.replace(linkRegex, `<style>${cssContent}</style>`);
        }
      }
      return newHtml;
    }
  };
}

export default defineConfig({
  plugins: [react(), inlineCssPlugin()],
})
