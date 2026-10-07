// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import tailwindcss from '@tailwindcss/vite';

// https://astro.build/config
export default defineConfig({
  site: 'https://mcp-tool-shop-org.github.io',
  base: '/offrig',
  integrations: [
    starlight({
      title: 'offrig',
      logo: {
        src: './src/assets/logo.png',
        alt: 'offrig',
        href: '/offrig/',
        replacesTitle: false,
      },
      description: 'Run big models on rented RunPod GPUs, never your own: app, CLI and MCP side-car.',
      disable404Route: true,
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/mcp-tool-shop-org/offrig' },
      ],
      sidebar: [
        {
          label: 'Handbook',
          items: [{ autogenerate: { directory: 'handbook' } }],
        },
      ],
      customCss: ['./src/styles/starlight-custom.css'],
    }),
  ],
  vite: {
    plugins: [tailwindcss()],
  },
});
