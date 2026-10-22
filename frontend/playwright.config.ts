import { defineConfig, devices } from '@playwright/test';

// Against the real Rust backend, never mocks: mocked uploads hide every bug
// that matters. `pnpm build` first; the server serves frontend/build.
const port = process.env.HALOID_E2E_PORT ?? '18081';
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
	testDir: 'tests/e2e',
	// Registers the instance's first account, which administers it, before
	// any spec can register one of its own.
	globalSetup: './tests/e2e/global-setup.ts',
	fullyParallel: false,
	// One instance, shared: a spec that changes a setting must not race another.
	workers: 1,
	use: { baseURL, trace: 'retain-on-failure' },
	projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
	webServer: {
		command: '../scripts/e2e-server.sh',
		url: `${baseURL}/api/v2/healthz`,
		timeout: 180_000,
		reuseExistingServer: false,
		stdout: 'pipe',
		stderr: 'pipe'
	}
});
