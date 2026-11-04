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
		'/formatter': '/tutorial/check#format',
		'/linter': '/tutorial/check#lints',
		'/diagnostics': '/reference/diagnostic-codes',
		'/configuration': '/reference/configuration',
		'/language-server': '/installation#editors',
		'/features': '/tutorial/types',
		'/limitations': '/reference/type-system#limitations',
		'/stdlib-stubs': '/development/authoring-stubs',
		'/typing': '/tutorial/types',
		'/typing/guide': '/tutorial/types',
		'/typing/reference': '/reference/type-system',
		'/typing-reference': '/reference/type-system',
		'/architecture': '/development/architecture',
		'/structure': '/development/structure',
		'/testing': '/development/testing',
		'/tour': '/tutorial/types',
		'/why-ry': '/getting-started',
		'/guides/adopting': '/tutorial/check',
		'/guides/checking': '/tutorial/check',
		'/guides/continuous-integration': '/tutorial/check#in-ci',
		'/guides/formatting': '/tutorial/check#format',
		'/guides/editors': '/installation#editors',
		'/guides/running': '/tutorial/runtime',
		'/guides/r-console': '/tutorial/runtime',
		'/guides/console': '/tutorial/runtime',
		'/guides/loading-packages': '/reference/package-loading',
		'/guides/dataframes': '/reference/dataframes',
		'/guides/from-data-table': '/reference/from-data-table',
		'/type-checking/overview': '/tutorial/types',
		'/type-checking/concepts': '/tutorial/types',
		'/type-checking/tutorial': '/tutorial/types',
		'/type-checking/tour': '/tutorial/types',
		'/type-checking/annotations': '/tutorial/types#annotate',
		'/type-checking/functions': '/tutorial/types#annotate',
		'/type-checking/vectors-and-lists': '/tutorial/types#shapes',
		'/type-checking/null': '/tutorial/types#null',
		'/type-checking/unknown': '/tutorial/types#unknown',
		'/type-checking/stubs': '/development/authoring-stubs',
		'/type-checking/limitations': '/reference/type-system#limitations',
		'/type-checking/inline-annotations': '/tutorial/runtime#inline-annotations',
		'/type-checking/records-and-tags': '/tutorial/runtime#records-tags-and-match',
		'/type-checking/arrays': '/reference/type-system#lists',
		'/type-checking/lists': '/reference/type-system#lists',
		'/type-checking/custom-types': '/tutorial/fully-typed#nominal-types',
		'/type-checking/domain-modeling': '/tutorial/fully-typed#nominal-types',
		'/type-checking/strict-mode': '/tutorial/fully-typed#strict-mode',
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
		'/contributing/design/base-image': '/development/design/base-image',
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
						{ slug: 'ry' },
						{ slug: 'benchmarks' },
					],
				},
				// One page per step of moving R code onto ry. Each page builds on the one before, and
				// detail lives in Reference.
				{
					label: 'Tutorial',
					items: [
						{ slug: 'tutorial/check' },
						{ slug: 'tutorial/types' },
						{ slug: 'tutorial/runtime' },
						{ slug: 'tutorial/fully-typed' },
					],
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
		// The benchmark pages read their programs and results from tools/bench/why/.
		server: { fs: { allow: ['..'] } },
	},
});
