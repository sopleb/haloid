import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { ADMIN_STATE } from './global-setup';
import { themes } from '../../src/lib/theme/themes';
import { expect, test } from './fixtures';

const fixture = (name: string) => path.resolve(import.meta.dirname, '../../../fixtures/media', name);

/** Accessibility as an acceptance test: no serious or critical axe finding. */
async function audit(page: Page, url: string) {
	await page.goto(url);
	await page.waitForLoadState('networkidle');
	const results = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
	const blocking = results.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
	expect(
		blocking.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(' ')).join(', ')})`),
		url
	).toEqual([]);
}

// Every theme is held to the same contrast. Night is the default; the others
// are chosen the way a visitor chooses one, through the stored preference.
for (const theme of themes.map((t) => t.id)) {
	test.describe(`in ${theme}`, () => {
		test.beforeEach(async ({ page }) => {
			await page.addInitScript((id) => localStorage.setItem('haloid.theme', id), theme);
		});

		test.describe('as a guest', () => {
			for (const url of ['/', '/explore', '/login', '/register', '/u/founder']) {
				test(`${url} has no serious accessibility problems`, async ({ page }) => {
					await audit(page, url);
					await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
				});
			}
		});

		test.describe('as the administrator', () => {
			test.use({ storageState: ADMIN_STATE });
			test('a viewer page, the account area and the admin area', async ({ page }) => adminPages(page));
		});
	});
}

async function adminPages(page: Page) {
	const ours = { 'x-requested-with': 'XMLHttpRequest', origin: new URL(test.info().project.use.baseURL!).origin };
	const created = await page.request.post('/api/v2/uploads', {
		headers: ours,
		multipart: {
			title: 'A still',
			file: { name: 'still.jpg', mimeType: 'image/jpeg', buffer: fs.readFileSync(fixture('still.jpg')) }
		}
	});
	expect(created.ok()).toBeTruthy();
	const { file } = await created.json();
	for (const url of [`/f/${file.id}`, '/account', '/admin/settings', '/admin/licence', '/admin/updates']) await audit(page, url);
}
