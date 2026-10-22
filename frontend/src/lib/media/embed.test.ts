import { describe, expect, it } from 'vitest';
import { altText, embed, embedAll, formats, targets } from './embed';

const base = 'https://img.example.com';
const urls = (id: string, kinds: string[]) => ({
	viewer: `${base}/f/${id}`,
	image: `${base}/i/${id}.x`,
	thumb: kinds.includes('thumb') ? `${base}/i/${id}.th.jpg` : null,
	medium: kinds.includes('medium') ? `${base}/i/${id}.md.jpg` : null,
	frame: kinds.includes('frame') ? `${base}/i/${id}.fr.jpg` : null
});

const picture = { kind: 'image' as const, title: 'Beach', description: null, urls: urls('pic', ['thumb', 'medium']) };
const tiny = { kind: 'image' as const, title: null, description: null, urls: urls('tiny', ['thumb']) };
const clip = { kind: 'video' as const, title: 'Waves', description: 'The sea', urls: urls('vid', ['thumb', 'frame']) };
const bareClip = { kind: 'video' as const, title: null, description: null, urls: urls('bare', []) };
const song = { kind: 'audio' as const, title: 'Tide', description: null, urls: urls('mp3', []) };
const all = [picture, tiny, clip, bareClip, song];

describe('every format × target × kind', () => {
	for (const file of all) {
		for (const format of formats) {
			for (const target of targets) {
				it(`${file.urls.viewer} ${format} ${target}`, () => {
					const code = embed(file, format, target);
					expect(code.length).toBeGreaterThan(0);
					// Always points at something of ours, never at nothing.
					expect(code).toContain(base);
					expect(code).not.toContain('null');
					expect(code).not.toContain('undefined');
					if (format === 'viewer') expect(code).toBe(file.urls.viewer);
				});
			}
		}
	}
});

describe('images', () => {
	it('uses the derivative asked for', () => {
		expect(embed(picture, 'url', 'medium')).toBe(`${base}/i/pic.md.jpg`);
		expect(embed(picture, 'bbcode-linked', 'thumb')).toBe(
			`[url=${base}/f/pic][img]${base}/i/pic.th.jpg[/img][/url]`
		);
		expect(embed(picture, 'html-linked', 'image')).toBe(
			`<a href="${base}/f/pic"><img src="${base}/i/pic.x" alt="Beach"></a>`
		);
		expect(embed(picture, 'markdown', 'image')).toBe(`![Beach](${base}/i/pic.x)`);
	});
	it('falls back when a derivative does not exist', () => {
		expect(embed(tiny, 'url', 'medium')).toBe(`${base}/i/tiny.x`);
		expect(embed(tiny, 'url', 'frame')).toBe(`${base}/i/tiny.x`);
		expect(embed(tiny, 'html', 'thumb')).toBe(`<img src="${base}/i/tiny.th.jpg" alt="">`);
	});
});

describe('video and audio', () => {
	it('plays the media, with a still where only a picture fits', () => {
		expect(embed(clip, 'html', 'frame')).toBe(
			`<video src="${base}/i/vid.x" poster="${base}/i/vid.fr.jpg" controls preload="metadata"></video>`
		);
		expect(embed(clip, 'url', 'image')).toBe(`${base}/i/vid.x`);
		expect(embed(clip, 'url', 'thumb')).toBe(`${base}/i/vid.th.jpg`);
		expect(embed(clip, 'markdown', 'frame')).toBe(`[![The sea](${base}/i/vid.fr.jpg)](${base}/f/vid)`);
	});
	it('links when there is no still at all', () => {
		expect(embed(bareClip, 'bbcode', 'frame')).toBe(`[url]${base}/i/bare.x[/url]`);
		expect(embed(song, 'html', 'image')).toBe(
			`<audio src="${base}/i/mp3.x" controls preload="metadata"></audio>`
		);
		expect(embed(song, 'markdown', 'thumb')).toBe(`[Tide](${base}/f/mp3)`);
		expect(embed(song, 'bbcode-linked', 'thumb')).toBe(`[url=${base}/f/mp3]Tide[/url]`);
	});
});

describe('escaping', () => {
	const hostile = { ...picture, title: '"><script>[x]', description: null };
	it('keeps what people typed from breaking out', () => {
		expect(embed(hostile, 'html', 'image')).toBe(
			`<img src="${base}/i/pic.x" alt="&quot;&gt;&lt;script&gt;[x]">`
		);
		expect(embed(hostile, 'markdown', 'image')).toBe(`!["><script>\\[x\\]](${base}/i/pic.x)`);
		expect(embed({ ...song, title: '[b]Tide[/b]' }, 'bbcode-linked', 'image')).toBe(
			`[url=${base}/f/mp3]bTide/b[/url]`
		);
	});
});

describe('alt text', () => {
	it('is the description, then the title, never a filename', () => {
		expect(altText({ title: 'T', description: 'D' })).toBe('D');
		expect(altText({ title: 'T', description: null })).toBe('T');
		expect(altText({ title: null, description: null })).toBe('');
	});
});

it('embeds many files one per line', () => {
	expect(embedAll([picture, tiny], 'url', 'image')).toBe(`${base}/i/pic.x\n${base}/i/tiny.x`);
});
