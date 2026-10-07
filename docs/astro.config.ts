// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import tailwind from '@tailwindcss/vite';
import starlightThemeBlack from 'starlight-theme-black'

// https://astro.build/config
export default defineConfig({
	// Every page published before the docs were reorganised keeps working.
	redirects: {
		'/formatter': '/reference/formatting-rules',
		'/linter': '/reference/diagnostic-codes',
		'/diagnostics': '/reference/diagnostic-codes',
		'/configuration': '/reference/configuration',
		'/language-server': '/tour#editors',
		'/features': '/tour',
		'/type-checking/concepts': '/tour',
		'/limitations': '/type-checking/limitations',
		'/stdlib-stubs': '/type-checking/stubs',
		'/typing': '/tour',
		'/typing/guide': '/tour',
		'/type-checking/tutorial': '/tour',
		'/typing/reference': '/reference/type-system',
		'/typing-reference': '/reference/type-system',
		'/development': '/contributing/development',
		'/architecture': '/contributing/architecture',
		'/structure': '/contributing/structure',
		'/testing': '/contributing/testing',
		'/guides/adopting': '/getting-started',
		'/guides/continuous-integration': '/reference/cli#exit-codes',
		'/guides/r-console': '/reference/cli#server-repl-and-run',
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
			// The installation page is deliberately absent: getting started closes with
			// the two extension links and the one-line install, and the page itself is
			// only for the awkward cases.
			sidebar: [
				{
					label: 'Introduction',
					items: [
						{ slug: 'getting-started' },
						{ slug: 'tour' },
						{ slug: 'why-ry' },
					],
				},
				{
					label: 'Type checking',
					items: [
						{ slug: 'type-checking/domain-modeling' },
						{ slug: 'type-checking/stubs' },
						{ slug: 'type-checking/limitations' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ slug: 'reference/configuration' },
						{ slug: 'reference/cli' },
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
						{ slug: 'contributing/authoring-stubs' },
						// The drafts under contributing/design/ are deliberately not
						// listed: they are proposals and open questions, not contracts,
						// and this index is the single way in.
						{ slug: 'contributing/design' },
					],
				},
			],
			// Replaces the theme's own PageTitle, whose breadcrumb is a hardcoded
			// "Docs > title". starlight-theme-black skips its override when one is
			// declared here, and warns that it did.
			components: {
				PageTitle: './src/components/PageTitle.astro',
			},
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
