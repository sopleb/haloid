import { describe, expect, it } from 'vitest';
import { parseChoice, resolveTheme } from './themes';

describe('resolveTheme', () => {
	it('uses the site theme until the visitor picks one', () => {
		expect(resolveTheme('site', 'day', false)).toBe('day');
		expect(resolveTheme('night', 'day', true)).toBe('night');
	});

	it('falls back to Night for a site theme this build does not have', () => {
		expect(resolveTheme('site', 'neon', false)).toBe('night');
		expect(resolveTheme('site', null, true)).toBe('night');
	});

	it('follows the device when asked to', () => {
		expect(resolveTheme('device', 'night', true)).toBe('day');
		expect(resolveTheme('device', 'day', false)).toBe('night');
	});
});

describe('parseChoice', () => {
	it('reads a stored choice, and anything unknown as the site default', () => {
		expect(parseChoice('day')).toBe('day');
		expect(parseChoice('device')).toBe('device');
		expect(parseChoice('neon')).toBe('site');
		expect(parseChoice(null)).toBe('site');
	});
});
