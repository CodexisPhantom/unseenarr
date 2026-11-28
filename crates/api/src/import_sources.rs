//! The Radarr import engine (docs/14-compatibility.md, section C). It reads a
//! running Radarr and merges what it finds into this library. `apply == false`
//! walks the same path without writing, so the preview counts match a run.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use arr_compat::prowlarr::{ProwlarrClient, ProwlarrError, feed_url};
use arr_compat::radarr::client::RadarrClient;
use arr_compat::radarr::dto::{
    Field, MovieFileResource, MovieResource, MovieStatusType, QualityDefinitionResource, Timestamp,
};
use arr_compat::radarr::{RadarrError, map};
use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use db::entities::import_sources::Model as Source;
use db::import_sources as store;
use db::movies::{Metadata, MovieError, NewFile, NewMovie, Patch};
use db::{
    collections, custom_formats, download_clients, indexers, movies, profiles, quality_definitions,
    root_folders, tags,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unseenarr_core::metadata::{CollectionRef, MetadataProvider};
use unseenarr_core::profile::{self, Item};
use utoipa::ToSchema;

use crate::AppState;
use crate::access::{ImportRun, Require};
use crate::error::Problem;
use crate::tmdb;

/// How many source resources of one kind were created, found or left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct Counts {
    pub created: u32,
    pub matched: u32,
    pub skipped: u32,
}

/// What an import run did, or would do when it is a preview.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub root_folders: Counts,
    pub quality_profiles: Counts,
    pub custom_formats: Counts,
    pub tags: Counts,
    pub collections: Counts,
    pub movies: Counts,
    pub indexers: Counts,
    pub download_clients: Counts,
    pub quality_definitions: Counts,
    /// Titles of movies that already existed and were merged into. Empty when
    /// the run created every movie.
    pub merged_movies: Vec<String>,
    /// Short lines about what was renamed, left out or needs the user.
    pub notes: Vec<String>,
}

fn failed(error: impl std::fmt::Debug) -> Problem {
    tracing::error!(?error, "radarr import");
    Problem::internal()
}

fn radarr_failed(error: RadarrError) -> Problem {
    let mut problem = Problem::new(
        StatusCode::BAD_GATEWAY,
        "import_source_failed",
        "The source did not answer",
    );
    problem.detail = Some(error.to_string());
    problem
}

fn prowlarr_failed(error: ProwlarrError) -> Problem {
    let mut problem = Problem::new(
        StatusCode::BAD_GATEWAY,
        "import_source_failed",
        "The source did not answer",
    );
    problem.detail = Some(error.to_string());
    tracing::error!(?error, "prowlarr import");
    problem
}

/// The local indexer kind for a Prowlarr protocol, or `None` when the
/// protocol has no local kind.
fn prowlarr_kind(protocol: &str) -> Option<&'static str> {
    if protocol.eq_ignore_ascii_case("torrent") {
        Some("torznab")
    } else if protocol.eq_ignore_ascii_case("usenet") {
        Some("newznab")
    } else {
        None
    }
}

