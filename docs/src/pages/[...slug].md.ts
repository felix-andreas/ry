import type { APIRoute, GetStaticPaths } from 'astro';
import { loadPages, pageMarkdown } from '../lib/markdown';

export const getStaticPaths: GetStaticPaths = async () =>
  (await loadPages()).map((page) => ({ params: { slug: page.id }, props: { page } }));

export const GET: APIRoute = ({ props }) =>
  new Response(pageMarkdown(props.page), { headers: { 'Content-Type': 'text/markdown; charset=utf-8' } });
