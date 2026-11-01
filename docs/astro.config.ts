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
		'/formatter': '/guides/formatting',
		'/linter': '/guides/checking',
		'/diagnostics': '/reference/diagnostic-codes',
		'/configuration': '/reference/configuration',
		'/language-server': '/guides/editors',
		'/features': '/type-checking/overview',
		'/type-checking/concepts': '/type-checking/overview',
		'/limitations': '/type-checking/limitations',
		'/stdlib-stubs': '/type-checking/stubs',
		'/typing': '/type-checking/overview',
		'/typing/guide': '/type-checking/overview',
		'/type-checking/tutorial': '/type-checking/overview',
		'/typing/reference': '/reference/type-system',
		'/typing-reference': '/reference/type-system',
		'/development': '/contributing/development',
		'/architecture': '/contributing/architecture',
		'/structure': '/contributing/structure',
		'/testing': '/contributing/testing',
		'/guides/adopting': '/getting-started',
		'/tour': '/type-checking/overview',
		'/type-checking/tour': '/type-checking/overview',
		'/type-checking/domain-modeling': '/type-checking/custom-types',
		'/guides/continuous-integration': '/guides/checking#in-ci',
		'/guides/r-console': '/guides/console',
	},
	integrations: [
		starlight({
			title: 'ry',
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
						{ slug: 'why-ry' },
						{ slug: 'installation' },
					],
				},
				// The groups follow the migration story on the landing page and in why-ry.md: classic R
				// first (steps 1 and 2), then what ry adds on top of R (steps 3 and 4).
				{
					label: '1. Check the code you have',
					items: [
						{ slug: 'guides/checking' },
						{ slug: 'guides/formatting' },
						{ slug: 'guides/editors' },
						{ slug: 'guides/console' },
						{ slug: 'guides/loading-packages' },
					],
				},
				{
					label: '2. Type your code',
					items: [
						{ slug: 'type-checking/overview' },
						{ slug: 'type-checking/annotations' },
						{ slug: 'type-checking/vectors-and-lists' },
						{ slug: 'type-checking/null' },
						{ slug: 'type-checking/functions' },
						{ slug: 'type-checking/custom-types' },
						{ slug: 'type-checking/unknown' },
						{ slug: 'type-checking/stubs' },
						{ slug: 'type-checking/limitations' },
					],
				},
				{
					label: '3. Switch to inline types',
					items: [
						{ slug: 'type-checking/inline-annotations' },
					],
				},
				{
					label: "4. Go fully typed",
					items: [
						{ slug: 'type-checking/records-and-tags' },
						{ slug: 'type-checking/dataframes' },
						{ slug: 'guides/dataframes' },
						{ slug: 'reference/standard-library' },
						{ slug: 'type-checking/strict-mode' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ slug: 'reference/configuration' },
						{ slug: 'reference/cli' },
						{ slug: 'reference/package-loading' },
						{ slug: 'reference/diagnostic-codes' },
						{ slug: 'reference/formatting-rules' },
						{ slug: 'reference/type-system' },
					],
				},
				{
					label: 'Contributing',
					items: [
						{ slug: 'contributing/development' },
						{ slug: 'contributing/architecture' },
						{ slug: 'contributing/structure' },
						{ slug: 'contributing/testing' },
						{ slug: 'contributing/corpus-and-oracle' },
						{ slug: 'contributing/authoring-stubs' },
						{ slug: 'contributing/releasing' },
						// Drafts under contributing/design/ are proposals, not contracts; their index is the way in.
						{ slug: 'contributing/design' },
					],
				},
			],
			// Replaces the theme's hardcoded "Docs > title" breadcrumb.
			components: {
				PageTitle: './src/components/PageTitle.astro',
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
	},
});
