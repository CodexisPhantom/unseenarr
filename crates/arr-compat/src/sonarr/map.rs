//! Mapping between Unseenarr's data and the Sonarr resources (docs/14, "Sonarr
//! endpoints (M8)"). `arr-compat` never touches the database, so the `*View`
//! structs hold what the API layer has read; they follow the `db` models.

use std::fmt;

use serde::Deserialize;
use unseenarr_core::profile::{self, Item};
use unseenarr_core::quality::Quality as CoreQuality;

use super::dto::*;

/// Where TMDB serves the images whose relative path Unseenarr stores.
const TMDB_IMAGES: &str = "https://image.tmdb.org/t/p/original";

/// Why a request body cannot become an Unseenarr operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    /// A field the operation needs is absent or empty.
    Missing(&'static str),
    /// A field has a value Unseenarr does not accept.
    Invalid(&'static str, String),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(field) => write!(f, "{field} is required"),
            Self::Invalid(field, why) => write!(f, "{field}: {why}"),
        }
    }
}

impl std::error::Error for MapError {}

// ---------------------------------------------------------------- qualities

/// The four qualities whose ids differ between Unseenarr (Radarr) and Sonarr
/// (`docs/07-release-scoring.md`, "Qualities for TV"): `(unseenarr_id, sonarr_id)`.
pub const QUALITY_ID_TRANSLATIONS: &[(i32, i32)] = &[
    (20, 13), // Bluray-480p
    (21, 22), // Bluray-576p
    (30, 20), // Remux-1080p / Bluray-1080p Remux
    (31, 21), // Remux-2160p / Bluray-2160p Remux
];

/// Every TV quality Sonarr knows (`Quality.cs` of the pinned release):
/// `(core, sonarr_id, sonarr_name, sonarr_source, sonarr_resolution)`.
const TV_QUALITIES: &[(CoreQuality, i32, &str, QualitySource, i32)] = &[
    (
        CoreQuality::Unknown,
        0,
        "Unknown",
        QualitySource::Unknown,
        0,
    ),
    (CoreQuality::Sdtv, 1, "SDTV", QualitySource::Television, 480),
    (CoreQuality::Dvd, 2, "DVD", QualitySource::Dvd, 480),
    (
        CoreQuality::WebDl1080p,
        3,
        "WEBDL-1080p",
        QualitySource::Web,
        1080,
    ),
    (
        CoreQuality::Hdtv720p,
        4,
        "HDTV-720p",
        QualitySource::Television,
        720,
    ),
    (
        CoreQuality::WebDl720p,
        5,
        "WEBDL-720p",
        QualitySource::Web,
        720,
    ),
    (
        CoreQuality::Bluray720p,
        6,
        "Bluray-720p",
        QualitySource::Bluray,
        720,
    ),
    (
        CoreQuality::Bluray1080p,
        7,
        "Bluray-1080p",
        QualitySource::Bluray,
        1080,
    ),
    (
        CoreQuality::WebDl480p,
        8,
        "WEBDL-480p",
        QualitySource::Web,
        480,
    ),
    (
        CoreQuality::Hdtv1080p,
        9,
        "HDTV-1080p",
        QualitySource::Television,
        1080,
    ),
    (
        CoreQuality::RawHd,
        10,
        "Raw-HD",
        QualitySource::TelevisionRaw,
        1080,
    ),
    (
        CoreQuality::WebRip480p,
        12,
        "WEBRip-480p",
        QualitySource::WebRip,
        480,
    ),
    (
        CoreQuality::Bluray480p,
        13,
        "Bluray-480p",
        QualitySource::Bluray,
        480,
    ),
    (
        CoreQuality::WebRip720p,
        14,
        "WEBRip-720p",
        QualitySource::WebRip,
        720,
    ),
    (
        CoreQuality::WebRip1080p,
        15,
        "WEBRip-1080p",
        QualitySource::WebRip,
        1080,
    ),
    (
        CoreQuality::Hdtv2160p,
        16,
        "HDTV-2160p",
        QualitySource::Television,
        2160,
    ),
    (
        CoreQuality::WebRip2160p,
        17,
        "WEBRip-2160p",
        QualitySource::WebRip,
        2160,
    ),
    (
        CoreQuality::WebDl2160p,
        18,
        "WEBDL-2160p",
        QualitySource::Web,
        2160,
    ),
    (
        CoreQuality::Bluray2160p,
        19,
        "Bluray-2160p",
        QualitySource::Bluray,
        2160,
    ),
    (
        CoreQuality::Remux1080p,
        20,
        "Bluray-1080p Remux",
        QualitySource::BlurayRaw,
        1080,
    ),
    (
        CoreQuality::Remux2160p,
        21,
        "Bluray-2160p Remux",
        QualitySource::BlurayRaw,
        2160,
    ),
    (
        CoreQuality::Bluray576p,
        22,
        "Bluray-576p",
        QualitySource::Bluray,
        576,
    ),
];

/// Translates an Unseenarr (Radarr) quality id to Sonarr's id. Returns `None`
/// for movie-only or unknown quality ids.
pub fn to_sonarr_quality_id(unseenarr_id: i32) -> Option<i32> {
    TV_QUALITIES
        .iter()
        .find(|(core, ..)| core.id() == unseenarr_id)
        .map(|(_, sonarr_id, ..)| *sonarr_id)
}

/// Translates a Sonarr quality id to Unseenarr's (Radarr's) id. Returns `None`
/// for ids Sonarr does not define.
pub fn from_sonarr_quality_id(sonarr_id: i32) -> Option<i32> {
    TV_QUALITIES
        .iter()
        .find(|(_, id, ..)| *id == sonarr_id)
        .map(|(core, ..)| core.id())
}

/// Maps a core quality to Sonarr's `Quality` resource, or `None` for a
/// movie-only quality.
pub fn quality_from_core(core: CoreQuality) -> Option<Quality> {
    TV_QUALITIES.iter().find(|(known, ..)| *known == core).map(
        |(_, id, name, source, resolution)| Quality {
            id: *id,
            name: (*name).to_owned(),
            source: *source,
            resolution: *resolution,
        },
    )
}

/// The Sonarr quality of an Unseenarr quality id; `Unknown` for a movie-only or
/// unknown id.
pub fn quality(unseenarr_id: i32) -> Quality {
    CoreQuality::from_id(unseenarr_id)
        .and_then(quality_from_core)
        .unwrap_or_else(|| Quality {
            id: 0,
            name: "Unknown".to_owned(),
            source: QualitySource::Unknown,
            resolution: 0,
        })
}

fn quality_model(unseenarr_id: i32) -> QualityModel {
    QualityModel {
        quality: quality(unseenarr_id),
        revision: Revision::default(),
    }
}

// ---------------------------------------------------------------- languages

