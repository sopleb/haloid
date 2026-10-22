import { expect, test as base, type Page } from '@playwright/test';
import { randomBytes } from 'node:crypto';
import { crc32, deflateSync } from 'node:zlib';

export { expect };

/** Every spec's `test`: our own frontend must never touch the legacy endpoint. */
export const test = base.extend<{ legacyGuard: void }>({
	legacyGuard: [
		async ({ page }, use) => {
			page.on('request', (request) => {
				// This guard has to name the endpoint it forbids.
				// eslint-disable-next-line no-restricted-syntax
				if (new URL(request.url()).pathname.startsWith('/api/1/')) {
					throw new Error(`the frontend called the legacy API: ${request.url()}`);
				}
			});
			await use();
		},
		{ auto: true }
	]
});

/** Register a fresh account through the UI and sign in with it. */
export async function signUpAndIn(page: Page): Promise<string> {
	const username = `u${Date.now().toString(36)}${Math.floor(Math.random() * 1000)}`;
	const password = 'correct horse battery';
	await page.goto('/register');
	await page.getByLabel('Username', { exact: true }).fill(username);
	await page.getByLabel('Password', { exact: true }).fill(password);
	await expect(page.getByRole('button', { name: 'Create account' })).toBeEnabled({ timeout: 30_000 });
	await page.getByRole('button', { name: 'Create account' }).click();
	await page.waitForURL(/\/login$/);
	await page.getByLabel('Username or email').fill(username);
	await page.getByLabel('Password', { exact: true }).fill(password);
	await expect(page.getByRole('button', { name: 'Sign in' })).toBeEnabled({ timeout: 30_000 });
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByTestId('nav-user')).toHaveText(username);
	return username;
}

/** A valid PNG of random pixels: incompressible, so it is as big as asked. */
export function noisePng(width: number, height: number): Buffer {
	const chunk = (type: string, data: Buffer) => {
		const length = Buffer.alloc(4);
		length.writeUInt32BE(data.length);
		const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
		const crc = Buffer.alloc(4);
		crc.writeUInt32BE(crc32(body));
		return Buffer.concat([length, body, crc]);
	};
	const header = Buffer.alloc(13);
	header.writeUInt32BE(width, 0);
	header.writeUInt32BE(height, 4);
	header[8] = 8; // bit depth
	header[9] = 2; // RGB
	const rows: Buffer[] = [];
	for (let y = 0; y < height; y++) rows.push(Buffer.concat([Buffer.from([0]), randomBytes(width * 3)]));
	return Buffer.concat([
		Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
		chunk('IHDR', header),
		chunk('IDAT', deflateSync(Buffer.concat(rows), { level: 1 })),
		chunk('IEND', Buffer.alloc(0))
	]);
}

