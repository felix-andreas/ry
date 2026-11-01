import type { APIRoute } from 'astro';
import { loadPages, markdownUrl, pageMarkdown, sections, siteSummary } from '../lib/markdown';

export const GET: APIRoute = async ({ site }) => {
  const pages = await loadPages();
  const parts = [`# ry\n\n> ${siteSummary(pages)}\n`];
  for (const section of sections(pages)) {
    for (const page of section.pages) {
      parts.push(`---\n\nSource: ${markdownUrl(page, site!)}\n\n${pageMarkdown(page)}`);
    }
  }
  return new Response(parts.join('\n'), { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
};