fn trim_separators(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

/// The path on this machine for a path the source reports. The prefixes are
/// the source's and the local one; a path outside the source prefix is kept.
fn local_path(remote_path: Option<&str>, local_path: Option<&str>, path: &str) -> String {
    let (Some(remote), Some(local)) = (remote_path, local_path) else {
        return path.to_owned();
    };
    let (remote, local) = (trim_separators(remote), trim_separators(local));
    if remote.is_empty() {
        return path.to_owned();
    }
    let Some(rest) = path.strip_prefix(remote) else {
        return path.to_owned();
    };
    if rest.is_empty() {
        return local.to_owned();
    }
    if !rest.starts_with(['/', '\\']) {
        return path.to_owned();
    }
    let rest = if local.contains('\\') {
        rest.to_owned()
    } else {
        rest.replace('\\', "/")
    };
    format!("{local}{rest}")
}

/// `path` below `folder`, with `/` between the parts.
fn relative_to(folder: &str, path: &str) -> Option<String> {
    let folder = trim_separators(folder).replace('\\', "/");
    let path = path.replace('\\', "/");
    let rest = path.strip_prefix(&format!("{folder}/"))?;
    (!rest.is_empty()).then(|| rest.to_owned())
}

fn availability_rank(value: &str) -> Option<u8> {
    match value {
        "announced" => Some(0),
        "inCinemas" => Some(1),
        "released" => Some(2),
        _ => None,
    }
}

/// The earlier of two minimum availabilities; an unknown value keeps `a`.
fn earlier_availability<'a>(a: &'a str, b: &'a str) -> &'a str {
    match (availability_rank(a), availability_rank(b)) {
        (Some(x), Some(y)) if y < x => b,
        _ => a,
    }
}

/// Both tag sets, sorted, once each.
fn tag_union(a: &[i32], b: &[i32]) -> Vec<i32> {
    let mut all: Vec<i32> = a.iter().chain(b).copied().collect();
    all.sort_unstable();
    all.dedup();
    all
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A date, or `None` for an absent one or the .NET minimum.
fn date(value: Option<&Timestamp>) -> Option<String> {
    value
        .and_then(|stamp| non_empty(&stamp.0))
        .filter(|text| !text.starts_with("0001-"))
}

/// The status Unseenarr stores: it has no `tba` or `deleted`.
fn status_wire(status: MovieStatusType) -> &'static str {
    match status {
        MovieStatusType::InCinemas => "inCinemas",
        MovieStatusType::Released => "released",
        _ => "announced",
    }
}

fn last_segment(path: &str) -> String {
    trim_separators(path)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn field_text(fields: &[Field], name: &str) -> Option<String> {
    let value = fields
        .iter()
        .find(|field| field.name == name)?
        .value
        .as_ref()?;
    match value {
        Value::String(text) => non_empty(text),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// A resource already in the library: its id and what makes two of them equal.
struct Known {
    id: i32,
    name: String,
    signature: Value,
}

enum Pick {
    Matched(i32),
    Create { name: String, renamed: bool },
    Skip,
}

/// Decides what a source resource named `name` becomes: the same stored one,
/// a new one, or (when `name` is taken by a different one) the same under
/// `alt`.
fn pick(known: &[Known], name: &str, alt: &str, signature: &Value) -> Pick {
    let find = |wanted: &str| known.iter().find(|item| item.name == wanted);
    match find(name) {
        None => Pick::Create {
            name: name.to_owned(),
            renamed: false,
        },
        Some(item) if item.signature == *signature => Pick::Matched(item.id),
        Some(_) => match find(alt) {
            None => Pick::Create {
                name: alt.to_owned(),
                renamed: true,
            },
            Some(item) if item.signature == *signature => Pick::Matched(item.id),
            Some(_) => Pick::Skip,
        },
    }
}

/// What makes two quality profiles equal. Zero scores are left out: a profile
/// stores none for them.
struct ProfileRules<'a> {
    items: &'a Value,
    cutoff: i32,
    language: &'a str,
    upgrade_allowed: bool,
    min_format_score: i32,
    cutoff_format_score: i32,
    min_upgrade_format_score: i32,
    /// Local custom format id and score.
    scores: Vec<(i32, i32)>,
}

impl ProfileRules<'_> {
    fn signature(mut self) -> Value {
        self.scores.retain(|(_, score)| *score != 0);
        self.scores.sort_unstable();
        json!({
            "items": self.items,
            "cutoff": self.cutoff,
            "language": self.language,
            "upgradeAllowed": self.upgrade_allowed,
            "minFormatScore": self.min_format_score,
            "cutoffFormatScore": self.cutoff_format_score,
            "minUpgradeFormatScore": self.min_upgrade_format_score,
            "scores": self.scores,
        })
    }
}

/// Ids of the source ids whose import is running. The server is one process.
// An in-process guard is enough because Unseenarr is a single process; a database
// lock is not used on purpose.
/// It lives in `AppState`, so each app has its own set.
#[derive(Clone, Default)]
pub struct RunSet(Arc<Mutex<HashSet<i32>>>);

/// Holds a source id in its [`RunSet`] until dropped, panic included.
struct RunGuard {
    set: RunSet,
    id: i32,
}

impl RunGuard {
    /// `None` when `id` is already running.
    fn acquire(set: &RunSet, id: i32) -> Option<Self> {
        let mut running = set.0.lock().unwrap_or_else(PoisonError::into_inner);
        // Lazy on purpose: an eagerly built `Self` would be dropped while
        // `running` is locked when `id` is taken, and its `Drop` would deadlock.
        running.insert(id).then(|| Self {
            set: set.clone(),
            id,
        })
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.set
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

struct Run<'a> {
    state: &'a AppState,
    source: &'a Source,
    apply: bool,
    report: Report,
    synthetic: i32,
    groups: Option<HashSet<String>>,
    /// Local root folders: id and path without trailing separator.
    roots: Vec<(i32, String)>,
    tag_ids: HashMap<i32, i32>,
    format_ids: HashMap<i32, i32>,
    profile_ids: HashMap<i32, i32>,
    /// Local collection id by TMDB id.
    collection_ids: HashMap<i32, i32>,
}

impl Run<'_> {
    fn path(&self, path: &str) -> String {
        local_path(
            self.source.remote_path.as_deref(),
            self.source.local_path.as_deref(),
            path,
        )
    }

    /// Whether the source asked for this group. No list means all of them.
    fn wants(&self, group: &str) -> bool {
        self.groups
            .as_ref()
            .is_none_or(|groups| groups.contains(group))
    }

    /// An id for something a preview would create.
    fn placeholder(&mut self) -> i32 {
        self.synthetic -= 1;
        self.synthetic
    }

    async fn mapped(&self, resource: &str, id: i32) -> Result<Option<i32>, Problem> {
        store::map_get(&self.state.db, self.source.id, resource, id)
            .await
            .map_err(failed)
    }

    async fn remember(&self, resource: &str, id: i32, local: i32) -> Result<(), Problem> {
        if !self.apply {
            return Ok(());
        }
        store::map_put(&self.state.db, self.source.id, resource, id, local)
            .await
            .map_err(failed)
    }

    fn renamed_name(&self, name: &str) -> String {
        format!("{name} ({})", self.source.name)
    }

    async fn root_folders(
        &mut self,
        remote: &[arr_compat::radarr::dto::RootFolderResource],
    ) -> Result<(), Problem> {
        let db = &self.state.db;
        self.roots = root_folders::list(db)
            .await
            .map_err(failed)?
            .into_iter()
            .map(|row| (row.id, trim_separators(&row.path).to_owned()))
            .collect();
        for folder in remote {
            let path = self.path(&folder.path);
            let key = trim_separators(&path).to_owned();
            let mapped = self.mapped(store::ROOT_FOLDER, folder.id).await?;
            if mapped.is_some_and(|id| self.roots.iter().any(|(known, _)| *known == id)) {
                self.report.root_folders.matched += 1;
                continue;
            }
            if let Some(id) = self
                .roots
                .iter()
                .find(|(_, p)| *p == key)
                .map(|(id, _)| *id)
            {
                self.report.root_folders.matched += 1;
                self.remember(store::ROOT_FOLDER, folder.id, id).await?;
                continue;
            }
            let id = if self.apply {
                root_folders::create(&self.state.db, path)
                    .await
                    .map_err(failed)?
                    .id
            } else {
                self.placeholder()
            };
            self.roots.push((id, key));
            self.report.root_folders.created += 1;
            self.remember(store::ROOT_FOLDER, folder.id, id).await?;
        }
        Ok(())
    }

    async fn tags(
        &mut self,
        remote: &[arr_compat::radarr::dto::TagResource],
    ) -> Result<(), Problem> {
        let mut known: Vec<(i32, String)> = tags::list(&self.state.db)
            .await
            .map_err(failed)?
            .into_iter()
            .map(|row| (row.id, row.label))
            .collect();
        for tag in remote {
            let Ok(label) = crate::tags::validate_label(&tag.label) else {
                self.report.tags.skipped += 1;
                self.report.notes.push(format!(
                    "Tag \"{}\" has a label Unseenarr does not accept.",
                    tag.label
                ));
                continue;
            };
            let mapped = self.mapped(store::TAG, tag.id).await?;
            if let Some(id) = mapped.filter(|id| known.iter().any(|(k, _)| k == id)) {
                self.tag_ids.insert(tag.id, id);
                self.report.tags.matched += 1;
                continue;
            }
            if let Some(id) = known.iter().find(|(_, l)| *l == label).map(|(id, _)| *id) {
                self.tag_ids.insert(tag.id, id);
                self.report.tags.matched += 1;
                self.remember(store::TAG, tag.id, id).await?;
                continue;
            }
            let id = if self.apply {
                match tags::create(&self.state.db, label.clone()).await {
                    Ok(row) => row.id,
                    Err(tags::TagError::Db(error)) => return Err(failed(error)),
                    Err(_) => {
                        self.report.tags.skipped += 1;
                        self.report.notes.push(format!(
                            "Tag \"{}\" has a label Unseenarr does not accept.",
                            tag.label
                        ));
                        continue;
                    }
                }
            } else {
                self.placeholder()
            };
            known.push((id, label));
            self.tag_ids.insert(tag.id, id);
            self.report.tags.created += 1;
            self.remember(store::TAG, tag.id, id).await?;
        }
        Ok(())
    }

    async fn custom_formats(
        &mut self,
        remote: &[arr_compat::radarr::dto::CustomFormatResource],
    ) -> Result<(), Problem> {
        let mut known: Vec<Known> = custom_formats::list(&self.state.db)
            .await
            .map_err(failed)?
            .into_iter()
            .map(|row| Known {
                id: row.id,
                name: row.name,
                signature: row.specifications,
            })
            .collect();
        for format in remote {
            let signature = serde_json::to_value(&format.specifications).map_err(failed)?;
            let mapped = self.mapped(store::CUSTOM_FORMAT, format.id).await?;
            if let Some(id) = mapped.filter(|id| known.iter().any(|k| k.id == *id)) {
                self.format_ids.insert(format.id, id);
                self.report.custom_formats.matched += 1;
                continue;
            }
            let alt = self.renamed_name(&format.name);
            match pick(&known, &format.name, &alt, &signature) {
                Pick::Matched(id) => {
                    self.format_ids.insert(format.id, id);
                    self.report.custom_formats.matched += 1;
                    self.remember(store::CUSTOM_FORMAT, format.id, id).await?;
                }
                Pick::Skip => {
                    self.report.custom_formats.skipped += 1;
                    self.report.notes.push(format!(
                        "Custom format \"{}\" differs from the stored one and \"{alt}\" is taken too; skipped.",
                        format.name
                    ));
                }
                Pick::Create { name, renamed } => {
                    if renamed {
                        self.report.notes.push(format!(
                            "Custom format \"{}\" differs from the stored one; imported as \"{name}\".",
                            format.name
                        ));
                    }
                    let id = if self.apply {
                        custom_formats::create(
                            &self.state.db,
                            custom_formats::Values {
                                name: name.clone(),
                                include_when_renaming: format
                                    .include_custom_format_when_renaming
                                    .unwrap_or(false),
                                specifications: signature.clone(),
                                trash_id: None,
                                trash_synced: false,
                            },
                        )
                        .await
                        .map_err(failed)?
                        .id
                    } else {
                        self.placeholder()
                    };
                    known.push(Known {
                        id,
                        name,
                        signature,
                    });
                    self.format_ids.insert(format.id, id);
                    self.report.custom_formats.created += 1;
                    self.remember(store::CUSTOM_FORMAT, format.id, id).await?;
                }
            }
        }
        Ok(())
    }

    async fn quality_profiles(
        &mut self,
        remote: &[arr_compat::radarr::dto::QualityProfileResource],
    ) -> Result<(), Problem> {
        if self.source.target_quality_profile_id.is_some() {
            if !remote.is_empty() {
                self.report.quality_profiles.skipped += count(remote.len());
                self.report.notes.push(
                    "Quality profiles are not imported: every movie gets the target profile."
                        .to_owned(),
                );
            }
            return Ok(());
        }
        let mut known: Vec<Known> = Vec::new();
        for row in profiles::list(&self.state.db).await.map_err(failed)? {
            let scores = profiles::format_scores(&self.state.db, row.id)
                .await
                .map_err(failed)?
                .into_iter()
                .map(|entry| (entry.custom_format_id, entry.score))
                .collect();
            known.push(Known {
                id: row.id,
                name: row.name,
                signature: ProfileRules {
                    items: &row.items,
                    cutoff: row.cutoff_quality_id,
                    language: &row.language,
                    upgrade_allowed: row.upgrade_allowed,
                    min_format_score: row.min_format_score,
                    cutoff_format_score: row.cutoff_format_score,
                    min_upgrade_format_score: row.min_upgrade_format_score,
                    scores,
                }
                .signature(),
            });
        }
        for item in remote {
            let mapped = self.mapped(store::QUALITY_PROFILE, item.id).await?;
            if let Some(id) = mapped.filter(|id| known.iter().any(|k| k.id == *id)) {
                self.profile_ids.insert(item.id, id);
                self.report.quality_profiles.matched += 1;
                continue;
            }
            let items: Vec<Item> = map::profile_items(item);
            let language = map::language_code(&item.language)
                .unwrap_or_else(|| profile::LANGUAGE_ANY.to_owned());
            let min_upgrade = item.min_upgrade_format_score.max(1);
            let rules = profile::Profile {
                name: item.name.clone(),
                items: items.clone(),
                cutoff_quality_id: item.cutoff,
                language: language.clone(),
                min_format_score: item.min_format_score,
                cutoff_format_score: item.cutoff_format_score,
                min_upgrade_format_score: min_upgrade,
            };
            if let Err((field, message)) = profile::validate(&rules) {
                self.report.quality_profiles.skipped += 1;
                self.report.notes.push(format!(
                    "Quality profile \"{}\" is invalid ({field} {message}); skipped.",
                    item.name
                ));
                continue;
            }
            let items = serde_json::to_value(&items).map_err(failed)?;
            let scores: Vec<(i32, i32)> = item
                .format_items
                .iter()
                .filter_map(|entry| {
                    self.format_ids
                        .get(&entry.format)
                        .map(|id| (*id, entry.score))
                })
                .collect();
            let signature = ProfileRules {
                items: &items,
                cutoff: item.cutoff,
                language: &language,
                upgrade_allowed: item.upgrade_allowed,
                min_format_score: item.min_format_score,
                cutoff_format_score: item.cutoff_format_score,
                min_upgrade_format_score: min_upgrade,
                scores,
            }
            .signature();
            let alt = self.renamed_name(&item.name);
            match pick(&known, &item.name, &alt, &signature) {
                Pick::Matched(id) => {
                    self.profile_ids.insert(item.id, id);
                    self.report.quality_profiles.matched += 1;
                    self.remember(store::QUALITY_PROFILE, item.id, id).await?;
                }
                Pick::Skip => {
                    self.report.quality_profiles.skipped += 1;
                    self.report.notes.push(format!(
                        "Quality profile \"{}\" differs from the stored one and \"{alt}\" is taken too; skipped.",
                        item.name
                    ));
                }
                Pick::Create { name, renamed } => {
                    if renamed {
                        self.report.notes.push(format!(
                            "Quality profile \"{}\" differs from the stored one; imported as \"{name}\".",
                            item.name
                        ));
                    }
                    let id = if self.apply {
                        let scores = item
                            .format_items
                            .iter()
                            .filter_map(|entry| {
                                self.format_ids
                                    .get(&entry.format)
                                    .map(|id| profiles::FormatScore {
                                        custom_format_id: *id,
                                        score: entry.score,
                                    })
                            })
                            .collect();
                        profiles::create(
                            &self.state.db,
                            profiles::Values {
                                name: name.clone(),
                                upgrade_allowed: item.upgrade_allowed,
                                cutoff_quality_id: item.cutoff,
                                items: items.clone(),
                                language,
                                min_format_score: item.min_format_score,
                                cutoff_format_score: item.cutoff_format_score,
                                min_upgrade_format_score: min_upgrade,
                                format_scores: scores,
                            },
                        )
                        .await
                        .map_err(failed)?
                        .id
                    } else {
                        self.placeholder()
                    };
                    known.push(Known {
                        id,
                        name,
                        signature,
                    });
                    self.profile_ids.insert(item.id, id);
                    self.report.quality_profiles.created += 1;
                    self.remember(store::QUALITY_PROFILE, item.id, id).await?;
                }
            }
        }
        Ok(())
    }

    async fn collections(
        &mut self,
        remote: &[arr_compat::radarr::dto::CollectionResource],
    ) -> Result<(), Problem> {
        let mut provider: Option<Box<dyn MetadataProvider>> = None;
        for item in remote {
            if let Some(found) = collections::find_by_tmdb_id(&self.state.db, item.tmdb_id)
                .await
                .map_err(failed)?
            {
                self.collection_ids.insert(item.tmdb_id, found.id);
                self.report.collections.matched += 1;
                self.remember(store::COLLECTION, item.id, found.id).await?;
                continue;
            }
            let id = if self.apply {
                if provider.is_none() {
                    provider = Some(tmdb::client(self.state).await?);
                }
                let Some(provider) = provider.as_deref() else {
                    return Err(Problem::internal());
                };
                let id = collections::ensure(
                    &self.state.db,
                    provider,
                    &CollectionRef {
                        tmdb_id: item.tmdb_id,
                        name: item.title.clone(),
                    },
                )
                .await
                .map_err(failed)?;
                collections::update(
                    &self.state.db,
                    id,
                    collections::Patch {
                        monitored: Some(item.monitored),
                        ..collections::Patch::default()
                    },
                )
                .await
                .map_err(failed)?;
                id
            } else {
                self.placeholder()
            };
            self.collection_ids.insert(item.tmdb_id, id);
            self.report.collections.created += 1;
            self.remember(store::COLLECTION, item.id, id).await?;
        }
        Ok(())
    }

    /// Local tag ids for remote ones, sorted. Unmapped remote ids are dropped.
    fn local_tags(&self, remote: &[i32]) -> Vec<i32> {
        let mut tags: Vec<i32> = remote
            .iter()
            .filter_map(|id| self.tag_ids.get(id).copied())
            .collect();
        tags.sort_unstable();
        tags
    }

    fn skip_movie(&mut self, movie: &MovieResource, why: &str) {
        self.report.movies.skipped += 1;
        self.report
            .notes
            .push(format!("Movie \"{}\" skipped: {why}.", movie.title));
    }

    async fn movies(&mut self, remote: &[MovieResource]) -> Result<(), Problem> {
        for movie in remote {
            let profile_id = match self.source.target_quality_profile_id {
                Some(id) => Some(id),
                None => self.profile_ids.get(&movie.quality_profile_id).copied(),
            };
            let Some(profile_id) = profile_id else {
                self.skip_movie(movie, "its quality profile was not imported");
                continue;
            };
            let incoming_tags: Vec<i32> = movie
                .tags
                .iter()
                .filter_map(|id| self.tag_ids.get(id).copied())
                .collect();
            let availability = status_wire(movie.minimum_availability);
            match movies::find_movie_by_tmdb_id(&self.state.db, movie.tmdb_id)
                .await
                .map_err(failed)?
            {
                Some(existing) => {
                    if self.mapped(store::MOVIE, movie.id).await?.is_some() {
                        self.report.movies.matched += 1;
                        continue;
                    }
                    self.report.movies.matched += 1;
                    if !movie.title.trim().is_empty() {
                        self.report.merged_movies.push(movie.title.clone());
                    }
                    if self.apply {
                        self.merge(movie, &existing, profile_id, &incoming_tags, availability)
                            .await?;
                        self.remember(store::MOVIE, movie.id, existing.model.id)
                            .await?;
                    }
                }
                None => {
                    self.create_movie(movie, profile_id, &incoming_tags, availability)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn create_movie(
        &mut self,
        movie: &MovieResource,
        profile_id: i32,
        tag_ids: &[i32],
        availability: &str,
    ) -> Result<(), Problem> {
        let root = self.path(&movie.root_folder_path);
        let Some(root_id) = self
            .roots
            .iter()
            .find(|(_, path)| *path == trim_separators(&root))
            .map(|(id, _)| *id)
        else {
            self.skip_movie(movie, "its root folder was not imported");
            return Ok(());
        };
        let folder_name = non_empty(&movie.folder_name)
            .map(|name| last_segment(&name))
            .unwrap_or_else(|| last_segment(&movie.path));
        if folder_name.is_empty() {
            self.skip_movie(movie, "it has no folder name");
            return Ok(());
        }
        if !self.apply {
            self.report.movies.created += 1;
            return Ok(());
        }
        let files: Vec<NewFile> = movie.movie_file.iter().map(new_file).collect();
        let new = NewMovie {
            metadata: metadata(movie),
            root_folder_id: root_id,
            folder_name,
            monitored: movie.monitored,
            minimum_availability: availability.to_owned(),
            collection_id: movie
                .collection
                .as_ref()
                .and_then(|c| self.collection_ids.get(&c.tmdb_id).copied()),
        };
        match movies::create_with_files(&self.state.db, new, &[profile_id], tag_ids, &files).await {
            Ok(created) => {
                self.report.movies.created += 1;
                self.remember(store::MOVIE, movie.id, created.model.id)
                    .await?;
                if let Some(file) = &movie.movie_file {
                    let stored = movies::files_of(&self.state.db, created.model.id)
                        .await
                        .map_err(failed)?;
                    if let Some(row) = stored.iter().find(|row| row.movie_profile_id.is_some()) {
                        self.remember(store::MOVIE_FILE, file.id, row.id).await?;
                    }
                }
            }
            Err(MovieError::TmdbTaken | MovieError::FolderTaken) => {
                self.skip_movie(movie, "its folder name is already used in the root folder");
            }
            Err(error) => return Err(failed(error)),
        }
        Ok(())
    }

    /// Merges a movie the library has into the source's view of it. The first
    /// source keeps monitoring, root folder, folder name and primary profile.
    async fn merge(
        &mut self,
        movie: &MovieResource,
        existing: &movies::Movie,
        profile_id: i32,
        incoming_tags: &[i32],
        availability: &str,
    ) -> Result<(), Problem> {
        let db = &self.state.db;
        let id = existing.model.id;
        let mut link = existing
            .profiles
            .iter()
            .find(|link| link.quality_profile_id == profile_id)
            .cloned();
        let added = link.is_none();
        if added {
            let updated = movies::add_profile(db, id, profile_id)
                .await
                .map_err(failed)?;
            link = updated
                .profiles
                .into_iter()
                .find(|link| link.quality_profile_id == profile_id);
        }
        if let (Some(file), Some(link)) = (&movie.movie_file, link) {
            let file_id = self.adopt(movie, existing, file).await?;
            if let Some((file_id, size)) = file_id {
                let swap = match (added, link.file_id) {
                    (false, Some(current)) => movies::files_by_ids(&self.state.db, &[current])
                        .await
                        .map_err(failed)?
                        .get(&current)
                        .is_none_or(|row| size > row.size),
                    _ => true,
                };
                if swap {
                    movies::set_managed_file(&self.state.db, link.id, Some(file_id))
                        .await
                        .map_err(failed)?;
                }
            }
        }
        let tags = tag_union(&existing.tags, incoming_tags);
        let stored = existing.model.minimum_availability.as_str();
        let earlier = earlier_availability(stored, availability);
        let patch = Patch {
            tags: (tags != existing.tags).then_some(tags),
            minimum_availability: (earlier != stored).then(|| earlier.to_owned()),
            ..Patch::default()
        };
        if patch.tags.is_some() || patch.minimum_availability.is_some() {
            movies::update(&self.state.db, id, patch)
                .await
                .map_err(failed)?;
        }
        Ok(())
    }

    /// Stores the source's file as a file of the existing movie. Returns its
    /// id and size, whether it was new or already stored.
    async fn adopt(
        &mut self,
        movie: &MovieResource,
        existing: &movies::Movie,
        file: &MovieFileResource,
    ) -> Result<Option<(i32, i64)>, Problem> {
        let mut new = new_file(file);
        if let Some(mapped) = non_empty(&file.path).map(|path| self.path(&path)) {
            let root = self
                .roots
                .iter()
                .find(|(id, _)| *id == existing.model.root_folder_id)
                .map(|(_, path)| path.clone())
                .unwrap_or_default();
            let folder = format!("{root}/{}", existing.model.folder_name);
            if let Some(relative) = relative_to(&folder, &mapped) {
                new.relative_path = relative;
            } else {
                self.report.notes.push(format!(
                    "Movie \"{}\": the file stays at {mapped}, outside the movie folder.",
                    movie.title
                ));
                new.relative_path = mapped;
            }
        }
        if new.relative_path.is_empty() {
            return Ok(None);
        }
        let db = &self.state.db;
        movies::adopt_files(db, existing.model.id, std::slice::from_ref(&new))
            .await
            .map_err(failed)?;
        let stored = movies::files_of(db, existing.model.id)
            .await
            .map_err(failed)?;
        Ok(stored
            .iter()
            .find(|row| row.relative_path == new.relative_path)
            .map(|row| (row.id, row.size)))
    }

    /// Applies the source's global size limits to the rows it lists. A quality
    /// the library does not have is skipped: the qualities are ours, not its.
    async fn quality_definitions(
        &mut self,
        remote: &[QualityDefinitionResource],
    ) -> Result<(), Problem> {
        for definition in remote {
            let id = definition.quality.id;
            let Some(current) = quality_definitions::find(&self.state.db, id)
                .await
                .map_err(failed)?
            else {
                self.report.quality_definitions.skipped += 1;
                continue;
            };
            let values = quality_definitions::Values {
                title: non_empty(&definition.title).unwrap_or_else(|| current.title.clone()),
                min_size: definition.min_size.unwrap_or(current.min_size),
                preferred_size: definition.preferred_size,
                max_size: definition.max_size,
            };
            if values.title == current.title
                && values.min_size == current.min_size
                && values.preferred_size == current.preferred_size
                && values.max_size == current.max_size
            {
                self.report.quality_definitions.matched += 1;
                continue;
            }
            if self.apply {
                quality_definitions::update(&self.state.db, id, values)
                    .await
                    .map_err(failed)?;
            }
            self.report.quality_definitions.created += 1;
        }
        Ok(())
    }

    async fn indexers(
        &mut self,
        remote: &[arr_compat::radarr::dto::IndexerResource],
    ) -> Result<(), Problem> {
        let mut known: Vec<(i32, String)> = indexers::list(&self.state.db)
            .await
            .map_err(failed)?
            .into_iter()
            .map(|row| (row.model.id, row.model.name))
            .collect();
        let mut arrived = false;
        for item in remote {
            let url = field_text(&item.fields, "baseUrl");
            let (true, Some(url)) = (item.implementation.eq_ignore_ascii_case("torznab"), url)
            else {
                self.report.indexers.skipped += 1;
                continue;
            };
            let mapped = self.mapped(store::INDEXER, item.id).await?;
            if mapped.is_some_and(|id| known.iter().any(|(k, _)| *k == id)) {
                self.report.indexers.matched += 1;
                continue;
            }
            if let Some(id) = known
                .iter()
                .find(|(_, n)| *n == item.name)
                .map(|(id, _)| *id)
            {
                self.report.indexers.matched += 1;
                self.remember(store::INDEXER, item.id, id).await?;
                continue;
            }
            let id = if self.apply {
                indexers::create(
                    &self.state.db,
                    indexers::Values {
                        name: item.name.clone(),
                        kind: "torznab".to_owned(),
                        url,
                        api_key: String::new(),
                        categories: json!([]),
                        priority: item.priority,
                        use_proxy: false,
                        enabled: false,
                        tags: self.local_tags(&item.tags),
                    },
                )
                .await
                .map_err(failed)?
                .model
                .id
            } else {
                self.placeholder()
            };
            known.push((id, item.name.clone()));
            arrived = true;
            self.report.indexers.created += 1;
            self.remember(store::INDEXER, item.id, id).await?;
        }
        if arrived {
            self.report.notes.push(
                "Indexers arrive disabled without an API key: enter the keys, then enable them."
                    .to_owned(),
            );
        }
        Ok(())
    }

    async fn prowlarr_indexers(
        &mut self,
        remote: &[arr_compat::prowlarr::dto::IndexerResource],
        base_url: &str,
    ) -> Result<(), Problem> {
        let mut known: Vec<Known> = indexers::list(&self.state.db)
            .await
            .map_err(failed)?
            .into_iter()
            .map(|row| Known {
                id: row.model.id,
                name: row.model.name.clone(),
                signature: json!({"kind": row.model.kind, "url": row.model.url}),
            })
            .collect();
        let mut arrived = false;
        for item in remote {
            let Some(kind) = prowlarr_kind(&item.protocol) else {
                self.report.indexers.skipped += 1;
                continue;
            };
            let url = feed_url(base_url, item.id)
                .map_err(|error| prowlarr_failed(ProwlarrError::Url(error)))?;
            let signature = json!({"kind": kind, "url": url});
            let mapped = self.mapped(store::INDEXER, item.id).await?;
            if mapped.is_some_and(|id| known.iter().any(|k| k.id == id)) {
                self.report.indexers.matched += 1;
                continue;
            }
            let alt = self.renamed_name(&item.name);
            match pick(&known, &item.name, &alt, &signature) {
                Pick::Matched(id) => {
                    self.report.indexers.matched += 1;
                    self.remember(store::INDEXER, item.id, id).await?;
                }
                Pick::Skip => {
                    self.report.indexers.skipped += 1;
                    self.report.notes.push(format!(
                        "Indexer \"{}\" differs from the stored one and \"{alt}\" is taken too; skipped.",
                        item.name
                    ));
                }
                Pick::Create { name, renamed } => {
                    if renamed {
                        self.report.notes.push(format!(
                            "Indexer \"{}\" differs from the stored one; imported as \"{name}\".",
                            item.name
                        ));
                    }
                    let id = if self.apply {
                        indexers::create(
                            &self.state.db,
                            indexers::Values {
                                name: name.clone(),
                                kind: kind.to_owned(),
                                url,
                                api_key: self.source.api_key.clone(),
                                categories: json!(item.categories()),
                                priority: item.priority.clamp(1, 50),
                                use_proxy: false,
                                enabled: true,
                                tags: Vec::new(),
                            },
                        )
                        .await
                        .map_err(failed)?
                        .model
                        .id
                    } else {
                        self.placeholder()
                    };
                    known.push(Known {
                        id,
                        name,
                        signature,
                    });
                    arrived = true;
                    self.report.indexers.created += 1;
                    self.remember(store::INDEXER, item.id, id).await?;
                }
            }
        }
        if arrived {
            self.report.notes.push(
                "Indexers point at Prowlarr's per-indexer feed and use its API key.".to_owned(),
            );
        }
        Ok(())
    }

    async fn download_clients(
        &mut self,
        remote: &[arr_compat::radarr::dto::DownloadClientResource],
    ) -> Result<(), Problem> {
        let mut known: Vec<(i32, String)> = download_clients::list(&self.state.db)
            .await
            .map_err(failed)?
            .into_iter()
            .map(|row| (row.model.id, row.model.name))
            .collect();
        let mut arrived = false;
        for item in remote {
            let host = field_text(&item.fields, "host");
            let (true, Some(host)) = (
                item.implementation.eq_ignore_ascii_case("qbittorrent"),
                host,
            ) else {
                self.report.download_clients.skipped += 1;
                continue;
            };
            let mapped = self.mapped(store::DOWNLOAD_CLIENT, item.id).await?;
            if mapped.is_some_and(|id| known.iter().any(|(k, _)| *k == id)) {
                self.report.download_clients.matched += 1;
                continue;
            }
            if let Some(id) = known
                .iter()
                .find(|(_, n)| *n == item.name)
                .map(|(id, _)| *id)
            {
                self.report.download_clients.matched += 1;
                self.remember(store::DOWNLOAD_CLIENT, item.id, id).await?;
                continue;
            }
            let scheme = if matches!(
                item.fields
                    .iter()
                    .find(|field| field.name == "useSsl")
                    .and_then(|field| field.value.as_ref()),
                Some(Value::Bool(true))
            ) {
                "https"
            } else {
                "http"
            };
            let port = field_text(&item.fields, "port")
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            let base = field_text(&item.fields, "urlBase")
                .map(|base| format!("/{}", base.trim_matches('/')))
                .filter(|base| base != "/")
                .unwrap_or_default();
            let url = format!("{scheme}://{host}{port}{base}");
            let id = if self.apply {
                download_clients::create(
                    &self.state.db,
                    download_clients::Values {
                        name: item.name.clone(),
                        kind: "qbittorrent".to_owned(),
                        url,
                        username: field_text(&item.fields, "username"),
                        password: None,
                        api_key: None,
                        category: field_text(&item.fields, "movieCategory").unwrap_or_default(),
                        remote_path: None,
                        local_path: None,
                        enabled: false,
                        tags: self.local_tags(&item.tags),
                    },
                )
                .await
                .map_err(failed)?
                .model
                .id
            } else {
                self.placeholder()
            };
            known.push((id, item.name.clone()));
            arrived = true;
            self.report.download_clients.created += 1;
            self.remember(store::DOWNLOAD_CLIENT, item.id, id).await?;
        }
        if arrived {
            self.report.notes.push(
                "Download clients arrive disabled without a password: enter it, then enable them."
                    .to_owned(),
            );
        }
        Ok(())
    }
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// The file of a source movie as Unseenarr stores it, relative to the movie
/// folder as the source reports it.
fn new_file(file: &MovieFileResource) -> NewFile {
    NewFile {
        relative_path: non_empty(&file.relative_path).unwrap_or_else(|| last_segment(&file.path)),
        size: file.size,
        quality_id: map::movie_quality_id(file),
        languages: file
            .languages
            .iter()
            .filter_map(map::language_code)
            .collect(),
    }
}

fn metadata(movie: &MovieResource) -> Metadata {
    Metadata {
        tmdb_id: movie.tmdb_id,
        imdb_id: non_empty(&movie.imdb_id),
        title: movie.title.clone(),
        original_title: non_empty(&movie.original_title),
        sort_title: unseenarr_core::naming::sort_title(&movie.title),
        year: movie.year,
        overview: non_empty(&movie.overview),
        runtime: (movie.runtime > 0).then_some(movie.runtime),
        status: status_wire(movie.status).to_owned(),
        in_cinemas: date(movie.in_cinemas.as_ref()),
        digital_release: date(movie.digital_release.as_ref()),
        physical_release: date(movie.physical_release.as_ref()),
        poster_path: None,
    }
}

/// The resource groups a source can be told to import, in report order.
const GROUPS: [&str; 8] = [
    "rootFolders",
    "qualityProfiles",
    "customFormats",
    "tags",
    "collections",
    "movies",
    "indexers",
    "downloadClients",
];

/// The only group a Prowlarr source has.
const PROWLARR_GROUPS: [&str; 1] = ["indexers"];

/// The groups a source asked for, or `None` for all of them.
fn groups_of(source: &Source) -> Option<HashSet<String>> {
    let groups: HashSet<String> = serde_json::from_str(source.resources.as_deref()?).ok()?;
    (!groups.is_empty()).then_some(groups)
}

/// Reads the source and merges it into the library. With `apply == false`
/// nothing is written and the source is not marked as imported.
pub async fn import(state: &AppState, source: &Source, apply: bool) -> Result<Report, Problem> {
    if source.kind == "prowlarr" {
        return import_prowlarr(state, source, apply).await;
    }
    import_radarr(state, source, apply).await
}

async fn import_prowlarr(
    state: &AppState,
    source: &Source,
    apply: bool,
) -> Result<Report, Problem> {
    let key = db::secret::decrypt(&state.secrets, &source.api_key).map_err(failed)?;
    let client = ProwlarrClient::new(state.http.clone(), &source.url, key.expose());
    let indexers = client.indexers().await.map_err(prowlarr_failed)?;
    let mut run = Run {
        state,
        source,
        apply,
        report: Report::default(),
        synthetic: 0,
        groups: None,
        roots: Vec::new(),
        tag_ids: HashMap::new(),
        format_ids: HashMap::new(),
        profile_ids: HashMap::new(),
        collection_ids: HashMap::new(),
    };
    run.prowlarr_indexers(&indexers, &source.url).await?;
    if apply {
        store::mark_imported(&state.db, source.id)
            .await
            .map_err(failed)?;
    }
    Ok(run.report)
}

async fn import_radarr(state: &AppState, source: &Source, apply: bool) -> Result<Report, Problem> {
    let key = db::secret::decrypt(&state.secrets, &source.api_key).map_err(failed)?;
    let client = RadarrClient::new(state.http.clone(), &source.url, key.expose());
    let mut run = Run {
        state,
        source,
        apply,
        report: Report::default(),
        synthetic: 0,
        groups: groups_of(source),
        roots: Vec::new(),
        tag_ids: HashMap::new(),
        format_ids: HashMap::new(),
        profile_ids: HashMap::new(),
        collection_ids: HashMap::new(),
    };
    if run.wants("rootFolders") {
        let remote = client.root_folders().await.map_err(radarr_failed)?;
        run.root_folders(&remote).await?;
    }
    if run.wants("tags") {
        let remote = client.tags().await.map_err(radarr_failed)?;
        run.tags(&remote).await?;
    }
    if run.wants("customFormats") {
        let remote = client.custom_formats().await.map_err(radarr_failed)?;
        run.custom_formats(&remote).await?;
    }
    if run.wants("qualityProfiles") {
        let remote = client.quality_profiles().await.map_err(radarr_failed)?;
        run.quality_profiles(&remote).await?;
    }
    if run.wants("collections") {
        let remote = client.collections().await.map_err(radarr_failed)?;
        run.collections(&remote).await?;
    }
    if run.wants("movies") {
        let remote = client.movies().await.map_err(radarr_failed)?;
        run.movies(&remote).await?;
    }
    if run.wants("indexers") {
        let remote = client.indexers().await.map_err(radarr_failed)?;
        run.indexers(&remote).await?;
    }
    if run.wants("downloadClients") {
        let remote = client.download_clients().await.map_err(radarr_failed)?;
        run.download_clients(&remote).await?;
    }
    match client.quality_definitions().await {
        Ok(list) if source.import_quality_definitions && !list.is_empty() => {
            run.quality_definitions(&list).await?;
        }
        Ok(list) if !list.is_empty() => {
            run.report.quality_definitions.skipped = count(list.len());
            run.report
                .notes
                .push("Quality definitions are global and were not changed.".to_owned());
        }
        Ok(_) => {}
        Err(_) => run
            .report
            .notes
            .push("The source did not list quality definitions; nothing to skip.".to_owned()),
    }
    if apply {
        store::mark_imported(&state.db, source.id)
            .await
            .map_err(failed)?;
    }
    Ok(run.report)
}

/// A configured import source. The API key is never returned.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportSource {
    id: i32,
    kind: String,
    name: String,
    url: String,
    target_quality_profile_id: Option<i32>,
    remote_path: Option<String>,
    local_path: Option<String>,
    /// The order the source is listed and run in; lower comes first.
    position: i32,
    /// The groups it imports. Every group when it imports all of them.
    resources: Vec<String>,
    /// The source's global size limits are applied by a run.
    import_quality_definitions: bool,
    last_import_at: Option<String>,
    created_at: String,
}

impl From<Source> for ImportSource {
    fn from(row: Source) -> Self {
        let prowlarr = row.kind == "prowlarr";
        Self {
            id: row.id,
            kind: row.kind,
            name: row.name,
            url: row.url,
            target_quality_profile_id: row.target_quality_profile_id,
            remote_path: row.remote_path,
            local_path: row.local_path,
            position: row.position,
            resources: row
                .resources
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_else(|| {
                    let all: &[&str] = if prowlarr { &PROWLARR_GROUPS } else { &GROUPS };
                    all.iter().map(|group| (*group).to_owned()).collect()
                }),
            import_quality_definitions: row.import_quality_definitions,
            last_import_at: row.last_import_at,
            created_at: row.created_at,
        }
    }
}

/// What a caller sends to add an import source. No `Debug`: it holds a key.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewSource {
    /// The kind of source: `radarr` or `prowlarr`.
    kind: String,
    name: String,
    /// Base URL of the source, such as `http://radarr:7878`.
    url: String,
    /// The source's API key. It is stored encrypted and never returned.
    #[schema(format = Password)]
    api_key: String,
    /// Quality profile given to every movie imported from this source. When
    /// set, the source's own quality profiles are not imported.
    target_quality_profile_id: Option<i32>,
    /// A path prefix as the source sees it. Needs `localPath`.
    remote_path: Option<String>,
    /// The same prefix on this machine. Needs `remotePath`.
    local_path: Option<String>,
    /// The resource groups to import. Empty means all of them.
    #[serde(default)]
    resources: Vec<String>,
    /// Apply the source's quality definitions to the global size limits.
    #[serde(default)]
    import_quality_definitions: bool,
}

/// Checks a request before it reaches the database.
fn validate(body: &NewSource) -> Result<(), Problem> {
    if body.kind != "radarr" && body.kind != "prowlarr" {
        return Err(Problem::validation(
            "kind",
            "must be `radarr` or `prowlarr`",
        ));
    }
    if body.kind == "prowlarr" {
        if body.target_quality_profile_id.is_some() {
            return Err(Problem::validation(
                "targetQualityProfileId",
                "is only used by a Radarr source",
            ));
        }
        if body.remote_path.is_some() {
            return Err(Problem::validation(
                "remotePath",
                "is only used by a Radarr source",
            ));
        }
        if body.local_path.is_some() {
            return Err(Problem::validation(
                "remotePath",
                "is only used by a Radarr source",
            ));
        }
        if body.import_quality_definitions {
            return Err(Problem::validation(
                "importQualityDefinitions",
                "is only used by a Radarr source",
            ));
        }
        if body
            .resources
            .iter()
            .any(|group| !PROWLARR_GROUPS.contains(&group.as_str()))
        {
            return Err(Problem::validation(
                "resources",
                "a Prowlarr source only imports `indexers`",
            ));
        }
    }
    if body.name.trim().is_empty() {
        return Err(Problem::validation("name", "must not be blank"));
    }
    if body.url.trim().is_empty() {
        return Err(Problem::validation("url", "must not be blank"));
    }
    if body.remote_path.is_some() != body.local_path.is_some() {
        return Err(Problem::validation(
            "remotePath",
            "and localPath must be given together",
        ));
    }
    if let Some(unknown) = body
        .resources
        .iter()
        .find(|group| !GROUPS.contains(&group.as_str()))
    {
        return Err(Problem::validation(
            "resources",
            &format!("`{unknown}` is not an import resource"),
        ));
    }
    if body.resources.iter().any(|group| group == "movies") {
        if !body.resources.iter().any(|group| group == "rootFolders") {
            return Err(Problem::validation(
                "resources",
                "movies require root folders",
            ));
        }
        if body.target_quality_profile_id.is_none()
            && !body
                .resources
                .iter()
                .any(|group| group == "qualityProfiles")
        {
            return Err(Problem::validation(
                "resources",
                "movies require quality profiles or a target quality profile",
            ));
        }
    }
    Ok(())
}

fn source_error(error: store::SourceError) -> Problem {
    match error {
        store::SourceError::NotFound => Problem::not_found(),
        store::SourceError::UrlTaken => Problem::conflict(
            "import_source_url_taken",
            "Another import source has this URL",
        ),
        store::SourceError::UnknownProfile => Problem::validation(
            "targetQualityProfileId",
            "must be the id of a quality profile",
        ),
        store::SourceError::Db(error) => failed(error),
    }
}

async fn load(state: &AppState, id: i32) -> Result<Source, Problem> {
    store::find(&state.db, id)
        .await
        .map_err(failed)?
        .ok_or_else(Problem::not_found)
}

/// Lists the import sources in the order they run in.
#[utoipa::path(
    get,
    path = "/api/v1/import-sources",
    tag = "import-sources",
    responses(
        (status = 200, description = "The import sources", body = Vec<ImportSource>),
        (status = 403, description = "Permission denied", body = Problem),
    )
)]
pub async fn list_sources(
    _caller: Require<ImportRun>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ImportSource>>, Problem> {
    let rows = store::list(&state.db).await.map_err(failed)?;
    Ok(Json(rows.into_iter().map(ImportSource::from).collect()))
}

/// Adds an import source. The key is stored encrypted.
#[utoipa::path(
    post,
    path = "/api/v1/import-sources",
    tag = "import-sources",
    request_body = NewSource,
    responses(
        (status = 201, description = "The new import source", body = ImportSource),
        (status = 403, description = "Permission denied", body = Problem),
        (status = 409, description = "Another source has this URL", body = Problem),
        (status = 422, description = "The request is not valid", body = Problem),
    )
)]
pub async fn create_source(
    _caller: Require<ImportRun>,
    State(state): State<AppState>,
    Json(body): Json<NewSource>,
) -> Result<(StatusCode, Json<ImportSource>), Problem> {
    validate(&body)?;
    let values = store::Values {
        kind: body.kind,
        name: body.name.trim().to_owned(),
        url: body.url.trim().to_owned(),
        api_key: db::secret::encrypt(&state.secrets, &db::secret::Secret::new(body.api_key)),
        target_quality_profile_id: body.target_quality_profile_id,
        remote_path: body.remote_path,
        local_path: body.local_path,
        resources: (!body.resources.is_empty())
            .then(|| serde_json::to_string(&body.resources).expect("a list of strings")),
        import_quality_definitions: body.import_quality_definitions,
    };
    let row = store::create(&state.db, values)
        .await
        .map_err(source_error)?;
    Ok((StatusCode::CREATED, Json(row.into())))
}

/// What a source's URL and key reach. Nothing is stored.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    /// `Radarr` for a Radarr, `Sonarr` for a Sonarr.
    pub app_name: String,
    /// The instance name set in the source's settings.
    pub instance_name: String,
    pub version: String,
}

/// A source to try before it is saved. No `Debug`: it holds a key.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestSource {
    pub url: String,
    /// The source's API key. It is not stored.
    #[schema(format = Password)]
    pub api_key: String,
}

