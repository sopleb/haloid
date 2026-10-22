// Windowing for the uniform grid: which rows are near enough the viewport
// to be worth rendering. Pure, so it is tested without a browser; the
// gallery feeds it the scroll position and its own measurements.

export interface WindowInput {
	/** How far the grid's top is above the viewport's top, in pixels. */
	scrolledPast: number;
	viewport: number;
	rowHeight: number;
	columns: number;
	count: number;
	/** Rows rendered beyond each edge, so fast scrolling shows no gaps. */
	overscan?: number;
}

export interface WindowRange {
	/** Item indices, `start` inclusive, `end` exclusive. */
	start: number;
	end: number;
	/** Space to leave above and below the rendered rows. */
	before: number;
	after: number;
}

export function visibleRange(input: WindowInput): WindowRange {
	const { rowHeight, count } = input;
	const columns = Math.max(1, Math.floor(input.columns));
	const overscan = input.overscan ?? 3;
	const rows = Math.ceil(count / columns);
	if (rows === 0 || rowHeight <= 0) return { start: 0, end: count, before: 0, after: 0 };
	const firstRow = Math.max(0, Math.floor(input.scrolledPast / rowHeight) - overscan);
	const lastRow = Math.min(
		rows,
		Math.ceil((input.scrolledPast + input.viewport) / rowHeight) + overscan
	);
	const start = Math.min(count, firstRow * columns);
	const end = Math.min(count, Math.max(start, lastRow * columns));
	return {
		start,
		end,
		before: firstRow * rowHeight,
		after: Math.max(0, rows - Math.max(firstRow, lastRow)) * rowHeight
	};
}

/** Below this many items, windowing costs more than it saves. */
export const WINDOW_FROM = 120;
