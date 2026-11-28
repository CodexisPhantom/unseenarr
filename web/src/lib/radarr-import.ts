// The Radarr import page's pure logic (docs/14-compatibility.md, section C).

/** The resource groups a source imports, in the order it imports them. */
export const resourceGroups = [
	{ value: 'rootFolders', label: 'Root folders' },
	{ value: 'qualityProfiles', label: 'Quality profiles' },
	{ value: 'customFormats', label: 'Custom formats' },
	{ value: 'tags', label: 'Tags' },
	{ value: 'collections', label: 'Collections' },
	{ value: 'movies', label: 'Movies' },
	{ value: 'indexers', label: 'Indexers' },
	{ value: 'downloadClients', label: 'Download clients' }
] as const;

export type ResourceGroup = (typeof resourceGroups)[number]['value'];

/** A source's groups. An empty list in a request means all of them. */
export function allResources(): ResourceGroup[] {
	return resourceGroups.map((group) => group.value);
}

export interface Counts {
	created: number;
	matched: number;
	skipped: number;
}

export interface Report {
	rootFolders: Counts;
	qualityProfiles: Counts;
	customFormats: Counts;
	tags: Counts;
	collections: Counts;
	movies: Counts;
	indexers: Counts;
	downloadClients: Counts;
	qualityDefinitions: Counts;
	mergedMovies: string[];
	notes: string[];
}

export interface ReportRow extends Counts {
	group: ResourceGroup | 'qualityDefinitions';
	label: string;
}

const reportGroups = [
	...resourceGroups,
	{ value: 'qualityDefinitions', label: 'Quality definitions' }
] as const;

/** The report's rows that did something, in report order. */
export function reportRows(report: Report): ReportRow[] {
	return reportGroups
		.map((group) => ({ group: group.value, label: group.label, ...report[group.value] }))
		.filter((row) => row.created > 0 || row.matched > 0 || row.skipped > 0);
}

/** The report as one line: how much a run created, matched and left out. */
export function totals(report: Report): Counts {
	return reportRows(report).reduce(
		(sum, row) => ({
			created: sum.created + row.created,
			matched: sum.matched + row.matched,
			skipped: sum.skipped + row.skipped
		}),
		{ created: 0, matched: 0, skipped: 0 }
	);
}

/** The list with the item at `from` moved to `to`; out of range is a no-op. */
export function moveItem<T>(items: T[], from: number, to: number): T[] {
	if (from === to || from < 0 || to < 0 || from >= items.length || to >= items.length) {
		return items;
	}
	const next = [...items];
	const [item] = next.splice(from, 1);
	next.splice(to, 0, item);
	return next;
}

/** The groups of a source as a line, for its row in the list. */
export function describeResources(resources: string[]): string {
	const chosen = resourceGroups.filter((group) => resources.includes(group.value));
	if (chosen.length === resourceGroups.length) return 'All resources';
	if (chosen.length === 0) return 'Nothing';
	return chosen.map((group) => group.label).join(', ');
}
