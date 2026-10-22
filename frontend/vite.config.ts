import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vitest/config';

// In development the SPA runs on Vite and the API on haloid-server; the
// proxy makes them one origin, as they are in production.
const backend = process.env.HALOID_BACKEND ?? 'http://127.0.0.1:8080';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	server: {
		proxy: {
			'/api': backend,
			'/files': backend
		}
	},
	test: {
		include: ['src/**/*.test.ts']
	}
});
