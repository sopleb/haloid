import fs from 'node:fs';
import path from 'node:path';
import { ADMIN_STATE } from './global-setup';
import { expect, test } from './fixtures';

const fixture = (name: string) => path.resolve(import.meta.dirname, '../../../fixtures/media', name);

test.use({ storageState: ADMIN_STATE });

test('the administrator holds an upload for review and approves it', async ({ page, browser }) => {
	await page.goto('/admin/settings');
	await page.getByTestId('setting-moderation.mode').fill('guests');
	await page.getByRole('button', { name: /^Save/ }).click();
	await expect(page.getByRole('status')).toContainText('live now');

	// A guest, in a browser of their own, uploads.
	const guest = await browser.newContext({ storageState: { cookies: [], origins: [] } });
	const guestPage = await guest.newPage();
	await guestPage.goto('/');
	await guestPage.getByTestId('file-input').setInputFiles(fixture('still.png'));
	await expect(guestPage.getByTestId('upload-item')).toHaveAttribute('data-status', 'done');
	await guest.close();

	await page.getByRole('link', { name: 'Moderation' }).click();
	const queued = page.getByTestId('queued-file');
	await expect(queued).toHaveCount(1);
	await queued.getByRole('button', { name: 'Approve' }).click();
	await expect(queued).toHaveCount(0);

	await page.getByRole('link', { name: 'Audit log' }).click();
	await expect(page.getByTestId('audit-log')).toContainText('file.approve');
	await expect(page.getByTestId('audit-log')).toContainText('settings.update');

	// Put the instance back for the other specs.
	await page.goto('/admin/settings');
	await page.getByTestId('setting-moderation.mode').fill('off');
	await page.getByRole('button', { name: /^Save/ }).click();
	await expect(page.getByRole('status')).toContainText('live now');
});

test('the administrator restores a deleted file, and tests the storage', async ({ page }) => {
	const ours = {
		'x-requested-with': 'XMLHttpRequest',
		origin: new URL(test.info().project.use.baseURL!).origin
	};
	const created = await page.request.post('/api/v2/uploads', {
		headers: ours,
		multipart: {
			file: { name: 'still.gif', mimeType: 'image/gif', buffer: fs.readFileSync(fixture('anim.gif')) }
		}
	});
	expect(created.status()).toBe(201);
	const { file } = await created.json();
	const gone = await page.request.delete(`/api/v2/files/${file.id}`, {
		headers: ours
	});
	expect(gone.status()).toBe(204);

	await page.goto('/admin/deleted');
	const row = page.getByTestId('deleted-file').filter({ hasText: file.id });
	await row.getByRole('button', { name: 'Restore' }).click();
	await expect(page.getByRole('status')).toContainText('Restored');
	const back = await page.request.get(`/api/v2/files/${file.id}`);
	expect(back.ok()).toBeTruthy();

	await page.goto('/admin/storages');
	const storage = page.getByTestId('storage').first();
	await storage.getByRole('button', { name: 'Test' }).click();
	await expect(storage.getByTestId('storage-checks')).toContainText('write');
	await expect(storage.getByTestId('storage-checks')).toContainText('quarantine_private');
});

test('a Community instance shows its id, its branding, and offers no external storage', async ({ page }) => {
	await page.goto('/admin/licence');
	await expect(page.getByTestId('licence-state')).toContainText('Community');
	await expect(page.getByTestId('instance-id')).toHaveText(/^inst_[0-9A-Za-z]{20}$/);
	await expect(page.getByTestId('branding')).toContainText('Powered by Haloid');

	// Not a key at all: refused with the reason, and nothing changes.
	await page.getByTestId('licence-key').fill('not a key');
	await page.getByRole('button', { name: 'Install key' }).click();
	await expect(page.getByRole('alert')).toContainText('not a Haloid licence key');

	await page.getByRole('link', { name: 'Storages' }).click();
	await expect(page.getByTestId('storage-pro-note')).toBeVisible();
	await expect(page.locator('option[value="s3"]')).toHaveCount(0);
});

test('the updates page shows this build and lets an administrator choose when to update', async ({ page }) => {
	await page.goto('/admin/updates');
	await expect(page.getByTestId('current-version')).toHaveText(/^\d+\.\d+\.\d+/);
	// The test server's release host is unreachable on purpose.
	await page.getByRole('button', { name: 'Look now' }).click();
	await expect(page.getByRole('alert').first()).toContainText('Looking for a release failed');

	const mode = page.getByRole('combobox', { name: 'When a release comes out' });
	await expect(mode).toHaveValue('notify');
	await mode.selectOption('auto');
	await expect(page.getByRole('combobox', { name: 'At (UTC)' })).toBeVisible();
	await mode.selectOption('notify');
	await expect(page.getByRole('combobox', { name: 'At (UTC)' })).toHaveCount(0);
});
