// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import tailwind from '@tailwindcss/vite';
import starlightThemeBlack from 'starlight-theme-black'

// https://astro.build/config
export default defineConfig({
	site: 'https://ry-lang.org',
	// Every page published before the docs were reorganised keeps working.
	redirects: {
		// Every URL the site ever published keeps working. Targets name the page that holds the
		// material now, with an anchor where the old page became one section.
		'/formatter': '/guide#format',
		'/linter': '/guide#lints',
		'/diagnostics': '/reference/diagnostic-codes',
		'/configuration': '/reference/configuration',
		'/language-server': '/installation#editors',
		'/features': '/guide#step-2-type-your-code',
		'/limitations': '/reference/type-system#limitations',
		'/stdlib-stubs': '/development/authoring-stubs',
		'/typing': '/guide#step-2-type-your-code',
		'/typing/guide': '/guide#step-2-type-your-code',
		'/typing/reference': '/reference/type-system',
		'/typing-reference': '/reference/type-system',
		'/architecture': '/development/architecture',
		'/structure': '/development/structure',
		'/testing': '/development/testing',
		'/tour': '/ry',
		'/why-ry': '/getting-started',
		'/tutorial/check': '/guide#step-1-check-your-code',
		'/tutorial/types': '/guide#step-2-type-your-code',
		'/tutorial/runtime': '/guide#step-3-run-on-rys-runtime',
		'/tutorial/fully-typed': '/guide#step-4-go-fully-typed',
		'/guides/adopting': '/guide#step-1-check-your-code',
		'/guides/checking': '/guide#step-1-check-your-code',
		'/guides/continuous-integration': '/guide#in-ci',
		'/guides/formatting': '/guide#format',
		'/guides/editors': '/installation#editors',
		'/guides/running': '/guide#step-3-run-on-rys-runtime',
		'/guides/r-console': '/guide#step-3-run-on-rys-runtime',
		'/guides/console': '/guide#step-3-run-on-rys-runtime',
		'/guides/loading-packages': '/reference/package-loading',
		'/guides/dataframes': '/reference/dataframes',
		'/guides/from-data-table': '/reference/from-data-table',
		'/type-checking/overview': '/guide#step-2-type-your-code',
		'/type-checking/concepts': '/guide#step-2-type-your-code',
		'/type-checking/tutorial': '/guide#step-2-type-your-code',
		'/type-checking/tour': '/guide#step-2-type-your-code',
		'/type-checking/annotations': '/guide#annotate',
		'/type-checking/functions': '/guide#annotate',
		'/type-checking/vectors-and-lists': '/guide#shapes',
		'/type-checking/null': '/guide#null',
		'/type-checking/unknown': '/guide#unknown',
		'/type-checking/stubs': '/development/authoring-stubs',
		'/type-checking/limitations': '/reference/type-system#limitations',
		'/type-checking/inline-annotations': '/guide#inline-annotations',
		'/type-checking/records-and-tags': '/guide#records-tags-and-match',
		'/type-checking/arrays': '/reference/type-system#lists',
		'/type-checking/lists': '/reference/type-system#lists',
		'/type-checking/custom-types': '/guide#nominal-types',
		'/type-checking/domain-modeling': '/guide#nominal-types',
		'/type-checking/strict-mode': '/guide#strict-mode',
		'/type-checking/dataframes': '/reference/dataframes#types',
		'/contributing/development': '/development',
		'/contributing/architecture': '/development/architecture',
		'/contributing/structure': '/development/structure',
		'/contributing/testing': '/development/testing',
		'/contributing/robustness': '/development/robustness',
		'/contributing/corpus-and-oracle': '/development/corpus-and-oracle',
		'/contributing/authoring-stubs': '/development/authoring-stubs',
		'/contributing/releasing': '/development/releasing',
		'/contributing/design': '/development/design',
		'/contributing/design/base-image': '/development/architecture/#the-start-up-image',
		'/contributing/design/data-masking': '/development/design/data-masking',
		'/contributing/design/inline-type-syntax': '/development/design/inline-type-syntax',
		'/contributing/design/open-questions': '/development/design/open-questions',
		'/contributing/design/packaging': '/development/design/packaging',
		'/contributing/design/prior-art': '/development/design/prior-art',
		'/contributing/design/r-runtime': '/development/design/r-runtime',
		'/contributing/design/records-and-tags': '/development/design/records-and-tags',
		'/contributing/design/repl': '/development/design/repl',
		'/contributing/design/standard-library': '/development/design/standard-library',
		'/contributing/design/typed-runtime': '/development/design/typed-runtime',
	},
	integrations: [
		starlight({
			title: 'ry',
			description: 'A type checker and a fast runtime for R, written in Rust.',
			logo: {
				src: './public/logo.svg',
				replacesTitle: true,
			},
			favicon: '/favicon.svg',
			// Diagnostic output is shown in `text` blocks, and a cut-off message is
			// unreadable, so those wrap; code keeps its lines.
			expressiveCode: {
				defaultProps: {
					overridesByLang: { text: { wrap: true } },
				},
			},
			social: [
				{ label: "Visual Studio Marketplace", icon: "vscode", href: 'https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry' },
				{ label: "GitHub", icon: "github", href: 'https://github.com/felix-andreas/ry' },
			],
			sidebar: [
				{
					label: 'Introduction',
					items: [
						{ slug: 'getting-started' },
						{ slug: 'installation' },
						{ slug: 'benchmarks' },
					],
				},
				{
					label: 'Learn',
					items: [{ slug: 'guide' }, { slug: 'ry' }],
				},
				{
					label: 'Reference',
					items: [
						{ slug: 'reference/cli' },
						{ slug: 'reference/configuration' },
						{ slug: 'reference/diagnostic-codes' },
						{ slug: 'reference/language' },
						{ slug: 'reference/type-system' },
						{ slug: 'reference/standard-library' },
						{ slug: 'reference/std' },
						{ slug: 'reference/dataframes' },
						{ slug: 'reference/from-data-table' },
						{ slug: 'reference/package-loading' },
						{ slug: 'reference/formatting-rules' },
						{ slug: 'reference/licensing' },
					],
				},
				{
					label: 'Development',
					items: [
						{ slug: 'development', label: 'Build and test' },
						{ slug: 'development/architecture' },
						{ slug: 'development/structure' },
						{ slug: 'development/testing' },
						{ slug: 'development/robustness' },
						{ slug: 'development/corpus-and-oracle' },
						{ slug: 'development/authoring-stubs' },
						{ slug: 'development/releasing' },
						// Drafts under development/design/ are proposals, not contracts; their index is the way in.
						{ slug: 'development/design' },
					],
				},
			],
			// Replaces the theme's hardcoded "Docs > title" breadcrumb.
			components: {
				PageTitle: './src/components/PageTitle.astro',
				Sidebar: './src/components/Sidebar.astro',
			},
			// Adds the `<link rel="alternate" type="text/markdown">` to every page.
			routeMiddleware: './src/routeData.ts',
			customCss: ['./src/tailwind.css'],
			plugins: [
				starlightThemeBlack({
					navLinks: [
						{
							label: 'Docs',
							link: '/getting-started',
						},
						{
							label: 'News',
							link: '/news',
						},
					],
					footerText: `<div class="py-8 flex items-center justify-between"><div class="flex items-center gap-2"><img src="/logo.svg" width="12" /> ry © ${new Date().getFullYear()}</div><a href="https://felixandreas.me/legal-notice/" target="_blank" rel="noopener" class="no-underline text-gray-500">Legal Notice</a></div>`
				})
			],
		}),
	],
	vite: {
		plugins: [tailwind()],
		// The benchmark pages read their programs and results from tools/bench/.
		server: { fs: { allow: ['..'] } },
	},
});
