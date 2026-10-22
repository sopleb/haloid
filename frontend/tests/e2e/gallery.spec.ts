import type { Page } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { expect, signUpAndIn, test } from './fixtures';

const fixture = (name: string) => path.resolve(import.meta.dirname, '../../../fixtures/media', name);

/** Headers our own page sends, which the API's CSRF rule asks for. */
const ours = (page: Page) => ({
	'x-requested-with': 'XMLHttpRequest',
	origin: new URL(page.url()).origin
});

async function uploadAll(page: Page, names: string[]) {
	await page.goto('/');
	await page.getByTestId('file-input').setInputFiles(names.map(fixture));
	for (const item of await page.getByTestId('upload-item').all()) {
		await expect(item).toHaveAttribute('data-status', 'done');
	}
}

test('the lightbox opens on the file URL, steps with the keys, and Back closes it', async ({ page }) => {
	const username = await signUpAndIn(page);
	await uploadAll(page, ['still.png', 'still.jpg', 'still.gif']);
	await page.goto(`/u/${username}`);

	const first = page.getByTestId('gallery-item').first();
	await first.click();
	const lightbox = page.getByTestId('lightbox');
	await expect(lightbox).toBeVisible();
	await expect(page).toHaveURL(/\/f\/[0-9A-Za-z]{10}$/);
	await expect(lightbox).toContainText('1 of 3');
	const firstUrl = page.url();

	await page.keyboard.press('ArrowRight');
	await expect(lightbox).toContainText('2 of 3');
	expect(page.url()).not.toBe(firstUrl);
	await page.keyboard.press('End');
	await expect(lightbox).toContainText('3 of 3');

	await page.keyboard.press('Escape');
	await expect(lightbox).toHaveCount(0);
	await expect(page).toHaveURL(new RegExp(`/u/${username}$`));
	// Focus goes back to the card the lightbox was opened from.
	await expect(first).toBeFocused();

	// Masonry is a preference that sticks.
	await page.getByRole('button', { name: 'Masonry' }).click();
	await page.reload();
	await expect(page.getByRole('button', { name: 'Masonry' })).toHaveAttribute('aria-pressed', 'true');
});

test('an API key is shown once, then listed by its prefix, and can be revoked', async ({ page }) => {
	await signUpAndIn(page);
	await page.getByRole('link', { name: 'Account', exact: true }).click();
	await page.getByLabel('Label').fill('ShareX');
	await page.getByRole('button', { name: 'Create key' }).click();
	const secret = await page.getByTestId('new-key-secret').textContent();
	expect(secret).toMatch(/^hld_/);

	const list = page.getByTestId('key-list');
	await expect(list).toContainText('ShareX');
	await expect(list).not.toContainText(secret!);

	// The key really works, as a bearer.
	const me = await page.request.get('/api/v2/auth/me', {
		headers: { authorization: `Bearer ${secret}` }
	});
	expect(me.ok()).toBeTruthy();

	page.once('dialog', (dialog) => dialog.accept());
	await list.getByRole('button', { name: 'Revoke' }).click();
	await expect(list).toContainText('revoked');
	const after = await page.request.get('/api/v2/auth/me', {
		headers: { authorization: `Bearer ${secret}` }
	});
	expect(after.status()).toBe(401);
});

test('a password album asks for its password, and opens with it', async ({ page }) => {
	await signUpAndIn(page);
	const created = await page.request.post('/api/v2/albums', {
		headers: ours(page),
		data: { name: 'Private trip', privacy: 'password', password: 'open sesame' }
	});
	expect(created.status()).toBe(201);
	const album = await created.json();
	const upload = await page.request.post('/api/v2/uploads', {
		headers: ours(page),
		multipart: {
			album: album.id,
			file: { name: 'still.png', mimeType: 'image/png', buffer: fs.readFileSync(fixture('still.png')) }
		}
	});
	expect(upload.status()).toBe(201);

	await page.getByRole('button', { name: 'Sign out' }).click();
	await expect(page.getByTestId('nav-user')).toHaveCount(0);
	await page.goto(`/a/${album.id}`);
	const form = page.getByTestId('unlock-form');
	await expect(form).toBeVisible();
	await form.getByLabel('Password').fill('nope');
	await form.getByRole('button', { name: 'Open album' }).click();
	await expect(page.getByRole('alert')).toContainText('not the password');

	await form.getByLabel('Password').fill('open sesame');
	await form.getByRole('button', { name: 'Open album' }).click();
	await expect(page.getByTestId('album-name')).toHaveText('Private trip');
	await expect(page.getByTestId('gallery-item')).toHaveCount(1);
});
