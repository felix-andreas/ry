import type { APIRoute } from 'astro';
import { loadPages, markdownUrl, sections, siteSummary } from '../lib/markdown';

export const GET: APIRoute = async ({ site }) => {
  const pages = await loadPages();
  const lines = ['# ry', '', `> ${siteSummary(pages)}`, ''];
  for (const section of sections(pages)) {
    lines.push(`## ${section.label}`, '');
    for (const page of section.pages) {
      const description = page.data.description ? `: ${page.data.description}` : '';
      lines.push(`- [${page.data.title}](${markdownUrl(page, site!)})${description}`);
    }
    lines.push('');
  }
  return new Response(lines.join('\n'), { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
};
