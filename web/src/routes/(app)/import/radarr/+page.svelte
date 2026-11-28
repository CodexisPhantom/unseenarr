<script lang="ts">
	import { resolve } from '$app/paths';
	import { createMutation, createQuery, useQueryClient } from '@tanstack/svelte-query';
	import { api } from '#lib/api/client.js';
	import NoAccess from '#lib/components/no-access.svelte';
	import * as AlertDialog from '#lib/components/ui/alert-dialog/index.js';
	import { Button } from '#lib/components/ui/button/index.js';
	import { Checkbox } from '#lib/components/ui/checkbox/index.js';
	import { Input } from '#lib/components/ui/input/index.js';
	import { Label } from '#lib/components/ui/label/index.js';
	import * as Select from '#lib/components/ui/select/index.js';
	import { Switch } from '#lib/components/ui/switch/index.js';
	import * as Table from '#lib/components/ui/table/index.js';
	import {
		allResources,
		describeResources,
		moveItem,
		reportRows,
		resourceGroups,
		totals,
		type Report,
		type ResourceGroup
	} from '#lib/radarr-import.js';
	import { session } from '#lib/session.svelte.js';

	const allowed = $derived(session.can('import:run'));
	const queryClient = useQueryClient();

	const sources = createQuery(() => ({
		queryKey: ['import-sources'],
		enabled: allowed,
		queryFn: async () => {
			const { data, error } = await api.GET('/api/v1/import-sources');
			if (!data) throw new Error(error?.detail ?? 'The import sources could not be loaded.');
			return data;
		}
	}));
	const profiles = createQuery(() => ({
		queryKey: ['quality-profiles'],
		enabled: allowed,
		queryFn: async () => {
			const { data, error } = await api.GET('/api/v1/quality-profiles');
			if (!data) throw new Error(error?.detail ?? 'The profiles could not be loaded.');
			return data;
		}
	}));

	let name = $state('');
	let url = $state('');
	let apiKey = $state('');
	let useTarget = $state(false);
	let target = $state<number | null>(null);
	let usePaths = $state(false);
	let remotePath = $state('');
	let localPath = $state('');
	let resources = $state<ResourceGroup[]>(allResources());
	let importDefinitions = $state(false);

	let failure = $state('');
	let connection = $state<{ appName: string; instanceName: string; version: string } | null>(null);
	let report = $state<Report | null>(null);
	let reportName = $state('');
	let previewed = $state<number | null>(null);
	let importMode = $state<'preview' | 'run' | null>(null);
	let removing = $state<{ id: number; name: string } | null>(null);

	// The first profile is the target once the switch turns on.
	$effect(() => {
		if (useTarget && target === null && profiles.data?.length) target = profiles.data[0].id;
	});

	const rows = $derived(report ? reportRows(report) : []);
	const sum = $derived(report ? totals(report) : null);

	const test = createMutation(() => ({
		mutationFn: async () => {
			const { data, error } = await api.POST('/api/v1/import-sources/test', {
				body: { url: url.trim(), apiKey: apiKey.trim() }
			});
			if (!data) throw new Error(error?.detail ?? error?.title ?? 'The source did not answer.');
			return data;
		},
		onSuccess: (data) => {
			failure = '';
			connection = data;
		},
		onError: (error) => {
			connection = null;
			failure = error.message;
		}
	}));

	const create = createMutation(() => ({
		mutationFn: async () => {
			const { data, error } = await api.POST('/api/v1/import-sources', {
				body: {
					kind: 'radarr',
					name: name.trim(),
					url: url.trim(),
					apiKey: apiKey.trim(),
					targetQualityProfileId: useTarget ? target : null,
					remotePath: usePaths ? remotePath.trim() : null,
					localPath: usePaths ? localPath.trim() : null,
					resources,
					importQualityDefinitions: importDefinitions
				}
			});
			if (!data) throw new Error(error?.detail ?? error?.title ?? 'The source could not be saved.');
			return data;
		},
		onSuccess: () => {
			failure = '';
			connection = null;
			name = '';
			url = '';
			apiKey = '';
			void queryClient.invalidateQueries({ queryKey: ['import-sources'] });
		},
		onError: (error) => (failure = error.message)
	}));

	const remove = createMutation(() => ({
		mutationFn: async (id: number) => {
			const { error } = await api.DELETE('/api/v1/import-sources/{id}', {
				params: { path: { id } }
			});
			if (error) throw new Error(error.detail ?? error.title);
		},
		onSuccess: () => {
			failure = '';
			report = null;
			previewed = null;
			void queryClient.invalidateQueries({ queryKey: ['import-sources'] });
		},
		onError: (error) => (failure = error.message)
	}));

	const reorder = createMutation(() => ({
		mutationFn: async (ids: number[]) => {
			const { error } = await api.PATCH('/api/v1/import-sources/order', { body: { ids } });
			if (error) throw new Error(error.detail ?? error.title);
		},
		onSuccess: () => {
			failure = '';
			void queryClient.invalidateQueries({ queryKey: ['import-sources'] });
		},
		onError: (error) => (failure = error.message)
	}));

	const importer = createMutation(() => ({
		mutationFn: async ({ id, run }: { id: number; run: boolean }) => {
			const { data, error } = run
				? await api.POST('/api/v1/import-sources/{id}/run', { params: { path: { id } } })
				: await api.POST('/api/v1/import-sources/{id}/preview', { params: { path: { id } } });
			if (!data) throw new Error(error?.detail ?? error?.title ?? 'The import failed.');
			return { id, run, data };
		},
		onSuccess: ({ id, run, data }) => {
			failure = '';
			report = data;
			reportName = sources.data?.find((source) => source.id === id)?.name ?? '';
			previewed = run ? null : id;
			if (run) {
				for (const key of [
					'movies',
					'root-folders',
					'quality-profiles',
					'custom-formats',
					'tags',
					'collections',
					'indexers',
					'download-clients',
					'import-sources'
				]) {
					void queryClient.invalidateQueries({ queryKey: [key] });
				}
			}
			importMode = null;
		},
		onError: (error) => {
			failure = error.message;
			importMode = null;
		}
	}));

	function order(ids: number[], from: number, to: number) {
		reorder.mutate(moveItem(ids, from, to));
	}

	function requestImport(id: number, run: boolean) {
		importMode = run ? 'run' : 'preview';
		importer.mutate({ id, run });
	}

	function toggleResource(group: ResourceGroup, on: boolean) {
		if (on) {
			const next: ResourceGroup[] = [...resources, group];
			if (group === 'movies') {
				next.push('rootFolders');
				if (!useTarget) next.push('qualityProfiles');
			}
			resources = [...new Set(next)];
			return;
		}
		resources = resources.filter((value) =>
			value !== group &&
			!(value === 'movies' && (group === 'rootFolders' || (group === 'qualityProfiles' && !useTarget)))
		);
	}

	function chosen(group: ResourceGroup) {
		return resources.includes(group);
	}

	function lastImport(at: string | null | undefined) {
		return at === null || at === undefined ? 'Never' : new Date(at).toLocaleString();
	}
