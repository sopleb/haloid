import { expect, test } from './fixtures';

// The server runs with HALOID_ALTCHA_COST=1, so the widget's proof-of-work
// finishes in moments; the protocol is otherwise the production one.

test('register, sign in, see yourself in the nav, sign out', async ({ page }) => {
	const username = `e2e${Date.now().toString(36)}`;
	const password = 'correct horse battery';

	await page.goto('/register');
	await page.getByLabel('Username', { exact: true }).fill(username);
	await page.getByLabel('Password', { exact: true }).fill(password);
	const create = page.getByRole('button', { name: 'Create account' });
	await expect(create).toBeEnabled({ timeout: 30_000 });
	await create.click();

	await page.waitForURL(/\/login$/);
	await expect(page.getByRole('status')).toContainText('account is ready');
	await page.getByLabel('Username or email').fill(username);
	await page.getByLabel('Password', { exact: true }).fill(password);
	const signIn = page.getByRole('button', { name: 'Sign in' });
	await expect(signIn).toBeEnabled({ timeout: 30_000 });
	await signIn.click();

	await page.waitForURL(/\/$/);
	await expect(page.getByTestId('nav-user')).toHaveText(username);

	await page.getByTestId('nav-user').click();
	await expect(page.getByRole('heading', { name: username })).toBeVisible();

	await page.getByRole('button', { name: 'Sign out' }).click();
	await expect(page.getByTestId('nav-user')).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Sign in' })).toBeVisible();
});

test('a wrong password says so, and the widget solves a fresh challenge', async ({ page }) => {
	await page.goto('/login');
	await page.getByLabel('Username or email').fill('nobody-here');
	await page.getByLabel('Password', { exact: true }).fill('not the password');
	const signIn = page.getByRole('button', { name: 'Sign in' });
	await expect(signIn).toBeEnabled({ timeout: 30_000 });
	await signIn.click();
	await expect(page.getByRole('alert')).toBeVisible();
	// A payload is single-use; the form must not offer to resend the spent one.
	await expect(signIn).toBeEnabled({ timeout: 30_000 });
});

test('explore lists what guests may see', async ({ page, request }) => {
	await page.goto('/explore');
	await expect(page.getByRole('heading', { name: 'Explore' })).toBeVisible();
	const response = await request.get('/api/v2/files');
	expect(response.ok()).toBeTruthy();
});