/// Tries a URL and key, so the page can say what they reach before saving.
#[utoipa::path(
    post,
    path = "/api/v1/import-sources/test",
    tag = "import-sources",
    request_body = TestSource,
    responses(
        (status = 200, description = "What the URL and key reach", body = Connection),
        (status = 403, description = "Permission denied", body = Problem),
        (status = 422, description = "The request is not valid", body = Problem),
        (status = 502, description = "The source did not answer", body = Problem),
    )
)]
pub async fn test_source(
    _caller: Require<ImportRun>,
    State(state): State<AppState>,
    Json(body): Json<TestSource>,
) -> Result<Json<Connection>, Problem> {
    if body.url.trim().is_empty() {
        return Err(Problem::validation("url", "must not be blank"));
    }
    if body.api_key.trim().is_empty() {
        return Err(Problem::validation("apiKey", "must not be blank"));
    }
    let status = RadarrClient::new(state.http.clone(), body.url.trim(), body.api_key.trim())
        .system_status()
        .await
        .map_err(radarr_failed)?;
    Ok(Json(Connection {
        app_name: status.app_name,
        instance_name: status.instance_name,
        version: status.version,
    }))
}

/// The order the sources are listed and run in, first to last.
#[derive(Deserialize, ToSchema)]
pub struct SourceOrder {
    /// Every source id, once.
    pub ids: Vec<i32>,
}

