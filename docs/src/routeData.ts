import { defineRouteMiddleware } from '@astrojs/starlight/route-data';

// Points tools at the Markdown version of each page, which is served at the page's URL plus `.md`.
export const onRequest = defineRouteMiddleware((context) => {
  const { id, head } = context.locals.starlightRoute;
  head.push({ tag: 'link', attrs: { rel: 'alternate', type: 'text/markdown', href: `/${id}.md` } });
});
