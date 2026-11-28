import { describe, expect, it } from 'vitest';
import {
	allResources,
	describeResources,
	moveItem,
	reportRows,
	totals,
	type Report
} from './radarr-import.js';

const none = { created: 0, matched: 0, skipped: 0 };

function report(overrides: Partial<Report> = {}): Report {
	return {
		rootFolders: none,
		qualityProfiles: none,
		customFormats: none,
		tags: none,
		collections: none,
		movies: none,
		indexers: none,
		downloadClients: none,
		qualityDefinitions: none,
		mergedMovies: [],
		notes: [],
		...overrides
	};
}

describe('reportRows', () => {
	it('keeps only the groups that did something, in report order', () => {
		const rows = reportRows(
			report({
				movies: { created: 2, matched: 1, skipped: 0 },
				qualityDefinitions: { created: 0, matched: 0, skipped: 9 },
				tags: { created: 0, matched: 3, skipped: 0 }
			})
		);
		expect(rows.map((row) => row.group)).toEqual(['tags', 'movies', 'qualityDefinitions']);
		expect(rows[1]).toMatchObject({ label: 'Movies', created: 2, matched: 1 });
	});

	it('is empty when nothing was found', () => {
		expect(reportRows(report())).toEqual([]);
	});
});

describe('totals', () => {
	it('sums every row', () => {
		const sum = totals(
			report({
				movies: { created: 2, matched: 3, skipped: 1 },
				tags: { created: 1, matched: 0, skipped: 4 }
			})
		);
		expect(sum).toEqual({ created: 3, matched: 3, skipped: 5 });
	});
});

describe('moveItem', () => {
	it('moves down and up', () => {
		expect(moveItem(['a', 'b', 'c'], 0, 2)).toEqual(['b', 'c', 'a']);
		expect(moveItem(['a', 'b', 'c'], 2, 0)).toEqual(['c', 'a', 'b']);
	});

	it('leaves the list alone when the move is not possible', () => {
		const items = ['a', 'b'];
		expect(moveItem(items, 1, 1)).toBe(items);
		expect(moveItem(items, -1, 0)).toBe(items);
		expect(moveItem(items, 0, 2)).toBe(items);
	});
});

describe('describeResources', () => {
	it('names all, none, or each chosen group', () => {
		expect(describeResources(allResources())).toBe('All resources');
		expect(describeResources([])).toBe('Nothing');
		expect(describeResources(['movies', 'tags'])).toBe('Tags, Movies');
	});
});