/// Sonarr's language ids (`Language.cs` of the pinned release) by ISO 639-1
/// (or BCP 47 regional) code.
const LANGUAGES: &[(&str, i32, &str)] = &[
    ("en", 1, "English"),
    ("fr", 2, "French"),
    ("es", 3, "Spanish"),
    ("de", 4, "German"),
    ("it", 5, "Italian"),
    ("da", 6, "Danish"),
    ("nl", 7, "Dutch"),
    ("ja", 8, "Japanese"),
    ("is", 9, "Icelandic"),
    ("zh", 10, "Chinese"),
    ("ru", 11, "Russian"),
    ("pl", 12, "Polish"),
    ("vi", 13, "Vietnamese"),
    ("sv", 14, "Swedish"),
    ("no", 15, "Norwegian"),
    ("fi", 16, "Finnish"),
    ("tr", 17, "Turkish"),
    ("pt", 18, "Portuguese"),
    ("nl-be", 19, "Flemish"),
    ("el", 20, "Greek"),
    ("ko", 21, "Korean"),
    ("hu", 22, "Hungarian"),
    ("he", 23, "Hebrew"),
    ("lt", 24, "Lithuanian"),
    ("cs", 25, "Czech"),
    ("ar", 26, "Arabic"),
    ("hi", 27, "Hindi"),
    ("bg", 28, "Bulgarian"),
    ("ml", 29, "Malayalam"),
    ("uk", 30, "Ukrainian"),
    ("sk", 31, "Slovak"),
    ("th", 32, "Thai"),
    ("pt-br", 33, "Portuguese (Brazil)"),
    ("es-419", 34, "Spanish (Latino)"),
    ("ro", 35, "Romanian"),
    ("lv", 36, "Latvian"),
    ("fa", 37, "Persian"),
    ("ca", 38, "Catalan"),
    ("hr", 39, "Croatian"),
    ("sr", 40, "Serbian"),
    ("bs", 41, "Bosnian"),
    ("et", 42, "Estonian"),
    ("ta", 43, "Tamil"),
    ("id", 44, "Indonesian"),
    ("mk", 45, "Macedonian"),
    ("sl", 46, "Slovenian"),
    ("az", 47, "Azerbaijani"),
    ("uz", 48, "Uzbek"),
    ("ms", 49, "Malay"),
    ("ur", 50, "Urdu"),
    ("rm", 51, "Romansh"),
    ("ka", 52, "Georgian"),
    ("ga", 53, "Irish"),
];

/// The Sonarr language of an Unseenarr language code: `any`, `original` or an
/// ISO 639-1 / regional code. Anything else is `Unknown`.
pub fn language(code: &str) -> Language {
    let code = code.trim().to_ascii_lowercase();
    let (id, name) = match code.as_str() {
        profile::LANGUAGE_ANY => (-1, "Any"),
        profile::LANGUAGE_ORIGINAL => (-2, "Original"),
        _ => LANGUAGES
            .iter()
            .find(|(known, ..)| *known == code)
            .map_or((0, "Unknown"), |(_, id, name)| (*id, *name)),
    };
    Language {
        id,
        name: name.to_owned(),
    }
}

// ------------------------------------------------------------------ helpers

fn clean_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn join(root: &str, name: &str) -> String {
    let separator = if root.contains('\\') && !root.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{}{separator}{name}", root.trim_end_matches(['/', '\\']))
}

fn poster_url(path: Option<&str>) -> Option<String> {
    let path = path?.trim();
    if path.is_empty() {
        return None;
    }
    if path.starts_with("http://") || path.starts_with("https://") {
        Some(path.to_owned())
    } else {
        Some(format!("{TMDB_IMAGES}{path}"))
    }
}

fn percent_of_episodes(file_count: i32, episode_count: i32) -> f64 {
    if episode_count <= 0 {
        0.0
    } else {
        f64::from(file_count) / f64::from(episode_count) * 100.0
    }
}

fn status_of(text: &str) -> SeriesStatusType {
    SeriesStatusType::ALL
        .iter()
        .copied()
        .find(|status| status.wire() == text)
        .unwrap_or_default()
}

fn series_type_of(text: &str) -> SeriesTypes {
    SeriesTypes::ALL
        .iter()
        .copied()
        .find(|kind| kind.wire() == text)
        .unwrap_or_default()
}

fn monitor_new_items_of(text: &str) -> NewItemMonitorTypes {
    NewItemMonitorTypes::ALL
        .iter()
        .copied()
        .find(|kind| kind.wire() == text)
        .unwrap_or_default()
}

// ------------------------------------------------------------ series/season

/// Which episodes to monitor when adding a show (docs/03-domain-model.md).
/// Sonarr's other `MonitorTypes` values are rejected on input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddMonitor {
    All,
    Future,
    Missing,
    None,
}

impl AddMonitor {
    /// The canonical wire / API name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Future => "future",
            Self::Missing => "missing",
            Self::None => "none",
        }
    }

    /// `None` for a value Unseenarr does not accept.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "all" => Some(Self::All),
            "future" => Some(Self::Future),
            "missing" => Some(Self::Missing),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

impl From<AddMonitor> for MonitorTypes {
    fn from(value: AddMonitor) -> Self {
        match value {
            AddMonitor::All => Self::All,
            AddMonitor::Future => Self::Future,
            AddMonitor::Missing => Self::Missing,
            AddMonitor::None => Self::None,
        }
    }
}

impl TryFrom<MonitorTypes> for AddMonitor {
    type Error = MapError;

    fn try_from(value: MonitorTypes) -> Result<Self, MapError> {
        match value {
            MonitorTypes::All => Ok(Self::All),
            MonitorTypes::Future => Ok(Self::Future),
            MonitorTypes::Missing => Ok(Self::Missing),
            MonitorTypes::None => Ok(Self::None),
            other => Err(MapError::Invalid(
                "addOptions.monitor",
                format!("`{}` is not supported", other.wire()),
            )),
        }
    }
}

/// One season of a show, with its aggregated episode statistics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeasonView {
    pub season_number: i32,
    pub monitored: bool,
    pub title: Option<String>,
    pub next_airing: Option<String>,
    pub previous_airing: Option<String>,
    pub episode_file_count: i32,
    pub episode_count: i32,
    pub total_episode_count: i32,
    pub size_on_disk: i64,
    pub release_groups: Vec<String>,
}

/// A show as `shows` stores it, with its root folder path, aliases, tags,
/// seasons and primary profile.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesView {
    pub id: i32,
    pub tvdb_id: Option<i32>,
    pub tmdb_id: Option<i32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub sort_title: String,
    pub year: i32,
    pub overview: Option<String>,
    pub runtime: Option<i32>,
    /// `continuing`, `ended`, `upcoming` or `deleted`.
    pub status: String,
    /// `standard`, `daily` or `anime`.
    pub series_type: String,
    pub network: Option<String>,
    /// Either a full URL (TVDB) or a TMDB path such as `/abc.jpg`.
    pub poster_path: Option<String>,
    pub season_folder: bool,
    pub root_folder_path: String,
    pub folder_name: String,
    pub monitored: bool,
    /// `all` or `none`.
    pub monitor_new_items: String,
    /// A date (`2015-12-14`) or a date-time.
    pub first_aired: Option<String>,
    pub last_aired: Option<String>,
    pub next_airing: Option<String>,
    pub previous_airing: Option<String>,
    /// Language code, see [`language`].
    pub original_language: Option<String>,
    pub added_at: String,
    pub aliases: Vec<String>,
    pub tags: Vec<i32>,
    /// The primary show profile's quality profile.
    pub quality_profile_id: i32,
    pub seasons: Vec<SeasonView>,
}

fn series_path(show: &SeriesView) -> String {
    if show.root_folder_path.is_empty() || show.folder_name.is_empty() {
        String::new()
    } else {
        join(&show.root_folder_path, &show.folder_name)
    }
}

pub fn season_resource(season: &SeasonView) -> SeasonResource {
    SeasonResource {
        season_number: season.season_number,
        monitored: season.monitored,
        title: season.title.clone().unwrap_or_default(),
        statistics: SeasonStatisticsResource {
            next_airing: season.next_airing.as_deref().map(Timestamp::parse),
            previous_airing: season.previous_airing.as_deref().map(Timestamp::parse),
            episode_file_count: season.episode_file_count,
            episode_count: season.episode_count,
            total_episode_count: season.total_episode_count,
            size_on_disk: season.size_on_disk,
            release_groups: season.release_groups.clone(),
            percent_of_episodes: percent_of_episodes(
                season.episode_file_count,
                season.episode_count,
            ),
        },
        images: Vec::new(),
    }
}

