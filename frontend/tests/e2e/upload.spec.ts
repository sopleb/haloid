import { expect, test } from './fixtures';
import fs from 'node:fs';
import path from 'node:path';

const fixture = (name: string) => path.resolve(import.meta.dirname, '../../../fixtures/media', name);

test('upload an image on / and open it at /f/{id}', async ({ page }) => {
	await page.goto('/');
	await page.getByTestId('file-input').setInputFiles(fixture('still.png'));
	const item = page.getByTestId('upload-item');
	await expect(item).toHaveAttribute('data-status', 'done');
	await item.getByRole('link', { name: 'Open' }).click();

	await page.waitForURL(/\/f\/[0-9A-Za-z]{10}$/);
	const image = page.getByTestId('viewer-image');
	await expect(image).toBeVisible();
	// Loaded, not just present: the bytes really came back from storage.
	await expect
		.poll(() => image.evaluate((img: HTMLImageElement) => img.naturalWidth))
		.toBe(32);
	expect(await image.getAttribute('width')).toBe('32');
	expect(await image.getAttribute('alt')).toBe('');
});

test('a deep link to a viewer page loads directly', async ({ page, request }) => {
	const response = await request.post('/api/v2/uploads', {
		multipart: {
			file: { name: 'deep.gif', mimeType: 'image/gif', buffer: fs.readFileSync(fixture('anim.gif')) }
		}
	});
	expect(response.status()).toBe(201);
	const { file } = await response.json();

	await page.goto(`/f/${file.id}`);
	await expect(page.getByTestId('viewer-image')).toBeVisible();

	// That visit counted; a reload within the half hour does not.
	await expect.poll(async () => (await (await request.get(`/api/v2/files/${file.id}`)).json()).views).toBe(1);
	await page.reload();
	await expect(page.getByTestId('views')).toHaveText('1 view');
});

test('an unknown viewer id says not found', async ({ page }) => {
	await page.goto('/f/0000000000');
	await expect(page.getByRole('heading', { name: 'Not found' })).toBeVisible();
});
