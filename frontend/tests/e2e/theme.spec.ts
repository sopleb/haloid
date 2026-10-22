import { expect, test } from './fixtures';

test('Night by default; a chosen theme applies at once and on the next visit', async ({ page }) => {
	await page.goto('/');
	const html = page.locator('html');
	await expect(html).toHaveAttribute('data-theme', 'night');

	await page.getByLabel('Theme').selectOption('day');
	await expect(html).toHaveAttribute('data-theme', 'day');
	const ground = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
	expect(ground).toBe('rgb(244, 241, 234)');

	// static/theme.js sets it before the app loads, so there is no flash of Night.
	await page.reload();
	await expect(html).toHaveAttribute('data-theme', 'day');

	await page.getByLabel('Theme').selectOption('site');
	await expect(html).toHaveAttribute('data-theme', 'night');
});

test('"Match my device" follows the system setting', async ({ page }) => {
	await page.emulateMedia({ colorScheme: 'light' });
	await page.goto('/');
	await page.getByLabel('Theme').selectOption('device');
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'day');
	await page.emulateMedia({ colorScheme: 'dark' });
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
});