/// Sets the order the sources are listed in. The first one's profile is the
/// primary profile of a movie both sources have.
#[utoipa::path(
    patch,
    path = "/api/v1/import-sources/order",
    tag = "import-sources",
    request_body = SourceOrder,
    responses(
        (status = 204, description = "Reordered"),
        (status = 403, description = "Permission denied", body = Problem),
        (status = 422, description = "The request is not valid", body = Problem),
    )
)]
pub async fn order_sources(
    _caller: Require<ImportRun>,
    State(state): State<AppState>,
    Json(body): Json<SourceOrder>,
) -> Result<StatusCode, Problem> {
    let known = store::list(&state.db).await.map_err(failed)?;
    let mut wanted = body.ids.clone();
    wanted.sort_unstable();
    wanted.dedup();
    let mut existing: Vec<i32> = known.iter().map(|row| row.id).collect();
    existing.sort_unstable();
    if wanted != existing {
        return Err(Problem::validation(
            "ids",
            "must list every import source once",
        ));
    }
    store::reorder(&state.db, &body.ids)
        .await
        .map_err(source_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes an import source and its resource map. Imported data stays.
#[utoipa::path(
    delete,
    path = "/api/v1/import-sources/{id}",
    tag = "import-sources",
    params(("id" = i32, Path, description = "Import source id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Permission denied", body = Problem),
        (status = 404, description = "No such import source", body = Problem),
    )
)]
pub async fn delete_source(
    _caller: Require<ImportRun>,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<i32>,
) -> Result<StatusCode, Problem> {
    store::delete(&state.db, id).await.map_err(source_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Counts what a run would do, without writing anything.
#[utoipa::path(
    post,
    path = "/api/v1/import-sources/{id}/preview",
    tag = "import-sources",
    params(("id" = i32, Path, description = "Import source id")),
    responses(
        (status = 200, description = "What a run would do", body = Report),
        (status = 403, description = "Permission denied", body = Problem),
        (status = 404, description = "No such import source", body = Problem),
        (status = 502, description = "The source did not answer", body = Problem),
    )
)]
pub async fn preview_source(
    _caller: Require<ImportRun>,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<i32>,
) -> Result<Json<Report>, Problem> {
    let source = load(&state, id).await?;
    Ok(Json(import(&state, &source, false).await?))
}

