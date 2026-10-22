import { describe, expect, it } from 'vitest';
import { visibleRange } from './window';

const base = { viewport: 600, rowHeight: 200, columns: 4, overscan: 1 };

describe('visibleRange', () => {
	it('renders the rows in view plus the overscan, and spaces the rest', () => {
		const r = visibleRange({ ...base, scrolledPast: 1000, count: 400 });
		// Rows 5..8 are in view; one more each side: rows 4..9.
		expect([r.start, r.end]).toEqual([16, 36]);
		expect(r.before).toBe(800);
		expect(r.after).toBe((100 - 9) * 200);
		expect(r.before + (r.end - r.start) / 4 * 200 + r.after).toBe(100 * 200);
	});

	it('starts at the top, and never runs past the end', () => {
		expect(visibleRange({ ...base, scrolledPast: -500, count: 400 })).toMatchObject({ start: 0, before: 0 });
		const end = visibleRange({ ...base, scrolledPast: 1e7, count: 10 });
		expect(end.end).toBe(10);
		expect(end.after).toBe(0);
	});

	it('copes with a partial last row and odd inputs', () => {
		const r = visibleRange({ ...base, scrolledPast: 0, count: 6 });
		expect([r.start, r.end, r.after]).toEqual([0, 6, 0]);
		expect(visibleRange({ ...base, scrolledPast: 0, count: 0 })).toEqual({ start: 0, end: 0, before: 0, after: 0 });
		expect(visibleRange({ ...base, columns: 0, scrolledPast: 0, count: 3 }).end).toBe(3);
	});
});
