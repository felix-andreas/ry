// Plain-Markdown views of the docs pages, for agents and other tools that read text rather than HTML.
// Every page is served at its URL plus `.md`, `/llms.txt` lists the pages in sidebar order, and
// `/llms-full.txt` concatenates them.
import { getCollection, type CollectionEntry } from 'astro:content';
import starlightConfig from 'virtual:starlight/user-config';
import vennSource from '../components/Venn.astro?raw';

type DocsPage = CollectionEntry<'docs'>;
type SidebarItem = { label?: string; slug?: string; items?: SidebarItem[] };

const SUMMARY_FALLBACK = 'ry is a toolchain for R: a type checker, formatter, linter and runtime.';
const OPTIONAL_PAGES_LABEL = 'Optional';

export function markdownPath(page: DocsPage): string {
  return `/${page.id}.md`;
}

export async function loadPages(): Promise<DocsPage[]> {
  return getCollection('docs');
}

export function pageMarkdown(page: DocsPage): string {
  return `# ${page.data.title}\n\n${bodyToMarkdown(page.body ?? '')}\n`;
}

// Pages in the sidebar's groups, in sidebar order, followed by the pages the sidebar does not list
// (drafts and news), which go under llms.txt's "Optional" heading.
export function sections(pages: DocsPage[]): { label: string; pages: DocsPage[] }[] {
  const byId = new Map(pages.map((page) => [page.id, page]));
  const listed = new Set<string>();
  const result: { label: string; pages: DocsPage[] }[] = [];

  for (const group of (starlightConfig.sidebar ?? []) as SidebarItem[]) {
    const groupPages = (group.items ?? [group]).flatMap(function collect(item: SidebarItem): DocsPage[] {
      if (item.items) return item.items.flatMap(collect);
      const page = item.slug === undefined ? undefined : byId.get(item.slug);
      if (page === undefined || listed.has(page.id)) return [];
      listed.add(page.id);
      return [page];
    });
    if (groupPages.length > 0) result.push({ label: group.label ?? '', pages: groupPages });
  }

  const unlisted = pages.filter((page) => !listed.has(page.id)).sort((a, b) => a.id.localeCompare(b.id));
  if (unlisted.length > 0) result.push({ label: OPTIONAL_PAGES_LABEL, pages: unlisted });
  return result;
}

export function siteSummary(pages: DocsPage[]): string {
  return pages.find((page) => page.id === 'why-ry')?.data.description ?? SUMMARY_FALLBACK;
}

// Turns an MDX page body into Markdown that needs no components: the import lines and layout tags
// go, a tab group becomes consecutive subheadings (one level below the heading it sits under), and
// absolute links to other pages point at their `.md` version.
function bodyToMarkdown(body: string): string {
  const output: string[] = [];
  const push = (line: string, inFence: boolean) => {
    if (!inFence && line.trim() === '' && output.at(-1)?.trim() === '') return;
    output.push(line);
  };
  let fence: { marker: string; length: number } | undefined;
  let headingLevel = 1;
  let inComment = false;

  for (const line of body.split('\n')) {
    const fenceMatch = /^\s*(`{3,}|~{3,})/.exec(line);
    if (fence) {
      if (fenceMatch && fenceMatch[1][0] === fence.marker && fenceMatch[1].length >= fence.length) fence = undefined;
      push(line, true);
      continue;
    }
    if (fenceMatch) {
      fence = { marker: fenceMatch[1][0], length: fenceMatch[1].length };
      push(line, true);
      continue;
    }

    if (inComment) {
      inComment = !line.includes('-->');
      continue;
    }
    if (line.startsWith('<!--')) {
      inComment = !line.includes('-->');
      continue;
    }

    const heading = /^(#{1,6})\s/.exec(line);
    if (heading) headingLevel = heading[1].length;

    const tab = /^<TabItem\s+label="([^"]*)"/.exec(line);
    if (tab) {
      push(`${'#'.repeat(Math.min(headingLevel + 1, 6))} ${tab[1]}`, false);
      continue;
    }
    if (/^<\/?(Tabs|TabItem|figure)\b/.test(line) || line.startsWith('import ')) continue;
    if (/^<Venn\b/.test(line.trim())) {
      push(`Diagram: ${vennDescription()}`, false);
      continue;
    }

    push(rewriteLinks(line), false);
  }

  return output.join('\n').trim();
}

function vennDescription(): string {
  return /<desc[^>]*>([^<]*)<\/desc>/.exec(vennSource)?.[1].trim() ?? 'a Venn diagram of R and ry.';
}

function rewriteLinks(line: string): string {
  return line.replace(/\]\((\/[^)#\s]*?)\/?(#[^)\s]*)?\)/g, (whole, path: string, fragment = '') =>
    path === '/' || path.split('/').pop()?.includes('.') ? whole : `](${path}.md${fragment})`,
  );
}