/// Imports from the source and records the time of the run.
#[utoipa::path(
    post,
    path = "/api/v1/import-sources/{id}/run",
    tag = "import-sources",
    params(("id" = i32, Path, description = "Import source id")),
    responses(
        (status = 200, description = "What the run did", body = Report),
        (status = 403, description = "Permission denied", body = Problem),
        (status = 404, description = "No such import source", body = Problem),
        (status = 409, description = "An import of this source is already running", body = Problem),
        (status = 502, description = "The source did not answer", body = Problem),
    )
)]
pub async fn run_source(
    _caller: Require<ImportRun>,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<i32>,
) -> Result<Json<Report>, Problem> {
    let source = load(&state, id).await?;
    let _running = RunGuard::acquire(&state.import_runs, id).ok_or_else(|| {
        Problem::conflict(
            "import_running",
            "An import of this source is already running",
        )
    })?;
    Ok(Json(import(&state, &source, true).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> NewSource {
        NewSource {
            kind: "radarr".to_owned(),
            name: "Radarr".to_owned(),
            url: "http://radarr:7878".to_owned(),
            api_key: "key".to_owned(),
            target_quality_profile_id: None,
            remote_path: None,
            local_path: None,
            resources: Vec::new(),
            import_quality_definitions: false,
        }
    }

    fn field(body: &NewSource) -> Option<String> {
        validate(body)
            .err()
            .and_then(|problem| problem.errors.first().map(|e| e.field.clone()))
    }

    #[test]
    fn a_source_runs_once_at_a_time() {
        let id = 987_654;
        let set = RunSet::default();
        let guard = RunGuard::acquire(&set, id).expect("first acquire");
        assert!(RunGuard::acquire(&set, id).is_none());
        drop(guard);
        assert!(RunGuard::acquire(&set, id).is_some());
    }

    #[test]
    fn profiles_differ_by_scores_and_limits() {
        let items = json!([]);
        let rules = |scores: Vec<(i32, i32)>, min: i32| {
            ProfileRules {
                items: &items,
                cutoff: 1,
                language: "any",
                upgrade_allowed: true,
                min_format_score: min,
                cutoff_format_score: 0,
                min_upgrade_format_score: 1,
                scores,
            }
            .signature()
        };
        assert_eq!(
            rules(vec![(2, 5), (1, 3), (4, 0)], 0),
            rules(vec![(1, 3), (2, 5)], 0)
        );
        assert_ne!(rules(vec![(1, 3)], 0), rules(vec![(1, 4)], 0));
        assert_ne!(rules(vec![], 0), rules(vec![], 1));
    }

    #[test]
    fn valid_request_passes() {
        assert!(validate(&body()).is_ok());
        let mut paired = body();
        paired.remote_path = Some("/movies".to_owned());
        paired.local_path = Some("/data".to_owned());
        assert!(validate(&paired).is_ok());
    }

    #[test]
    fn rejects_unknown_kind_and_blank_fields() {
        let mut b = body();
        b.kind = "sonarr".to_owned();
        assert_eq!(field(&b).as_deref(), Some("kind"));
        let mut b = body();
        b.name = "  ".to_owned();
        assert_eq!(field(&b).as_deref(), Some("name"));
        let mut b = body();
        b.url = String::new();
        assert_eq!(field(&b).as_deref(), Some("url"));
    }

    #[test]
    fn prowlarr_protocols_map_to_indexer_kinds() {
        assert_eq!(prowlarr_kind("torrent"), Some("torznab"));
        assert_eq!(prowlarr_kind("Torrent"), Some("torznab"));
        assert_eq!(prowlarr_kind("usenet"), Some("newznab"));
        assert_eq!(prowlarr_kind("other"), None);
    }

    fn prowlarr_body() -> NewSource {
        NewSource {
            kind: "prowlarr".to_owned(),
            name: "Prowlarr".to_owned(),
            url: "http://prowlarr:9696".to_owned(),
            api_key: "key".to_owned(),
            target_quality_profile_id: None,
            remote_path: None,
            local_path: None,
            resources: Vec::new(),
            import_quality_definitions: false,
        }
    }

    #[test]
    fn a_prowlarr_source_takes_no_radarr_fields() {
        assert!(validate(&prowlarr_body()).is_ok());
        let mut b = prowlarr_body();
        b.target_quality_profile_id = Some(1);
        assert_eq!(field(&b).as_deref(), Some("targetQualityProfileId"));
        let mut b = prowlarr_body();
        b.remote_path = Some("/movies".to_owned());
        b.local_path = Some("/data".to_owned());
        assert_eq!(field(&b).as_deref(), Some("remotePath"));
        let mut b = prowlarr_body();
        b.remote_path = Some("/movies".to_owned());
        assert_eq!(field(&b).as_deref(), Some("remotePath"));
    }

    #[test]
    fn a_prowlarr_source_imports_indexers_only() {
        let mut b = prowlarr_body();
        b.resources = vec!["indexers".to_owned()];
        assert!(validate(&b).is_ok());
        b.resources = vec!["indexers".to_owned(), "tags".to_owned()];
        assert_eq!(field(&b).as_deref(), Some("resources"));
        let mut b = prowlarr_body();
        b.import_quality_definitions = true;
        assert_eq!(field(&b).as_deref(), Some("importQualityDefinitions"));
    }

    #[test]
    fn rejects_an_unknown_resource_group() {
        let mut b = body();
        b.resources = vec!["movies".to_owned(), "subtitles".to_owned()];
        assert_eq!(field(&b).as_deref(), Some("resources"));
        let mut b = body();
        b.resources = vec!["rootFolders".to_owned(), "qualityProfiles".to_owned()];
        assert!(validate(&b).is_ok());
    }

    #[test]
    fn movie_imports_require_their_selected_dependencies() {
        let mut b = body();
        b.resources = vec!["movies".to_owned()];
        assert_eq!(field(&b).as_deref(), Some("resources"));
        b.resources = vec!["movies".to_owned(), "rootFolders".to_owned()];
        assert_eq!(field(&b).as_deref(), Some("resources"));
        b.target_quality_profile_id = Some(1);
        assert!(validate(&b).is_ok());
        b.resources.push("qualityProfiles".to_owned());
        b.target_quality_profile_id = None;
        assert!(validate(&b).is_ok());
    }

    #[test]
    fn rejects_a_lone_path() {
        let mut b = body();
        b.remote_path = Some("/movies".to_owned());
        assert_eq!(field(&b).as_deref(), Some("remotePath"));
        let mut b = body();
        b.local_path = Some("/data".to_owned());
        assert_eq!(field(&b).as_deref(), Some("remotePath"));
    }

    fn mapped(path: &str) -> String {
        local_path(Some("/movies"), Some("/data/films"), path)
    }

    #[test]
    fn local_path_without_mapping_is_unchanged() {
        assert_eq!(local_path(None, None, "/movies/a"), "/movies/a");
        assert_eq!(local_path(Some("/movies"), None, "/movies/a"), "/movies/a");
        assert_eq!(local_path(None, Some("/data"), "/movies/a"), "/movies/a");
    }

    #[test]
    fn local_path_replaces_the_prefix() {
        assert_eq!(mapped("/movies"), "/data/films");
        assert_eq!(mapped("/movies/Dune (2021)"), "/data/films/Dune (2021)");
        assert_eq!(mapped("/movies/a/b/c.mkv"), "/data/films/a/b/c.mkv");
    }

    #[test]
    fn local_path_ignores_a_near_miss() {
        assert_eq!(mapped("/movies2/a"), "/movies2/a");
        assert_eq!(mapped("/other/movies/a"), "/other/movies/a");
    }

    #[test]
    fn local_path_trims_trailing_separators() {
        assert_eq!(
            local_path(Some("/movies/"), Some("/data/films/"), "/movies/a"),
            "/data/films/a"
        );
        assert_eq!(
            local_path(Some("/movies\\"), Some("/data/"), "/movies"),
            "/data"
        );
    }

    #[test]
    fn local_path_handles_backslashes() {
        assert_eq!(
            local_path(Some("D:\\Movies"), Some("/data"), "D:\\Movies\\Dune\\a.mkv"),
            "/data/Dune/a.mkv"
        );
        assert_eq!(
            local_path(Some("/movies"), Some("E:\\Films"), "/movies/Dune"),
            "E:\\Films/Dune"
        );
    }

    #[test]
    fn earlier_availability_picks_the_earlier_one() {
        assert_eq!(earlier_availability("released", "announced"), "announced");
        assert_eq!(earlier_availability("announced", "released"), "announced");
        assert_eq!(earlier_availability("inCinemas", "inCinemas"), "inCinemas");
        assert_eq!(earlier_availability("released", "inCinemas"), "inCinemas");
        assert_eq!(earlier_availability("released", "odd"), "released");
    }

    #[test]
    fn tag_union_is_sorted_and_unique() {
        assert_eq!(tag_union(&[3, 1], &[2, 3]), vec![1, 2, 3]);
        assert_eq!(tag_union(&[], &[]), Vec::<i32>::new());
    }

    #[test]
    fn relative_to_requires_the_folder() {
        assert_eq!(
            relative_to("/data/Dune (2021)/", "/data/Dune (2021)/a.mkv"),
            Some("a.mkv".to_owned())
        );
        assert_eq!(relative_to("/data/Dune", "/data/Dune2/a.mkv"), None);
        assert_eq!(relative_to("/data/Dune", "/data/Dune"), None);
    }

    #[test]
    fn status_and_dates_are_normalised() {
        assert_eq!(status_wire(MovieStatusType::Tba), "announced");
        assert_eq!(status_wire(MovieStatusType::Released), "released");
        assert_eq!(date(Some(&Timestamp::default())), None);
        assert_eq!(
            date(Some(&Timestamp("2021-09-15T00:00:00Z".into()))),
            Some("2021-09-15T00:00:00Z".to_owned())
        );
        assert_eq!(last_segment("/movies/Dune (2021)/"), "Dune (2021)");
    }

    #[test]
    fn pick_matches_renames_or_skips() {
        let known = |name: &str, sig: Value| Known {
            id: 1,
            name: name.to_owned(),
            signature: sig,
        };
        let same = json!([1]);
        let other = json!([2]);
        assert!(matches!(
            pick(&[], "a", "a (s)", &same),
            Pick::Create { renamed: false, .. }
        ));
        assert!(matches!(
            pick(&[known("a", same.clone())], "a", "a (s)", &same),
            Pick::Matched(1)
        ));
        assert!(matches!(
            pick(&[known("a", other.clone())], "a", "a (s)", &same),
            Pick::Create { renamed: true, .. }
        ));
        assert!(matches!(
            pick(
                &[known("a", other.clone()), known("a (s)", other)],
                "a",
                "a (s)",
                &same
            ),
            Pick::Skip
        ));
    }
}
