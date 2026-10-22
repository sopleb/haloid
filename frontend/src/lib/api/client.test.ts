import { describe, expect, it } from 'vitest';
import { ApiError, getFile, toError } from './client';

describe('getFile', () => {
	it('asks the v2 API and returns the file', async () => {
		const seen: string[] = [];
		const fetcher = (async (url: string) => {
			seen.push(url);
			return new Response(JSON.stringify({ id: 'abc' }), { status: 200 });
		}) as unknown as typeof fetch;

		const file = await getFile('abc', fetcher);
		expect(file.id).toBe('abc');
		expect(seen).toEqual(['/api/v2/files/abc']);
	});

	it('escapes the id rather than letting it change the path', async () => {
		const seen: string[] = [];
		const fetcher = (async (url: string) => {
			seen.push(url);
			return new Response('{}', { status: 200 });
		}) as unknown as typeof fetch;
		await getFile('../uploads', fetcher);
		expect(seen[0]).toBe('/api/v2/files/..%2Fuploads');
	});

	it('turns problem details into an ApiError carrying them', async () => {
		const fetcher = (async () =>
			new Response(
				JSON.stringify({ type: 'about:blank', title: 'Not Found', status: 404, detail: 'file not found', code: 'not_found' }),
				{ status: 404 }
			)) as unknown as typeof fetch;
		await expect(getFile('x', fetcher)).rejects.toMatchObject({ problem: { code: 'not_found', status: 404 } });
	});
});

describe('toError', () => {
	it('copes with a response that is not problem JSON', async () => {
		const error = await toError(new Response('<html>bad gateway</html>', { status: 502, statusText: 'Bad Gateway' }));
		expect(error).toBeInstanceOf(ApiError);
		expect(error.problem.status).toBe(502);
		expect(error.problem.code).toBe('unexpected');
	});
});
