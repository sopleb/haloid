import { error } from '@sveltejs/kit';
import { ApiError, getFile } from '$lib/api/client';
import type { PageLoad } from './$types';

export const load: PageLoad = async ({ params, fetch }) => {
	try {
		return { file: await getFile(params.id, fetch) };
	} catch (e) {
		// Carry the API's status through, so a missing file is a 404 page
		// rather than an unexplained failure.
		if (e instanceof ApiError) error(e.problem.status || 500, e.problem.detail);
		throw e;
	}
};
