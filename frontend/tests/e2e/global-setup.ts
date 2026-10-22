import { chromium, type FullConfig } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

/** Where the administrator's signed-in browser state is kept for specs. */
export const ADMIN_STATE = path.resolve(import.meta.dirname, '.auth/admin.json');

/**
 * The first account registered on an instance becomes its administrator.
 * The e2e server starts on an empty database, so registering here, before
 * any spec runs, makes this account the admin, deterministically.
 */
export default async function globalSetup(config: FullConfig) {
	const baseURL = config.projects[0].use.baseURL!;
	const browser = await chromium.launch();
	const page = await browser.newPage({ baseURL });
	const password = 'correct horse battery';
	await page.goto('/register');
	await page.getByLabel('Username', { exact: true }).fill('founder');
	await page.getByLabel('Password', { exact: true }).fill(password);
	const create = page.getByRole('button', { name: 'Create account' });
	await create.waitFor();
	await page.waitForFunction(() => !document.querySelector<HTMLButtonElement>('button[type=submit]')?.disabled, null, {
		timeout: 30_000
	});
	await create.click();
	await page.waitForURL(/\/login$/);
	await page.getByLabel('Username or email').fill('founder');
	await page.getByLabel('Password', { exact: true }).fill(password);
	await page.waitForFunction(() => !document.querySelector<HTMLButtonElement>('button[type=submit]')?.disabled, null, {
		timeout: 30_000
	});
	await page.getByRole('button', { name: 'Sign in' }).click();
	await page.getByTestId('nav-user').waitFor();
	fs.mkdirSync(path.dirname(ADMIN_STATE), { recursive: true });
	await page.context().storageState({ path: ADMIN_STATE });
	await browser.close();
}