fn series_statistics(seasons: &[SeasonView]) -> SeriesStatisticsResource {
    let mut season_count = 0;
    let mut episode_file_count = 0;
    let mut episode_count = 0;
    let mut total_episode_count = 0;
    let mut size_on_disk = 0;
    let mut release_groups = Vec::new();
    for season in seasons {
        size_on_disk += season.size_on_disk;
        if season.season_number > 0 {
            season_count += 1;
            episode_file_count += season.episode_file_count;
            episode_count += season.episode_count;
            total_episode_count += season.total_episode_count;
        }
        for group in &season.release_groups {
            if !release_groups.contains(group) {
                release_groups.push(group.clone());
            }
        }
    }
    SeriesStatisticsResource {
        season_count,
        episode_file_count,
        episode_count,
        total_episode_count,
        size_on_disk,
        release_groups,
        percent_of_episodes: percent_of_episodes(episode_file_count, episode_count),
    }
}

/// A show as Sonarr's `SeriesResource`.
pub fn series_resource(show: &SeriesView) -> SeriesResource {
    let status = status_of(&show.status);
    let poster = poster_url(show.poster_path.as_deref());
    let lang = show
        .original_language
        .as_deref()
        .map(language)
        .unwrap_or_default();
    let title_slug = show
        .tvdb_id
        .or(show.tmdb_id)
        .map(|id| id.to_string())
        .unwrap_or_else(|| clean_title(&show.title));
    let original_title = show.original_title.clone().unwrap_or_default();
    SeriesResource {
        id: show.id,
        title: show.title.clone(),
        alternate_titles: show
            .aliases
            .iter()
            .map(|title| AlternateTitleResource {
                title: title.clone(),
                ..AlternateTitleResource::default()
            })
            .collect(),
        sort_title: show.sort_title.clone(),
        status,
        ended: status == SeriesStatusType::Ended,
        overview: show.overview.clone().unwrap_or_default(),
        next_airing: show.next_airing.as_deref().map(Timestamp::parse),
        previous_airing: show.previous_airing.as_deref().map(Timestamp::parse),
        network: show.network.clone().unwrap_or_default(),
        images: poster
            .iter()
            .map(|url| MediaCover {
                cover_type: MediaCoverTypes::Poster,
                url: url.clone(),
                remote_url: url.clone(),
            })
            .collect(),
        language: lang.clone(),
        original_language: lang,
        remote_poster: poster.unwrap_or_default(),
        seasons: show.seasons.iter().map(season_resource).collect(),
        year: show.year,
        path: series_path(show),
        quality_profile_id: show.quality_profile_id,
        season_folder: show.season_folder,
        monitored: show.monitored,
        monitor_new_items: monitor_new_items_of(&show.monitor_new_items),
        runtime: show.runtime.unwrap_or_default(),
        tvdb_id: show.tvdb_id.unwrap_or(0),
        tmdb_id: show.tmdb_id.unwrap_or(0),
        first_aired: show.first_aired.as_deref().map(Timestamp::parse),
        last_aired: show.last_aired.as_deref().map(Timestamp::parse),
        series_type: series_type_of(&show.series_type),
        clean_title: clean_title(&show.title),
        clean_original_title: clean_title(&original_title),
        original_title,
        imdb_id: show.imdb_id.clone().unwrap_or_default(),
        title_slug,
        root_folder_path: show.root_folder_path.clone(),
        folder: show.folder_name.clone(),
        tags: show.tags.clone(),
        added: Timestamp::parse(&show.added_at),
        statistics: series_statistics(&show.seasons),
        language_profile_id: 1,
        ..SeriesResource::default()
    }
}

/// One season entry sent inside `POST /series` or `PUT /series`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeasonRequest {
    pub season_number: i32,
    pub monitored: bool,
}

/// The `addOptions` object sent inside `POST /series`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AddSeriesOptionsRequest {
    pub monitor: Option<MonitorTypes>,
    pub search_for_missing_episodes: bool,
    pub search_for_cutoff_unmet_episodes: bool,
}

/// The body of `POST /series` and `PUT /series`. A client sends the fields it
/// knows, so everything is optional and anything else is ignored.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeriesRequest {
    pub id: Option<i32>,
    pub tvdb_id: Option<i32>,
    pub tmdb_id: Option<i32>,
    pub quality_profile_id: Option<i32>,
    pub root_folder_path: Option<String>,
    pub monitored: Option<bool>,
    pub monitor_new_items: Option<NewItemMonitorTypes>,
    pub series_type: Option<SeriesTypes>,
    pub season_folder: Option<bool>,
    pub tags: Option<Vec<i32>>,
    pub seasons: Option<Vec<SeasonRequest>>,
    pub add_options: Option<AddSeriesOptionsRequest>,
}

/// What `POST /series` asks Unseenarr to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddSeries {
    pub tvdb_id: i32,
    pub tmdb_id: Option<i32>,
    pub quality_profile_id: i32,
    pub root_folder_path: String,
    pub monitored: bool,
    pub monitor_new_items: NewItemMonitorTypes,
    pub series_type: SeriesTypes,
    pub season_folder: bool,
    pub tags: Vec<i32>,
    pub seasons: Vec<SeasonRequest>,
    /// `None` when the client did not specify `addOptions.monitor` (or sent
    /// `unknown`); the API applies `seasons` or the default monitoring rule.
    pub monitor: Option<AddMonitor>,
    /// Needs the `search:run` permission (docs/14).
    pub search_for_missing_episodes: bool,
    pub search_for_cutoff_unmet_episodes: bool,
}

/// What `PUT /series` asks Unseenarr to change. `None` keeps the stored value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSeries {
    pub id: i32,
    pub quality_profile_id: Option<i32>,
    pub monitored: Option<bool>,
    pub monitor_new_items: Option<NewItemMonitorTypes>,
    pub series_type: Option<SeriesTypes>,
    pub season_folder: Option<bool>,
    pub tags: Option<Vec<i32>>,
    pub seasons: Option<Vec<SeasonRequest>>,
}

impl SeriesRequest {
    /// A new series: it needs a TVDB id, a quality profile and a root folder.
    /// Defaults match Unseenarr's schema (`monitored = true`,
    /// `monitorNewItems = all`, `seriesType = standard`, `seasonFolder = true`).
    pub fn into_add(self) -> Result<AddSeries, MapError> {
        let positive =
            |value: Option<i32>, field| value.filter(|id| *id > 0).ok_or(MapError::Missing(field));
        let add_options = self.add_options;
        let monitor = match add_options.as_ref().and_then(|opts| opts.monitor) {
            None | Some(MonitorTypes::Unknown) => None,
            Some(value) => Some(AddMonitor::try_from(value)?),
        };
        Ok(AddSeries {
            tvdb_id: positive(self.tvdb_id, "tvdbId")?,
            tmdb_id: self.tmdb_id.filter(|id| *id > 0),
            quality_profile_id: positive(self.quality_profile_id, "qualityProfileId")?,
            root_folder_path: self
                .root_folder_path
                .filter(|path| !path.trim().is_empty())
                .ok_or(MapError::Missing("rootFolderPath"))?,
            monitored: self.monitored.unwrap_or(true),
            monitor_new_items: self.monitor_new_items.unwrap_or(NewItemMonitorTypes::All),
            series_type: self.series_type.unwrap_or(SeriesTypes::Standard),
            season_folder: self.season_folder.unwrap_or(true),
            tags: self.tags.unwrap_or_default(),
            seasons: self.seasons.unwrap_or_default(),
            monitor,
            search_for_missing_episodes: add_options
                .as_ref()
                .is_some_and(|opts| opts.search_for_missing_episodes),
            search_for_cutoff_unmet_episodes: add_options
                .as_ref()
                .is_some_and(|opts| opts.search_for_cutoff_unmet_episodes),
        })
    }

