import { expect, noisePng, signUpAndIn, test } from './fixtures';



test('a big upload survives going offline and resumes without resending', async ({ page, context }) => {
	test.setTimeout(120_000);
	await signUpAndIn(page);
	const png = noisePng(2600, 2600); // about 20 MB: three 8 MiB chunks
	expect(png.length).toBeGreaterThan(16 * 1024 * 1024);

	const sent: number[] = [];
	page.on('requestfinished', (request) => {
		const match = /\/chunks\/(\d+)$/.exec(new URL(request.url()).pathname);
		if (match && request.method() === 'PUT') sent.push(Number(match[1]));
	});

	// The connection drops as chunk 1 goes out, the first time only.
	let dropped = false;
	await page.route('**/api/v2/uploads/sessions/*/chunks/1', async (route) => {
		if (dropped) return route.continue();
		dropped = true;
		await context.setOffline(true);
		await route.abort('internetdisconnected');
	});

	await page.goto('/');
	await page.getByTestId('file-input').setInputFiles({ name: 'noise.png', mimeType: 'image/png', buffer: png });

	const item = page.getByTestId('upload-item');
	await expect(item).toHaveAttribute('data-status', 'failed', { timeout: 30_000 });
	await context.setOffline(false);
	await item.getByRole('button', { name: 'Retry' }).click();
	await expect(item).toHaveAttribute('data-status', 'done', { timeout: 60_000 });

	// Every chunk arrived exactly once: the retry asked what the server had.
	expect([...sent].sort()).toEqual([0, 1, 2]);
	await item.getByRole('link', { name: 'Open' }).click();
	await expect(page.getByTestId('viewer-image')).toHaveAttribute('width', '2600');
});
