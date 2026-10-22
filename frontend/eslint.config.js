import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';

/**
 * Our own frontend must never call the legacy API. It is a compatibility
 * surface for other people's clients, bug-for-bug, and nothing we build
 * should depend on its quirks — this rule, and a network assertion in the
 * end-to-end tests, keep it that way.
 */
const noLegacyApi = {
	'no-restricted-syntax': [
		'error',
		{
			selector: 'Literal[value=/\\/api\\/1\\//]',
			message: 'The frontend must not use the legacy /api/1/ endpoint; use /api/v2.'
		},
		{
			selector: 'TemplateElement[value.raw=/\\/api\\/1\\//]',
			message: 'The frontend must not use the legacy /api/1/ endpoint; use /api/v2.'
		}
	]
};

export default ts.config(
	js.configs.recommended,
	...ts.configs.recommended,
	...svelte.configs.recommended,
	{
		languageOptions: { globals: { ...globals.browser, ...globals.node } }
	},
	{
		files: ['**/*.svelte', '**/*.svelte.ts'],
		languageOptions: { parserOptions: { parser: ts.parser } }
	},
	// The app's own code; this file has to name the endpoint to forbid it.
	{ files: ['src/**', 'tests/**'], rules: noLegacyApi },
	{ ignores: ['build/', '.svelte-kit/', 'node_modules/', 'test-results/', 'playwright-report/'] }
);