</script>

<svelte:head>
	<title>Radarr import · Unseenarr</title>
</svelte:head>

{#if !allowed}
	<NoAccess anyOf={['import:run']} />
{:else}
	<h1 class="text-2xl font-semibold">Import from Radarr</h1>
	<p class="mt-1 max-w-2xl text-sm text-muted-foreground">
		Read a running Radarr and merge what it has into this library. A source keeps a map of what
		it already imported, so running it again creates nothing twice. Preview first: a preview
		counts everything a run would do without writing. <a
			class="underline underline-offset-4"
			href={resolve('import')}>Adopt a library instead</a
		>.
	</p>

	{#if sources.isError}
		<p role="alert" class="mt-4 text-sm text-destructive">{sources.error.message}</p>
	{/if}
	{#if failure}
		<p role="alert" class="mt-4 text-sm text-destructive">{failure}</p>
	{/if}
	{#if importer.isPending}
		<p role="status" aria-live="polite" class="mt-4 text-sm text-muted-foreground">
			{importMode === 'run' ? 'Import in progress…' : 'Counting changes…'}
		</p>
	{/if}

	<h2 class="mt-8 font-semibold">Sources</h2>
	{#if sources.isPending}
		<p class="mt-2 text-sm text-muted-foreground">Loading…</p>
	{:else if !sources.data?.length}
		<p class="mt-2 text-sm text-muted-foreground">No source yet. Add one below.</p>
	{:else}
		<Table.Root class="mt-2">
			<Table.Header>
				<Table.Row>
					<Table.Head>Name</Table.Head>
					<Table.Head>URL</Table.Head>
					<Table.Head>Resources</Table.Head>
					<Table.Head>Last import</Table.Head>
					<Table.Head class="text-end">Actions</Table.Head>
				</Table.Row>
			</Table.Header>
			<Table.Body>
				{#each sources.data as source, index (source.id)}
					<Table.Row>
						<Table.Cell>
							<span class="font-medium">{source.name}</span>
							{#if source.importQualityDefinitions}
								<span class="ms-2 text-xs text-muted-foreground">quality sizes</span>
							{/if}
						</Table.Cell>
						<Table.Cell class="font-mono text-xs break-all">{source.url}</Table.Cell>
						<Table.Cell class="text-sm">{describeResources(source.resources)}</Table.Cell>
						<Table.Cell class="text-sm">{lastImport(source.lastImportAt)}</Table.Cell>
						<Table.Cell class="text-end">
							<div class="flex flex-wrap justify-end gap-2">
								<Button
									variant="ghost"
									size="sm"
									aria-label={`Move ${source.name} up`}
									disabled={index === 0 || reorder.isPending}
									onclick={() =>
										order(
											sources.data.map((item) => item.id),
											index,
											index - 1
										)}
								>
									↑
								</Button>
								<Button
									variant="ghost"
									size="sm"
									aria-label={`Move ${source.name} down`}
									disabled={index === sources.data.length - 1 || reorder.isPending}
									onclick={() =>
										order(
											sources.data.map((item) => item.id),
											index,
											index + 1
										)}
								>
									↓
								</Button>
								<Button
									variant="outline"
									size="sm"
									disabled={importer.isPending}
									onclick={() => requestImport(source.id, false)}
								>
									Preview
								</Button>
								<Button
									size="sm"
									disabled={importer.isPending || previewed !== source.id}
									title={previewed === source.id ? '' : 'Preview this source first.'}
									onclick={() => requestImport(source.id, true)}
								>
									Import
								</Button>
								<Button
									variant="ghost"
									size="sm"
									onclick={() => (removing = { id: source.id, name: source.name })}
								>
									Remove
								</Button>
							</div>
						</Table.Cell>
					</Table.Row>
				{/each}
			</Table.Body>
		</Table.Root>
		<p class="mt-2 text-xs text-muted-foreground">
			The first source is the one whose quality profile becomes the primary profile of a movie
			that more than one source has.
		</p>
	{/if}

	<section class="mt-8 rounded-lg border p-4" aria-labelledby="radarr-add">
		<h2 id="radarr-add" class="font-semibold">Add a source</h2>
		<div class="mt-3 grid gap-4 sm:grid-cols-2">
			<div class="grid gap-2">
				<Label for="radarr-name">Name</Label>
				<Input id="radarr-name" bind:value={name} placeholder="Radarr" />
			</div>
			<div class="grid gap-2">
				<Label for="radarr-url">URL</Label>
				<Input id="radarr-url" bind:value={url} placeholder="http://radarr:7878" />
			</div>
			<div class="grid gap-2">
				<Label for="radarr-key">API key</Label>
				<Input id="radarr-key" type="password" bind:value={apiKey} autocomplete="off" />
			</div>
			<div class="grid content-start gap-3">
				<Label class="gap-2 font-normal">
					<Switch bind:checked={useTarget} />
					Give every movie one quality profile
				</Label>
				{#if useTarget}
					<Select.Root
						type="single"
						value={target === null ? '' : String(target)}
						onValueChange={(value) => (target = Number(value))}
					>
						<Select.Trigger class="w-full">
							{profiles.data?.find((profile) => profile.id === target)?.name ??
								(profiles.isPending ? 'Loading…' : 'Choose a profile')}
						</Select.Trigger>
						<Select.Content>
							{#each profiles.data ?? [] as profile (profile.id)}
								<Select.Item value={String(profile.id)} label={profile.name} />
							{/each}
						</Select.Content>
					</Select.Root>
					<p class="text-xs text-muted-foreground">
						Its own quality profiles are not imported then.
					</p>
				{/if}
			</div>
			<div class="grid content-start gap-3 sm:col-span-2">
				<Label class="gap-2 font-normal">
					<Switch bind:checked={usePaths} />
					The source sees the files under another path
				</Label>
				{#if usePaths}
					<div class="grid gap-4 sm:grid-cols-2">
						<div class="grid gap-2">
							<Label for="radarr-remote">Path on the source</Label>
							<Input id="radarr-remote" bind:value={remotePath} placeholder="/movies" />
						</div>
						<div class="grid gap-2">
							<Label for="radarr-local">Path on this machine</Label>
							<Input id="radarr-local" bind:value={localPath} placeholder="D:\Movies" />
						</div>
					</div>
				{/if}
			</div>
			<div class="grid content-start gap-2 sm:col-span-2">
				<Label>Resources</Label>
				<p class="text-xs text-muted-foreground">
					Movies need root folders and, unless you choose one target profile, quality profiles.
				</p>
				<div class="grid gap-2 sm:grid-cols-2">
					{#each resourceGroups as group (group.value)}
						<Label class="gap-2 font-normal">
							<Checkbox
								checked={chosen(group.value)}
								disabled={
									(group.value === 'rootFolders' && chosen('movies')) ||
									(group.value === 'qualityProfiles' && chosen('movies') && !useTarget)
								}
								onCheckedChange={(on) => toggleResource(group.value, on === true)}
							/>
							{group.label}
						</Label>
					{/each}
				</div>
				<Label class="mt-2 gap-2 font-normal">
					<Switch bind:checked={importDefinitions} />
					Apply the source's quality sizes to the global limits
				</Label>
			</div>
		</div>
		<div class="mt-4 flex flex-wrap items-center gap-2">
			<Button
				variant="outline"
				disabled={url.trim() === '' || apiKey.trim() === '' || test.isPending}
				onclick={() => test.mutate()}
			>
				{test.isPending ? 'Testing…' : 'Test connection'}
			</Button>
			<Button
				disabled={name.trim() === '' ||
					url.trim() === '' ||
					apiKey.trim() === '' ||
					(useTarget && target === null) ||
					resources.length === 0 ||
					create.isPending}
				onclick={() => create.mutate()}
			>
				{create.isPending ? 'Saving…' : 'Save source'}
			</Button>
			{#if connection}
				<span class="text-sm text-muted-foreground">
					{connection.appName} {connection.version}
					{connection.instanceName ? `· ${connection.instanceName}` : ''}
				</span>
			{/if}
		</div>
	</section>

	{#if report && sum}
		<section class="mt-8 rounded-lg border p-4" aria-labelledby="radarr-report">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<h2 id="radarr-report" class="font-semibold">
					{previewed === null ? 'Import finished' : 'Preview'} · {reportName}
				</h2>
				<span class="text-sm text-muted-foreground">
					{sum.created} created, {sum.matched} already there, {sum.skipped} left out
				</span>
			</div>
			{#if rows.length === 0}
				<p class="mt-2 text-sm text-muted-foreground">Nothing to import.</p>
			{:else}
				<Table.Root class="mt-3">
					<Table.Header>
						<Table.Row>
							<Table.Head>Resource</Table.Head>
							<Table.Head class="text-end">Created</Table.Head>
							<Table.Head class="text-end">Already there</Table.Head>
							<Table.Head class="text-end">Left out</Table.Head>
						</Table.Row>
					</Table.Header>
					<Table.Body>
						{#each rows as row (row.group)}
							<Table.Row>
								<Table.Cell>{row.label}</Table.Cell>
								<Table.Cell class="text-end tabular-nums">{row.created}</Table.Cell>
								<Table.Cell class="text-end tabular-nums">{row.matched}</Table.Cell>
								<Table.Cell class="text-end tabular-nums">{row.skipped}</Table.Cell>
							</Table.Row>
						{/each}
					</Table.Body>
				</Table.Root>
			{/if}
			{#if report.mergedMovies.length > 0}
				<h3 class="mt-4 text-sm font-medium">Merged into movies already here</h3>
				<ul class="mt-1 grid gap-1 text-sm">
					{#each report.mergedMovies as title (title)}
						<li>{title}</li>
					{/each}
				</ul>
			{/if}
			{#if report.notes.length > 0}
				<h3 class="mt-4 text-sm font-medium">Notes</h3>
				<ul class="mt-1 grid gap-1 text-sm text-muted-foreground">
					{#each report.notes as note, index (index)}
						<li>{note}</li>
					{/each}
				</ul>
			{/if}
			{#if previewed !== null}
				<div class="mt-4 flex items-center gap-2">
					<Button
						disabled={importer.isPending}
						onclick={() => previewed !== null && requestImport(previewed, true)}
					>
						{importer.isPending ? 'Importing…' : 'Run import'}
					</Button>
					<span class="text-xs text-muted-foreground">
						Nothing has been written yet.
					</span>
				</div>
			{/if}
		</section>
	{/if}

	<AlertDialog.Root open={removing !== null} onOpenChange={(open) => !open && (removing = null)}>
		<AlertDialog.Content>
			<AlertDialog.Header>
				<AlertDialog.Title>Remove this source?</AlertDialog.Title>
				<AlertDialog.Description>
					Unseenarr removes {removing?.name} and its import map. The movies and other library items
					it imported stay.
				</AlertDialog.Description>
			</AlertDialog.Header>
			<AlertDialog.Footer>
				<AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
				<AlertDialog.Action
					onclick={() => {
						if (removing) remove.mutate(removing.id);
						removing = null;
					}}
				>
					Remove
				</AlertDialog.Action>
			</AlertDialog.Footer>
		</AlertDialog.Content>
	</AlertDialog.Root>
{/if}
