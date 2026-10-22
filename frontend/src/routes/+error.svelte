<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
</script>

<section class="py-20 text-center">
	<h1 class="display text-5xl md:text-6xl">{page.status === 404 ? 'Not found' : 'Something went wrong'}</h1>
	<p class="mt-2 muted">{page.error?.message}</p>
	<a href={resolve('/')} class="mt-6 inline-block text-accent underline">Upload something</a>
</section>