    /// A change of an existing series: it needs the Unseenarr id.
    pub fn into_update(self) -> Result<UpdateSeries, MapError> {
        Ok(UpdateSeries {
            id: self
                .id
                .filter(|id| *id > 0)
                .ok_or(MapError::Missing("id"))?,
            quality_profile_id: self.quality_profile_id.filter(|id| *id > 0),
            monitored: self.monitored,
            monitor_new_items: self.monitor_new_items,
            series_type: self.series_type,
            season_folder: self.season_folder,
            tags: self.tags,
            seasons: self.seasons,
        })
    }
}

// ----------------------------------------------------------------- episodes

/// One episode file, as `episode_files` stores it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EpisodeFileView {
    pub id: i32,
    pub series_id: i32,
    pub season_number: i32,
    /// Relative to the show folder.
    pub relative_path: String,
    pub size: i64,
    /// Unseenarr (Radarr) quality id; translated to Sonarr's id on output.
    pub quality_id: i32,
    pub release_group: Option<String>,
    pub scene_name: Option<String>,
    /// Language codes, see [`language`].
    pub languages: Vec<String>,
    pub release_type: ReleaseType,
    /// `None` while the file is not probed.
    pub media_info: Option<MediaInfoResource>,
    pub quality_cutoff_not_met: bool,
    pub added_at: String,
}

/// One episode, as `episodes` stores it, with the managed file of the primary
/// show profile if any.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EpisodeView {
    pub id: i32,
    pub series_id: i32,
    pub tvdb_id: Option<i32>,
    pub season_number: i32,
    pub episode_number: i32,
    pub absolute_number: Option<i32>,
    pub title: String,
    pub overview: Option<String>,
    /// Date `YYYY-MM-DD` as the provider gives it.
    pub air_date: Option<String>,
    /// UTC date-time when known.
    pub air_date_utc: Option<String>,
    pub runtime: Option<i32>,
    pub monitored: bool,
    /// The file of the primary show profile.
    pub file: Option<EpisodeFileView>,
}

pub fn episode_file_resource(show_path: &str, file: &EpisodeFileView) -> EpisodeFileResource {
    let path = if show_path.is_empty() {
        file.relative_path.clone()
    } else {
        join(show_path, &file.relative_path)
    };
    EpisodeFileResource {
        id: file.id,
        series_id: file.series_id,
        season_number: file.season_number,
        relative_path: file.relative_path.clone(),
        path,
        size: file.size,
        date_added: Timestamp::parse(&file.added_at),
        scene_name: file.scene_name.clone().unwrap_or_default(),
        release_group: file.release_group.clone().unwrap_or_default(),
        languages: file.languages.iter().map(|code| language(code)).collect(),
        quality: quality_model(file.quality_id),
        release_type: file.release_type,
        media_info: file.media_info.clone().unwrap_or_default(),
        quality_cutoff_not_met: file.quality_cutoff_not_met,
        ..EpisodeFileResource::default()
    }
}

pub fn episode_resource(episode: &EpisodeView, show_path: &str) -> EpisodeResource {
    let file = episode.file.as_ref();
    EpisodeResource {
        id: episode.id,
        series_id: episode.series_id,
        tvdb_id: episode.tvdb_id.unwrap_or(0),
        episode_file_id: file.map_or(0, |file| file.id),
        season_number: episode.season_number,
        episode_number: episode.episode_number,
        title: episode.title.clone(),
        air_date: episode
            .air_date
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        air_date_utc: episode.air_date_utc.as_deref().map(Timestamp::parse),
        runtime: episode.runtime.unwrap_or(0),
        overview: episode.overview.clone().unwrap_or_default(),
        episode_file: file.map(|file| episode_file_resource(show_path, file)),
        has_file: file.is_some(),
        monitored: episode.monitored,
        absolute_episode_number: episode.absolute_number,
        ..EpisodeResource::default()
    }
}

/// The body of `PUT /episode/monitor`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EpisodeMonitorRequest {
    pub episode_ids: Vec<i32>,
    pub monitored: Option<bool>,
}

/// What `PUT /episode/monitor` asks Unseenarr to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeMonitor {
    pub episode_ids: Vec<i32>,
    pub monitored: bool,
}

impl EpisodeMonitorRequest {
    pub fn into_monitor(self) -> Result<EpisodeMonitor, MapError> {
        if self.episode_ids.is_empty() {
            return Err(MapError::Missing("episodeIds"));
        }
        if self.episode_ids.iter().any(|id| *id <= 0) {
            return Err(MapError::Invalid(
                "episodeIds",
                "ids must be positive".to_owned(),
            ));
        }
        let monitored = self.monitored.ok_or(MapError::Missing("monitored"))?;
        Ok(EpisodeMonitor {
            episode_ids: self.episode_ids,
            monitored,
        })
    }
}

// --------------------------------------------------------- quality profiles

/// A quality profile as `quality_profiles` stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileView {
    pub id: i32,
    pub name: String,
    pub upgrade_allowed: bool,
    /// Unseenarr quality id or group id (`>= 1000`); quality ids are translated
    /// to Sonarr's numbering on output.
    pub cutoff_quality_id: i32,
    pub items: Vec<Item>,
    pub min_format_score: i32,
    pub cutoff_format_score: i32,
    pub min_upgrade_format_score: i32,
}

fn quality_item(unseenarr_quality_id: i32, allowed: bool) -> QualityProfileQualityItemResource {
    QualityProfileQualityItemResource {
        quality: Some(quality(unseenarr_quality_id)),
        allowed,
        ..QualityProfileQualityItemResource::default()
    }
}

/// A profile as Sonarr's `QualityProfileResource`. Custom formats are not in
/// the compat API before M11, so `formatItems` is empty. Quality ids (and a
/// non-group `cutoff`) are translated to Sonarr's numbering.
pub fn quality_profile(view: &ProfileView) -> QualityProfileResource {
    let cutoff = if view.cutoff_quality_id >= profile::GROUP_ID_MIN {
        view.cutoff_quality_id
    } else {
        to_sonarr_quality_id(view.cutoff_quality_id).unwrap_or(0)
    };
    QualityProfileResource {
        id: view.id,
        name: view.name.clone(),
        upgrade_allowed: view.upgrade_allowed,
        cutoff,
        items: view
            .items
            .iter()
            .map(|item| match item {
                Item::Quality {
                    quality_id,
                    allowed,
                } => quality_item(*quality_id, *allowed),
                Item::Group {
                    id,
                    name,
                    quality_ids,
                    allowed,
                } => QualityProfileQualityItemResource {
                    id: *id,
                    name: name.clone(),
                    quality: None,
                    items: quality_ids
                        .iter()
                        .map(|quality_id| quality_item(*quality_id, *allowed))
                        .collect(),
                    allowed: *allowed,
                    ..QualityProfileQualityItemResource::default()
                },
            })
            .collect(),
        min_format_score: view.min_format_score,
        cutoff_format_score: view.cutoff_format_score,
        min_upgrade_format_score: view.min_upgrade_format_score,
        format_items: Vec::new(),
    }
}

// -------------------------------------------------------- language profiles

