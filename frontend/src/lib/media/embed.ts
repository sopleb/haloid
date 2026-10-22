// Embed codes: a pure function over format × target × kind. Bulk embed
// codes are what people move hosts for, so this is tested exhaustively.
import type { FileView } from '$lib/api/types';

export const formats = [
	'url',
	'viewer',
	'html',
	'html-linked',
	'bbcode',
	'bbcode-linked',
	'markdown'
] as const;
export type Format = (typeof formats)[number];

/** Which picture an embed shows. For video and audio the media itself is
 * always `image`; the target picks the still shown in its place. */
export const targets = ['image', 'medium', 'thumb', 'frame'] as const;
export type Target = (typeof targets)[number];

export const formatLabels: Record<Format, string> = {
	url: 'Direct link',
	viewer: 'Viewer page',
	html: 'HTML',
	'html-linked': 'HTML, linked',
	bbcode: 'BBCode',
	'bbcode-linked': 'BBCode, linked',
	markdown: 'Markdown'
};

export const targetLabels: Record<Target, string> = {
	image: 'Full size',
	medium: 'Medium',
	thumb: 'Thumbnail',
	frame: 'First frame'
};

type Embeddable = Pick<FileView, 'kind' | 'title' | 'description' | 'urls'>;

// Where a missing derivative falls back to: a small file has no medium, a
// still has no frame, and the full image is always there.
const fallbacks: Record<Target, Target[]> = {
	image: ['image'],
	medium: ['medium', 'image'],
	thumb: ['thumb', 'medium', 'image'],
	frame: ['frame', 'medium', 'image']
};

function pick(file: Embeddable, target: Target, stillOnly: boolean): string | null {
	for (const candidate of fallbacks[target]) {
		// The `image` of a video or a sound is the media, not a picture.
		if (stillOnly && candidate === 'image') continue;
		const url = file.urls[candidate];
		if (url) return url;
	}
	if (stillOnly) {
		return file.urls.frame ?? file.urls.medium ?? file.urls.thumb ?? null;
	}
	return file.urls.image ?? null;
}

const html = (text: string) =>
	text.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const markdownText = (text: string) => text.replace(/([\\[\]])/g, '\\$1');
const bbcodeText = (text: string) => text.replace(/[[\]]/g, '');

/** Alt text: the description, then the title, else empty — never the
 * filename, which describes nothing. */
export function altText(file: Pick<FileView, 'title' | 'description'>): string {
	return (file.description || file.title || '').trim();
}

export function embed(file: Embeddable, format: Format, target: Target): string {
	const viewer = file.urls.viewer;
	const media = file.urls.image ?? viewer;
	const alt = altText(file);
	const label = (file.title || '').trim() || viewer;

	if (format === 'viewer') return viewer;

	if (file.kind === 'image') {
		const src = pick(file, target, false) ?? viewer;
		switch (format) {
			case 'url':
				return src;
			case 'html':
				return `<img src="${html(src)}" alt="${html(alt)}">`;
			case 'html-linked':
				return `<a href="${html(viewer)}"><img src="${html(src)}" alt="${html(alt)}"></a>`;
			case 'bbcode':
				return `[img]${src}[/img]`;
			case 'bbcode-linked':
				return `[url=${viewer}][img]${src}[/img][/url]`;
			case 'markdown':
				return `![${markdownText(alt)}](${src})`;
		}
	}

	// Video and audio: the media plays from `image`; a still, when there is
	// one, stands in where a forum or chat can only show a picture.
	const still = file.kind === 'video' ? pick(file, target, true) : null;
	switch (format) {
		case 'url':
			return target === 'image' || !still ? media : still;
		case 'html':
			if (file.kind === 'audio') {
				return `<audio src="${html(media)}" controls preload="metadata"></audio>`;
			}
			return `<video src="${html(media)}"${still ? ` poster="${html(still)}"` : ''} controls preload="metadata"></video>`;
		case 'html-linked':
			return still
				? `<a href="${html(viewer)}"><img src="${html(still)}" alt="${html(alt)}"></a>`
				: `<a href="${html(viewer)}">${html(label)}</a>`;
		case 'bbcode':
			return still ? `[img]${still}[/img]` : `[url]${media}[/url]`;
		case 'bbcode-linked':
			return still
				? `[url=${viewer}][img]${still}[/img][/url]`
				: `[url=${viewer}]${bbcodeText(label)}[/url]`;
		case 'markdown':
			return still
				? `[![${markdownText(alt)}](${still})](${viewer})`
				: `[${markdownText(label)}](${viewer})`;
	}
}

/** The same code for many files, one per line — the bulk case. */
export function embedAll(files: Embeddable[], format: Format, target: Target): string {
	return files.map((file) => embed(file, format, target)).join('\n');
}
