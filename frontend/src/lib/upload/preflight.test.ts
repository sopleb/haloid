import { describe, expect, it } from 'vitest';
import type { ConfigView } from '$lib/api/types';
import { preflight, size } from './preflight';

const config = (overrides: Partial<ConfigView['uploads']> = {}): ConfigView => ({
	site_name: 'Haloid',
	base_url: 'http://localhost',
	registration_open: true,
	signed_in: false,
	theme: 'night',
	branding: true,
	uploads: {
		allowed: true,
		guests_allowed: true,
		max_size: 1024,
		max_pixels: 1_000_000,
		extensions: ['jpg', 'png'],
		max_expiration_secs: 60,
		url_sources: false,
		...overrides
	}
});

describe('preflight', () => {
	it('lets an acceptable file through', () => {
		expect(preflight({ name: 'cat.JPG', size: 10 }, config())).toBeNull();
	});
	it('refuses what the server would refuse, before uploading', () => {
		expect(preflight({ name: 'cat.jpg', size: 2048 }, config())).toMatch(/limit is 1 KB/);
		expect(preflight({ name: 'doc.pdf', size: 10 }, config())).toMatch(/\.pdf/);
		expect(preflight({ name: 'cat.jpg', size: 0 }, config())).toMatch(/empty/);
		expect(preflight({ name: 'cat.jpg', size: 1 }, config({ allowed: false }))).toMatch(/not open/);
	});
	it('leaves an extensionless name to the server, which sniffs the bytes', () => {
		expect(preflight({ name: 'clipboard', size: 10 }, config())).toBeNull();
	});
});

describe('size', () => {
	it('reads like a person would say it', () => {
		expect(size(512)).toBe('512 B');
		expect(size(1536)).toBe('1.5 KB');
		expect(size(64 * 1024 * 1024)).toBe('64 MB');
	});
});