/// A Sonarr v3 language profile (stubbed as empty in M8, shaped for full
/// schema coverage).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LanguageProfileView {
    pub id: i32,
    pub name: String,
    pub upgrade_allowed: bool,
    /// Language code of the cutoff language, see [`language`].
    pub cutoff: String,
    /// `(language_code, allowed)` pairs.
    pub languages: Vec<(String, bool)>,
}

pub fn language_profile(view: &LanguageProfileView) -> LanguageProfileResource {
    LanguageProfileResource {
        id: view.id,
        name: view.name.clone(),
        upgrade_allowed: view.upgrade_allowed,
        cutoff: language(&view.cutoff),
        languages: view
            .languages
            .iter()
            .map(|(code, allowed)| LanguageProfileItemResource {
                id: 0,
                language: language(code),
                allowed: *allowed,
            })
            .collect(),
    }
}

// ------------------------------------------------------------ root folders

/// A root folder with what the API layer found on disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RootFolderView {
    pub id: i32,
    pub path: String,
    pub accessible: bool,
    pub free_space: Option<i64>,
    pub total_space: Option<i64>,
    /// Names of the folders without a show.
    pub unmapped: Vec<String>,
}

pub fn root_folder(view: &RootFolderView) -> RootFolderResource {
    RootFolderResource {
        id: view.id,
        path: view.path.clone(),
        accessible: view.accessible,
        free_space: view.free_space,
        total_space: view.total_space,
        unmapped_folders: view
            .unmapped
            .iter()
            .map(|name| UnmappedFolder {
                name: name.clone(),
                path: join(&view.path, name),
                relative_path: name.clone(),
            })
            .collect(),
    }
}

// -------------------------------------------------------------------- tags

pub fn tag(id: i32, label: &str) -> TagResource {
    TagResource {
        id,
        label: label.to_owned(),
    }
}

/// The label of a tag that a client sends.
pub fn tag_label(resource: &TagResource) -> Result<String, MapError> {
    let label = resource.label.trim();
    if label.is_empty() {
        Err(MapError::Missing("label"))
    } else {
        Ok(label.to_owned())
    }
}

// ------------------------------------------------------------------- queue

/// One download of a show / episode.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueueView {
    pub id: i32,
    pub series: Option<SeriesView>,
    pub episode: Option<EpisodeView>,
    pub season_number: Option<i32>,
    /// Unseenarr (Radarr) quality id; translated to Sonarr's id on output.
    pub quality_id: i32,
    /// Language codes, see [`language`].
    pub languages: Vec<String>,
    pub title: String,
    /// Bytes.
    pub size: f64,
    pub size_left: f64,
    pub time_left_seconds: Option<u64>,
    pub estimated_completion: Option<String>,
    pub added: Option<String>,
    pub status: QueueStatus,
    pub tracked_status: TrackedDownloadStatus,
    pub tracked_state: TrackedDownloadState,
    /// Title and messages.
    pub status_messages: Vec<(String, Vec<String>)>,
    pub error_message: Option<String>,
    pub download_id: Option<String>,
    pub protocol: DownloadProtocol,
    pub download_client: Option<String>,
    pub indexer: Option<String>,
    pub output_path: Option<String>,
}

/// A .NET `TimeSpan`: `[d.]hh:mm:ss`.
fn time_span(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let clock = format!(
        "{:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    );
    if days > 0 {
        format!("{days}.{clock}")
    } else {
        clock
    }
}

pub fn queue_item(view: &QueueView) -> QueueResource {
    let text = |value: &Option<String>| value.clone().unwrap_or_default();
    let show_path = view.series.as_ref().map(series_path).unwrap_or_default();
    QueueResource {
        id: view.id,
        series_id: view.series.as_ref().map(|series| series.id),
        episode_id: view.episode.as_ref().map(|episode| episode.id),
        season_number: view
            .season_number
            .or_else(|| view.episode.as_ref().map(|episode| episode.season_number)),
        series: view.series.as_ref().map(series_resource),
        episode: view
            .episode
            .as_ref()
            .map(|episode| episode_resource(episode, &show_path)),
        languages: view.languages.iter().map(|code| language(code)).collect(),
        quality: quality_model(view.quality_id),
        size: view.size,
        title: view.title.clone(),
        estimated_completion_time: view.estimated_completion.as_deref().map(Timestamp::parse),
        added: view.added.as_deref().map(Timestamp::parse),
        status: view.status,
        tracked_download_status: view.tracked_status,
        tracked_download_state: view.tracked_state,
        status_messages: view
            .status_messages
            .iter()
            .map(|(title, messages)| TrackedDownloadStatusMessage {
                title: title.clone(),
                messages: messages.clone(),
            })
            .collect(),
        error_message: text(&view.error_message),
        download_id: text(&view.download_id),
        protocol: view.protocol,
        download_client: text(&view.download_client),
        indexer: text(&view.indexer),
        output_path: text(&view.output_path),
        episode_has_file: view
            .episode
            .as_ref()
            .is_some_and(|episode| episode.file.is_some()),
        sizeleft: view.size_left,
        timeleft: view.time_left_seconds.map(time_span),
        ..QueueResource::default()
    }
}

/// One page of `records`; `total` counts all records, not only this page.
pub fn page<T>(
    records: Vec<T>,
    page: i32,
    page_size: i32,
    total: i32,
    sort_key: &str,
    sort_direction: SortDirection,
) -> Paging<T> {
    Paging {
        page,
        page_size,
        sort_key: sort_key.to_owned(),
        sort_direction,
        total_records: total,
        records,
    }
}

// ---------------------------------------------------------------- commands

/// The body of `POST /command`. `seriesId` is not in the upstream `Command`
/// schema, so it is read here and never written back.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CommandRequest {
    pub name: String,
    pub series_id: Option<i32>,
}

/// The commands Unseenarr runs; any other name is answered with 501 (docs/14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandKind {
    MissingEpisodeSearch { series_id: Option<i32> },
    RefreshMonitoredDownloads,
    Unsupported(String),
}

impl CommandRequest {
    /// Command names compare without regard to case, as Sonarr does.
    pub fn kind(self) -> CommandKind {
        match self.name.to_ascii_lowercase().as_str() {
            "missingepisodesearch" => CommandKind::MissingEpisodeSearch {
                series_id: self.series_id.filter(|id| *id > 0),
            },
            "refreshmonitoreddownloads" => CommandKind::RefreshMonitoredDownloads,
            _ => CommandKind::Unsupported(self.name),
        }
    }
}

/// A command that was queued or has run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandView {
    pub id: i32,
    /// Sonarr's command name, such as `MissingEpisodeSearch`.
    pub name: String,
    pub status: CommandStatus,
    pub queued: String,
    pub started: Option<String>,
    pub ended: Option<String>,
}

/// `MissingEpisodeSearch` becomes `Missing Episode Search`.
fn humanize(name: &str) -> String {
    let mut text = String::with_capacity(name.len() + 4);
    for (index, c) in name.chars().enumerate() {
        if index > 0 && c.is_uppercase() {
            text.push(' ');
        }
        text.push(c);
    }
    text
}

pub fn command(view: &CommandView) -> CommandResource {
    let result = match view.status {
        CommandStatus::Completed => CommandResult::Successful,
        CommandStatus::Failed | CommandStatus::Aborted | CommandStatus::Cancelled => {
            CommandResult::Unsuccessful
        }
        _ => CommandResult::Unknown,
    };
    let state_change = view
        .ended
        .as_ref()
        .or(view.started.as_ref())
        .unwrap_or(&view.queued);
    CommandResource {
        id: view.id,
        name: view.name.clone(),
        command_name: humanize(&view.name),
        body: Command {
            send_updates_to_client: true,
            name: view.name.clone(),
            trigger: CommandTrigger::Manual,
            ..Command::default()
        },
        status: view.status,
        result,
        queued: Timestamp::parse(&view.queued),
        started: view.started.as_deref().map(Timestamp::parse),
        ended: view.ended.as_deref().map(Timestamp::parse),
        trigger: CommandTrigger::Manual,
        state_change_time: Some(Timestamp::parse(state_change)),
        send_updates_to_client: true,
        ..CommandResource::default()
    }
}

