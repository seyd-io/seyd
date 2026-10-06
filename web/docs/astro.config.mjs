// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Seyd developer documentation (PLAN.md §2.8).
//
// Served from the seyd-signal image at /docs/ (cloud/api/deploy.sh copies
// dist/ there), so the site is built with that base. The API references are
// generated from the code by scripts/generate.mjs before every build; the
// JavaScript reference is generated here, by starlight-typedoc, from the
// exports of @seyd/core and @seyd/web.
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightTypeDoc, { typeDocSidebarGroup } from 'starlight-typedoc';
import { fileURLToPath } from 'node:url';

// `@repo/…` reaches any file in the monorepo, so a page imports an example as
// `import src from '@repo/sdks/c/examples/sensor-robot.c?raw'` and renders it:
// the example is the file, and the check that compiles it checks the page.
const repo = fileURLToPath(new URL('../..', import.meta.url));

export default defineConfig({
  vite: { resolve: { alias: { '@repo': repo, '@components': fileURLToPath(new URL('./src/components', import.meta.url)) } } },
  site: 'https://docs.seyd.io',
  base: '/docs',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'Seyd Docs',
      description: 'Developer documentation for Seyd: the daemon, the robot SDKs, the web pilot SDK, the cloud and the protocol.',
      favicon: '/favicon.svg',
      customCss: ['./src/styles/seyd.css'],
      // No CDN, no analytics: the docs must work self-hosted and offline like every other Seyd surface.
      pagefind: true,
      lastUpdated: false,
      credits: false,
      components: {
        SiteTitle: './src/components/SiteTitle.astro',
      },
      expressiveCode: {
        themes: ['github-light', 'github-dark'],
        useStarlightUiThemeColors: false,
        styleOverrides: {
          borderRadius: '6px',
          codeFontFamily: 'var(--seyd-font-mono)',
          codeFontSize: '12.5px',
          uiFontFamily: 'var(--seyd-font-mono)',
          frames: { shadowColor: 'transparent' },
        },
      },
      plugins: [
        starlightTypeDoc({
          entryPoints: ['../../sdks/js/core/src/index.ts', '../../sdks/js/web/src/index.ts'],
          tsconfig: './typedoc.tsconfig.json',
          output: 'reference/js',
          sidebar: { label: 'JavaScript (@seyd/core, @seyd/web)', collapsed: true },
          typeDoc: {
            name: 'JavaScript SDK',
            readme: 'none',
            excludeInternal: true,
            excludePrivate: true,
            excludeProtected: true,
            skipErrorChecking: true,
            entryPointStrategy: 'resolve',
            plugin: ['typedoc-plugin-markdown'],
            entryFileName: 'index',
          },
        }),
      ],
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'What Seyd is', slug: 'start/overview' },
            { label: 'Concepts', slug: 'start/concepts' },
            { label: 'Choose a form factor', slug: 'start/form-factor' },
            { label: 'Quickstart: drive the demo', slug: 'start/quickstart' },
            { label: 'Integrate with a coding agent', slug: 'start/agent-skill' },
          ],
        },
        {
          label: 'Robot side',
          items: [
            { label: 'Integrate the daemon (seydd)', slug: 'robot/daemon' },
            { label: 'A robot in Python', slug: 'robot/python' },
            { label: 'A robot in C', slug: 'robot/c' },
            { label: 'A robot in Rust', slug: 'robot/rust' },
            { label: 'The publisher contract', slug: 'robot/publisher-contract' },
            { label: 'Simulcast', slug: 'robot/simulcast' },
            { label: 'Encoder setup', slug: 'robot/encoder-setup' },
          ],
        },
        {
          label: 'Pilot side',
          items: [
            { label: 'The web components', slug: 'pilot/web-components' },
            { label: 'Your own UI on SeydSession', slug: 'pilot/session-api' },
            { label: 'Latency and the HUD', slug: 'pilot/latency' },
          ],
        },
        {
          label: 'Networking',
          items: [
            { label: 'Reachability', slug: 'networking/reachability' },
            { label: 'Starlink', slug: 'networking/starlink' },
            {
              label: 'When the pilot cannot connect',
              collapsed: true,
              items: [{ autogenerate: { directory: 'networking/classes' } }],
            },
          ],
        },
        {
          label: 'Cloud',
          items: [
            { label: 'Enrolment and access', slug: 'cloud/access' },
            { label: 'Run the cloud yourself', slug: 'cloud/self-hosting' },
            { label: 'Identity providers', slug: 'cloud/identity-providers' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'C ABI (seyd.h)', slug: 'reference/c' },
            { label: 'Python (seyd)', slug: 'reference/python' },
            typeDocSidebarGroup,
            { label: 'Rust crates (rustdoc)', slug: 'reference/rust' },
            { label: 'seydd.toml', slug: 'reference/seydd-config' },
            { label: 'QoS profiles', slug: 'reference/qos-profiles' },
            { label: 'Protocol contracts', collapsed: true, items: [{ autogenerate: { directory: 'reference/protocol' } }] },
            { label: 'Architecture decisions', collapsed: true, items: [{ autogenerate: { directory: 'reference/adr' } }] },
          ],
        },
      ],
    }),
  ],
});
