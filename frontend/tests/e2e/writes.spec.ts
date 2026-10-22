import path from 'node:path';
import { expect, noisePng, signUpAndIn, test } from './fixtures';

const fixture = (name: string) => path.resolve(import.meta.dirname, '../../../fixtures/media', name);

test('a guest uploads, keeps the delete link, and uses it', async ({ page }) => {
	await page.goto('/');
	// Bytes nobody has sent before: a duplicate of another guest's upload
	// rightly comes back without a delete link.
	await page
		.getByTestId('file-input')
		.setInputFiles({ name: 'mine.png', mimeType: 'image/png', buffer: noisePng(8, 8) });
	const item = page.getByTestId('upload-item');
	await expect(item).toHaveAttribute('data-status', 'done');
	const viewer = await item.getByRole('link', { name: 'Open' }).getAttribute('href');

	await item.getByTestId('delete-link').click();
	await page.waitForURL(/\/delete\/[0-9A-Za-z]{10}\/.+/);
	await page.getByRole('button', { name: 'Delete it' }).click();
	await expect(page.getByRole('status')).toContainText('Deleted');

	await page.goto(viewer!);
	await expect(page.getByRole('heading', { name: 'Not found' })).toBeVisible();
});

test('several files at once, with one embed code per file', async ({ page }) => {
	await page.goto('/');
	await page
		.getByTestId('file-input')
		.setInputFiles([fixture('still.png'), fixture('still.jpg'), fixture('anim.gif')]);
	const items = page.getByTestId('upload-item');
	await expect(items).toHaveCount(3);
	for (const item of await items.all()) {
		await expect(item).toHaveAttribute('data-status', 'done');
	}
	const code = page.getByTestId('embed-code');
	await expect(code).toHaveValue(/^(https?:\/\/\S+\n){2}https?:\/\/\S+$/);
	await page.getByLabel('Format').selectOption('bbcode-linked');
	await expect(code).toHaveValue(/^\[url=.+\]\[img\].+\[\/img\]\[\/url\]/);

	await page.getByRole('button', { name: 'Clear finished' }).click();
	await expect(items).toHaveCount(0);
});


test('a member files uploads into a new album and edits both', async ({ page }) => {
	const username = await signUpAndIn(page);
	await page.goto('/');
	await page.getByTestId('file-input').setInputFiles([fixture('still.png'), fixture('still.jpg')]);
	for (const item of await page.getByTestId('upload-item').all()) {
		await expect(item).toHaveAttribute('data-status', 'done');
	}

	await page.goto(`/u/${username}`);
	await page.getByRole('button', { name: 'New album' }).click();
	await page.getByLabel('Name').fill('Holiday');
	await page.getByRole('button', { name: 'Create album' }).click();
	await expect(page.getByTestId('album-list')).toContainText('Holiday');

	const boxes = page.getByTestId('select-file');
	await expect(boxes).toHaveCount(2);
	for (const box of await boxes.all()) await box.check();
	const bar = page.getByTestId('bulk-bar');
	await bar.getByLabel('Move to').selectOption({ label: 'Holiday' });
	await bar.getByRole('button', { name: 'Move' }).click();
	await expect(page.getByRole('status')).toContainText('Moved 2 files');

	await page.getByTestId('album-list').getByRole('link', { name: /Holiday/ }).click();
	await expect(page.getByTestId('album-name')).toHaveText('Holiday');
	await expect(page.getByTestId('file-grid').getByRole('listitem')).toHaveCount(2);

	await page.getByRole('button', { name: 'Edit album' }).click();
	await page.getByLabel('Who can see it').selectOption('private');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect(page.getByText('2 files · private')).toBeVisible();

	// Edit one file's title from its page.
	await page.getByTestId('gallery-item').first().click();
	await page.getByTestId('lightbox').getByRole('link', { name: 'Open page' }).click();
	await page.getByRole('button', { name: 'Edit' }).click();
	await page.getByLabel('Title').fill('By the sea');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect(page.getByTestId('viewer-title')).toHaveText('By the sea');

	// Make it the album's cover: the card on the profile shows it.
	await page.getByRole('button', { name: 'Make album cover' }).click();
	await expect(page.getByRole('status')).toContainText('cover');
	await page.goto(`/u/${username}`);
	await expect(page.getByTestId('album-list').locator('img')).toHaveCount(1);
});