// ------------------------------------------------------------------ system

/// What `GET /system/status` reports about this process.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemView {
    pub start_time: String,
    pub startup_path: String,
    pub app_data: String,
    pub os_name: String,
    pub os_version: String,
    pub sqlite_version: String,
}

/// The status of an Unseenarr that poses as the pinned Sonarr release.
pub fn system(view: &SystemView) -> SystemResource {
    SystemResource {
        start_time: Timestamp::parse(&view.start_time),
        startup_path: view.startup_path.clone(),
        app_data: view.app_data.clone(),
        os_name: view.os_name.clone(),
        os_version: view.os_version.clone(),
        sqlite_version: view.sqlite_version.clone(),
        database_version: view.sqlite_version.clone(),
        ..SystemResource::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show_view() -> SeriesView {
        SeriesView {
            id: 7,
            tvdb_id: Some(280_619),
            tmdb_id: Some(63_639),
            imdb_id: Some("tt3230854".to_owned()),
            title: "The Expanse".to_owned(),
            original_title: Some("The Expanse".to_owned()),
            sort_title: "expanse".to_owned(),
            year: 2015,
            overview: Some("A detective and a rogue ship's crew.".to_owned()),
            runtime: Some(45),
            status: "ended".to_owned(),
            series_type: "standard".to_owned(),
            network: Some("Prime Video".to_owned()),
            poster_path: Some("/expanse.jpg".to_owned()),
            season_folder: true,
            root_folder_path: "/media/tv".to_owned(),
            folder_name: "The Expanse".to_owned(),
            monitored: true,
            monitor_new_items: "all".to_owned(),
            first_aired: Some("2015-12-14".to_owned()),
            last_aired: Some("2022-01-14T00:00:00Z".to_owned()),
            next_airing: None,
            previous_airing: Some("2022-01-14T00:00:00Z".to_owned()),
            original_language: Some("en".to_owned()),
            added_at: "2026-11-16T20:15:13.123Z".to_owned(),
            aliases: vec!["Expanse".to_owned()],
            tags: vec![2, 5],
            quality_profile_id: 4,
            seasons: vec![
                SeasonView {
                    season_number: 0,
                    monitored: false,
                    title: Some("Specials".to_owned()),
                    episode_file_count: 1,
                    episode_count: 2,
                    total_episode_count: 2,
                    size_on_disk: 500,
                    release_groups: vec!["SPECIAL".to_owned()],
                    ..SeasonView::default()
                },
                SeasonView {
                    season_number: 1,
                    monitored: true,
                    episode_file_count: 5,
                    episode_count: 10,
                    total_episode_count: 10,
                    size_on_disk: 10_000,
                    release_groups: vec!["GRP".to_owned()],
                    ..SeasonView::default()
                },
                SeasonView {
                    season_number: 2,
                    monitored: true,
                    episode_file_count: 10,
                    episode_count: 10,
                    total_episode_count: 13,
                    size_on_disk: 20_000,
                    release_groups: vec!["GRP".to_owned(), "FLUX".to_owned()],
                    ..SeasonView::default()
                },
            ],
        }
    }

    fn sent_back(view: &SeriesView) -> SeriesRequest {
        let json = serde_json::to_string(&series_resource(view)).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn quality_ids_translate_in_both_directions() {
        assert_eq!(QUALITY_ID_TRANSLATIONS.len(), 4);
        for &(unseenarr, sonarr) in QUALITY_ID_TRANSLATIONS {
            assert_eq!(to_sonarr_quality_id(unseenarr), Some(sonarr));
            assert_eq!(from_sonarr_quality_id(sonarr), Some(unseenarr));
        }

        // Exact pairs from docs/07-release-scoring.md ("Qualities for TV").
        assert_eq!(to_sonarr_quality_id(20), Some(13)); // Bluray-480p
        assert_eq!(to_sonarr_quality_id(21), Some(22)); // Bluray-576p
        assert_eq!(to_sonarr_quality_id(30), Some(20)); // Remux-1080p -> Bluray-1080p Remux
        assert_eq!(to_sonarr_quality_id(31), Some(21)); // Remux-2160p -> Bluray-2160p Remux

        assert_eq!(from_sonarr_quality_id(13), Some(20));
        assert_eq!(from_sonarr_quality_id(22), Some(21));
        assert_eq!(from_sonarr_quality_id(20), Some(30));
        assert_eq!(from_sonarr_quality_id(21), Some(31));

        // Round-trip every TV quality.
        for &(core, sonarr_id, name, source, resolution) in TV_QUALITIES {
            assert_eq!(to_sonarr_quality_id(core.id()), Some(sonarr_id));
            assert_eq!(from_sonarr_quality_id(sonarr_id), Some(core.id()));
            let dto = quality(core.id());
            assert_eq!(dto.id, sonarr_id);
            assert_eq!(dto.name, name);
            assert_eq!(dto.source, source);
            assert_eq!(dto.resolution, resolution);
        }

        // Sonarr names for remuxes and DVD resolution differ from Radarr.
        assert_eq!(quality(30).name, "Bluray-1080p Remux");
        assert_eq!(quality(31).name, "Bluray-2160p Remux");
        assert_eq!(quality(2).resolution, 480);

        // Movie-only qualities and unknown ids do not translate; `quality` falls back to Unknown.
        for movie_only in [
            CoreQuality::Workprint,
            CoreQuality::Cam,
            CoreQuality::Telesync,
            CoreQuality::Telecine,
            CoreQuality::Regional,
            CoreQuality::DvdScr,
            CoreQuality::DvdR,
            CoreQuality::BrDisk,
        ] {
            assert_eq!(to_sonarr_quality_id(movie_only.id()), None);
            assert_eq!(quality_from_core(movie_only), None);
            assert_eq!(quality(movie_only.id()).id, 0);
        }
        assert_eq!(from_sonarr_quality_id(11), None);
        assert_eq!(from_sonarr_quality_id(99), None);
    }

    #[test]
    fn series_fields_and_statistics_follow_sonarr_rules() {
        let resource = series_resource(&show_view());
        assert_eq!(resource.title_slug, "280619");
        assert_eq!(resource.path, "/media/tv/The Expanse");
        assert_eq!(resource.root_folder_path, "/media/tv");
        assert_eq!(resource.folder, "The Expanse");
        assert_eq!(resource.clean_title, "theexpanse");
        assert_eq!(resource.clean_original_title, "theexpanse");
        assert_eq!(resource.status, SeriesStatusType::Ended);
        assert!(resource.ended);
        assert_eq!(resource.series_type, SeriesTypes::Standard);
        assert_eq!(resource.monitor_new_items, NewItemMonitorTypes::All);
        assert_eq!(resource.alternate_titles.len(), 1);
        assert_eq!(resource.alternate_titles[0].title, "Expanse");
        assert_eq!(
            resource.first_aired,
            Some(Timestamp("2015-12-14T00:00:00Z".to_owned()))
        );
        assert_eq!(
            resource.remote_poster,
            "https://image.tmdb.org/t/p/original/expanse.jpg"
        );
        assert_eq!(resource.original_language.id, 1);
        assert_eq!(resource.language.id, 1);
        assert_eq!(resource.language_profile_id, 1);

        // Specials (season 0) are excluded from episode counts, included in size_on_disk.
        assert_eq!(resource.statistics.season_count, 2);
        assert_eq!(resource.statistics.episode_file_count, 15);
        assert_eq!(resource.statistics.episode_count, 20);
        assert_eq!(resource.statistics.total_episode_count, 23);
        assert_eq!(resource.statistics.size_on_disk, 30_500);
        assert_eq!(
            resource.statistics.release_groups,
            ["SPECIAL", "GRP", "FLUX"]
        );
        assert!((resource.statistics.percent_of_episodes - 75.0).abs() < f64::EPSILON);

        // Direct TVDB poster URL passes through unchanged; lookup without path stays empty.
        let mut lookup = SeriesView {
            title: "Frieren".to_owned(),
            poster_path: Some("https://artworks.thetvdb.com/poster.jpg".to_owned()),
            ..SeriesView::default()
        };
        let mapped = series_resource(&lookup);
        assert_eq!(
            mapped.remote_poster,
            "https://artworks.thetvdb.com/poster.jpg"
        );
        assert_eq!(mapped.path, "");
        assert_eq!(mapped.title_slug, "frieren");
        assert_eq!(mapped.statistics.percent_of_episodes, 0.0);

        lookup.poster_path = Some("  ".to_owned());
        assert_eq!(series_resource(&lookup).remote_poster, "");
    }

    #[test]
    fn episodes_and_files_translate_qualities_and_paths() {
        let ep = EpisodeView {
            id: 42,
            series_id: 7,
            tvdb_id: Some(5_000_001),
            season_number: 1,
            episode_number: 1,
            absolute_number: Some(1),
            title: "Dulcinea".to_owned(),
            overview: Some("Pilot".to_owned()),
            air_date: Some("2015-12-14".to_owned()),
            air_date_utc: Some("2015-12-15T03:00:00Z".to_owned()),
            runtime: Some(46),
            monitored: true,
            file: Some(EpisodeFileView {
                id: 99,
                series_id: 7,
                season_number: 1,
                relative_path: "Season 1/The Expanse - S01E01.mkv".to_owned(),
                size: 4_000_000_000,
                quality_id: CoreQuality::Remux1080p.id(),
                release_group: Some("GRP".to_owned()),
                scene_name: Some("The.Expanse.S01E01.1080p.BluRay.REMUX-GRP".to_owned()),
                languages: vec!["en".to_owned(), "fr".to_owned()],
                release_type: ReleaseType::SingleEpisode,
                media_info: None,
                quality_cutoff_not_met: false,
                added_at: "2026-11-17T08:00:00Z".to_owned(),
            }),
        };
        let resource = episode_resource(&ep, "/media/tv/The Expanse");
        assert!(resource.has_file);
        assert_eq!(resource.episode_file_id, 99);
        assert_eq!(resource.air_date.as_deref(), Some("2015-12-14"));
        let file = resource.episode_file.expect("episode file");
        assert_eq!(
            file.path,
            "/media/tv/The Expanse/Season 1/The Expanse - S01E01.mkv"
        );
        assert_eq!(file.quality.quality.id, 20);
        assert_eq!(file.quality.quality.name, "Bluray-1080p Remux");
        assert_eq!(file.languages.len(), 2);

        // Empty show path keeps relative path.
        let bare = episode_file_resource("", ep.file.as_ref().unwrap());
        assert_eq!(bare.path, "Season 1/The Expanse - S01E01.mkv");
    }

    #[test]
    fn adding_and_updating_a_series_survive_the_round_trip() {
        let add = sent_back(&show_view()).into_add().unwrap();
        assert_eq!(add.tvdb_id, 280_619);
        assert_eq!(add.tmdb_id, Some(63_639));
        assert_eq!(add.quality_profile_id, 4);
        assert_eq!(add.root_folder_path, "/media/tv");
        assert!(add.monitored);
        assert_eq!(add.monitor_new_items, NewItemMonitorTypes::All);
        assert_eq!(add.series_type, SeriesTypes::Standard);
        assert!(add.season_folder);
        assert_eq!(add.tags, [2, 5]);
        assert_eq!(add.seasons.len(), 3);
        assert_eq!(add.monitor, None);
        assert!(!add.search_for_missing_episodes);

        let mut updated_view = show_view();
        updated_view.monitored = false;
        let update = sent_back(&updated_view).into_update().unwrap();
        assert_eq!(update.id, 7);
        assert_eq!(update.monitored, Some(false));
        assert_eq!(update.tags, Some(vec![2, 5]));
        assert_eq!(update.seasons.unwrap().len(), 3);
    }

    #[test]
    fn a_seerr_series_add_body_is_read() {
        let body = r#"{
            "tvdbId": 280619,
            "title": "The Expanse",
            "qualityProfileId": 4,
            "languageProfileId": 1,
            "seasons": [
                { "seasonNumber": 0, "monitored": false },
                { "seasonNumber": 1, "monitored": true }
            ],
            "tags": [1],
            "seasonFolder": true,
            "monitored": true,
            "monitorNewItems": "none",
            "rootFolderPath": "/tv",
            "seriesType": "anime",
            "addOptions": {
                "ignoreEpisodesWithFiles": true,
                "searchForMissingEpisodes": true
            },
            "unknownField": 123
        }"#;
        let add = serde_json::from_str::<SeriesRequest>(body)
            .unwrap()
            .into_add()
            .unwrap();
        assert_eq!(add.tvdb_id, 280_619);
        assert_eq!(add.series_type, SeriesTypes::Anime);
        assert_eq!(add.monitor_new_items, NewItemMonitorTypes::None);
        assert_eq!(add.monitor, None);
        assert!(add.search_for_missing_episodes);
        assert!(!add.search_for_cutoff_unmet_episodes);
        assert_eq!(add.seasons.len(), 2);
        assert_eq!(add.tags, [1]);
    }

    fn parse_series(body: &str) -> SeriesRequest {
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn series_add_validates_required_fields_and_monitor_modes() {
        assert_eq!(
            parse_series("{}").into_add(),
            Err(MapError::Missing("tvdbId"))
        );
        assert_eq!(
            parse_series(r#"{"tvdbId": 1}"#).into_add(),
            Err(MapError::Missing("qualityProfileId"))
        );
        assert_eq!(
            parse_series(r#"{"tvdbId": 1, "qualityProfileId": 2, "rootFolderPath": " "}"#)
                .into_add(),
            Err(MapError::Missing("rootFolderPath"))
        );
        assert_eq!(
            parse_series("{}").into_update(),
            Err(MapError::Missing("id"))
        );

        for (wire, expected) in [
            ("all", AddMonitor::All),
            ("future", AddMonitor::Future),
            ("missing", AddMonitor::Missing),
            ("none", AddMonitor::None),
        ] {
            assert_eq!(AddMonitor::parse(wire), Some(expected));
            assert_eq!(expected.as_str(), wire);
            assert_eq!(MonitorTypes::from(expected).wire(), wire);
            let add = parse_series(&format!(
                r#"{{"tvdbId":1,"qualityProfileId":2,"rootFolderPath":"/tv","addOptions":{{"monitor":"{wire}"}}}}"#
            ))
            .into_add()
            .unwrap();
            assert_eq!(add.monitor, Some(expected));
        }
        assert_eq!(AddMonitor::parse("pilot"), None);

        for unsupported in [
            "existing",
            "firstSeason",
            "lastSeason",
            "latestSeason",
            "pilot",
            "recent",
            "monitorSpecials",
            "unmonitorSpecials",
            "skip",
        ] {
            let err = parse_series(&format!(
                r#"{{"tvdbId":1,"qualityProfileId":2,"rootFolderPath":"/tv","addOptions":{{"monitor":"{unsupported}"}}}}"#
            ))
            .into_add()
            .unwrap_err();
            assert!(
                matches!(err, MapError::Invalid("addOptions.monitor", ref msg) if msg.contains(unsupported)),
                "unexpected error for {unsupported}: {err:?}"
            );
        }
        assert_eq!(
            AddMonitor::try_from(MonitorTypes::Unknown),
            Err(MapError::Invalid(
                "addOptions.monitor",
                "`unknown` is not supported".to_owned()
            ))
        );
    }

    #[test]
    fn episode_monitor_request_validates_ids_and_flag() {
        let ok = serde_json::from_str::<EpisodeMonitorRequest>(
            r#"{"episodeIds": [10, 11], "monitored": true, "extra": 1}"#,
        )
        .unwrap()
        .into_monitor()
        .unwrap();
        assert_eq!(
            ok,
            EpisodeMonitor {
                episode_ids: vec![10, 11],
                monitored: true,
            }
        );

        assert_eq!(
            EpisodeMonitorRequest::default().into_monitor(),
            Err(MapError::Missing("episodeIds"))
        );
        assert_eq!(
            EpisodeMonitorRequest {
                episode_ids: vec![1, 0],
                monitored: Some(true),
            }
            .into_monitor(),
            Err(MapError::Invalid(
                "episodeIds",
                "ids must be positive".to_owned()
            ))
        );
        assert_eq!(
            EpisodeMonitorRequest {
                episode_ids: vec![1],
                monitored: None,
            }
            .into_monitor(),
            Err(MapError::Missing("monitored"))
        );
    }

    #[test]
    fn map_error_displays_field_and_reason() {
        assert_eq!(
            MapError::Missing("tvdbId").to_string(),
            "tvdbId is required"
        );
        assert_eq!(
            MapError::Invalid("addOptions.monitor", "`pilot` is not supported".to_owned())
                .to_string(),
            "addOptions.monitor: `pilot` is not supported"
        );
    }

    #[test]
    fn languages_use_sonarr_ids() {
        assert_eq!(
            language("any"),
            Language {
                id: -1,
                name: "Any".to_owned()
            }
        );
        assert_eq!(language("original").id, -2);
        assert_eq!(language("EN").name, "English");
        assert_eq!(language("ar").id, 26);
        assert_eq!(language("pt-BR").id, 33);
        assert_eq!(language("uk").id, 30);
        assert_eq!(language("ga").id, 53);
        assert_eq!(language("xyz").id, 0);
    }

    #[test]
    fn quality_profile_translates_cutoff_and_items_and_keeps_group_ids() {
        let profile = quality_profile(&ProfileView {
            id: 1,
            name: "Remux".to_owned(),
            upgrade_allowed: true,
            cutoff_quality_id: CoreQuality::Remux1080p.id(), // Unseenarr 30 -> Sonarr 20
            items: vec![
                Item::Quality {
                    quality_id: CoreQuality::Bluray480p.id(), // Unseenarr 20 -> Sonarr 13
                    allowed: false,
                },
                Item::Group {
                    id: 1000,
                    name: "WEB 1080p".to_owned(),
                    quality_ids: vec![CoreQuality::WebDl1080p.id(), CoreQuality::WebRip1080p.id()],
                    allowed: true,
                },
                Item::Quality {
                    quality_id: CoreQuality::Remux1080p.id(), // Unseenarr 30 -> Sonarr 20
                    allowed: true,
                },
            ],
            min_format_score: 0,
            cutoff_format_score: 10,
            min_upgrade_format_score: 1,
        });
        assert_eq!(profile.cutoff, 20);
        assert_eq!(profile.items[0].quality.as_ref().unwrap().id, 13);
        assert_eq!(profile.items[1].id, 1000);
        assert!(profile.items[1].quality.is_none());
        assert_eq!(
            profile.items[2].quality.as_ref().unwrap().name,
            "Bluray-1080p Remux"
        );

        let group_cutoff = quality_profile(&ProfileView {
            id: 2,
            name: "Group".to_owned(),
            upgrade_allowed: true,
            cutoff_quality_id: 1000,
            items: vec![],
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
        });
        assert_eq!(group_cutoff.cutoff, 1000);
    }

    #[test]
    fn language_profile_maps_cutoff_and_items() {
        let mapped = language_profile(&LanguageProfileView {
            id: 1,
            name: "English".to_owned(),
            upgrade_allowed: false,
            cutoff: "en".to_owned(),
            languages: vec![("en".to_owned(), true), ("fr".to_owned(), false)],
        });
        assert_eq!(mapped.cutoff.id, 1);
        assert_eq!(mapped.languages.len(), 2);
        assert!(mapped.languages[0].allowed);
        assert_eq!(mapped.languages[1].language.id, 2);
    }

    #[test]
    fn commands_are_told_apart_by_name() {
        let kind = |body: &str| serde_json::from_str::<CommandRequest>(body).unwrap().kind();
        assert_eq!(
            kind(r#"{"name": "MissingEpisodeSearch", "seriesId": 7}"#),
            CommandKind::MissingEpisodeSearch { series_id: Some(7) }
        );
        assert_eq!(
            kind(r#"{"name": "missingepisodesearch"}"#),
            CommandKind::MissingEpisodeSearch { series_id: None }
        );
        assert_eq!(
            kind(r#"{"name": "RefreshMonitoredDownloads"}"#),
            CommandKind::RefreshMonitoredDownloads
        );
        assert_eq!(
            kind(r#"{"name": "RescanSeries"}"#),
            CommandKind::Unsupported("RescanSeries".to_owned())
        );

        let failed = command(&CommandView {
            id: 2,
            name: "MissingEpisodeSearch".to_owned(),
            status: CommandStatus::Failed,
            queued: "2026-11-25T10:00:00Z".to_owned(),
            started: Some("2026-11-25T10:00:01Z".to_owned()),
            ended: None,
        });
        assert_eq!(failed.result, CommandResult::Unsuccessful);
        assert_eq!(failed.command_name, "Missing Episode Search");

        let queued = command(&CommandView {
            id: 3,
            name: "RefreshMonitoredDownloads".to_owned(),
            status: CommandStatus::Queued,
            queued: "2026-11-25T10:00:00Z".to_owned(),
            started: None,
            ended: None,
        });
        assert_eq!(queued.result, CommandResult::Unknown);
    }

    #[test]
    fn tags_and_root_folders_map_both_ways() {
        assert_eq!(
            tag(3, "anime"),
            TagResource {
                id: 3,
                label: "anime".to_owned()
            }
        );
        assert_eq!(tag_label(&tag(0, "  anime ")).unwrap(), "anime");
        assert_eq!(tag_label(&tag(0, " ")), Err(MapError::Missing("label")));
        let folder = root_folder(&RootFolderView {
            id: 1,
            path: r"D:\TV\".to_owned(),
            free_space: Some(500),
            total_space: Some(1_000),
            unmapped: vec!["Stray".to_owned()],
            ..RootFolderView::default()
        });
        assert_eq!(folder.total_space, Some(1_000));
        assert_eq!(folder.unmapped_folders[0].path, r"D:\TV\Stray");
        assert_eq!(folder.unmapped_folders[0].relative_path, "Stray");
    }

    #[test]
    fn a_time_span_has_days_when_it_needs_them() {
        assert_eq!(time_span(59), "00:00:59");
        assert_eq!(time_span(3_661), "01:01:01");
        assert_eq!(time_span(93_784), "1.02:03:04");
    }
}
