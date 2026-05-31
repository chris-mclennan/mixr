// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// https://astro.build/config
export default defineConfig({
  site: 'https://mixr.sh',
  integrations: [
    starlight({
      title: 'mixr',
      description:
        'Lean terminal DJ app for electronic music — Beatport streaming, AI-assisted mixing, hardware controllers.',
      // Hidden-during-dev: remove this `head` block before public launch.
      head: [
        {
          tag: 'meta',
          attrs: { name: 'robots', content: 'noindex, nofollow' },
        },
      ],
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/chris-mclennan/mixr',
        },
      ],
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Overview', slug: 'index' },
            { label: 'Install', slug: 'install' },
            { label: 'First run', slug: 'getting-started' },
          ],
        },
        {
          // Manual pages added by the `manual-writer` agent over time.
          // Order here reflects intended reading sequence.
          label: 'Manual',
          items: [
            { label: 'Getting Started', slug: 'manual/getting-started' },
          ],
        },
        {
          label: 'Family',
          items: [
            { label: 'tmnl — GPU terminal', link: 'https://tmnl.sh' },
            { label: 'mnml — terminal IDE', link: 'https://mnml.sh' },
          ],
        },
      ],
    }),
  ],
});
