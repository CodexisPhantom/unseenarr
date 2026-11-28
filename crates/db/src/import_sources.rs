//! Import sources and their resource map (docs/04-database.md, "System
//! tables"). The API key arrives encrypted; this module never sees plain text.
//!
//! `import_map` records which local row a source resource became, so a second
//! run of the same import creates nothing new.
use crate::entities::{import_map, import_sources};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder,
    Set, SqlErr, TransactionTrait, sea_query::OnConflict,
};
use std::collections::HashMap;
/// The resources an import maps. The value is stored in `import_map.resource`.
pub const ROOT_FOLDER: &str = "root_folder";
pub const QUALITY_PROFILE: &str = "quality_profile";
pub const CUSTOM_FORMAT: &str = "custom_format";
pub const TAG: &str = "tag";
pub const COLLECTION: &str = "collection";
pub const MOVIE: &str = "movie";
pub const MOVIE_PROFILE: &str = "movie_profile";
pub const MOVIE_FILE: &str = "movie_file";
pub const INDEXER: &str = "indexer";
pub const DOWNLOAD_CLIENT: &str = "download_client";

/// What a caller sets on a new import source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Values {
    pub kind: String,
    pub name: String,
    pub url: String,
    /// The encrypted key, as `secret::encrypt` returns it.
    pub api_key: String,
    pub target_quality_profile_id: Option<i32>,
    /// The path prefix as the source sees it, and its local replacement.
    pub remote_path: Option<String>,
    pub local_path: Option<String>,
    /// The resource groups to import, as a JSON array, or `None` for all.
    pub resources: Option<String>,
    pub import_quality_definitions: bool,
}

#[derive(Debug)]
pub enum SourceError {
    NotFound,
    /// Another source has this URL.
    UrlTaken,
    /// The target quality profile does not exist.
    UnknownProfile,
    Db(DbErr),
}

impl From<DbErr> for SourceError {
    fn from(error: DbErr) -> Self {
        match error.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(_)) => Self::UrlTaken,
            Some(SqlErr::ForeignKeyConstraintViolation(_)) => Self::UnknownProfile,
            _ => Self::Db(error),
        }
    }
}

/// Every source, by position then id.
pub async fn list(db: &DatabaseConnection) -> Result<Vec<import_sources::Model>, DbErr> {
    import_sources::Entity::find()
        .order_by_asc(import_sources::Column::Position)
        .order_by_asc(import_sources::Column::Id)
        .all(db)
        .await
}

pub async fn find(
    db: &DatabaseConnection,
    id: i32,
) -> Result<Option<import_sources::Model>, DbErr> {
    import_sources::Entity::find_by_id(id).one(db).await
}

pub async fn create(
    db: &DatabaseConnection,
    values: Values,
) -> Result<import_sources::Model, SourceError> {
    let now = crate::now();
    let position = import_sources::Entity::find()
        .order_by_desc(import_sources::Column::Position)
        .one(db)
        .await
        .map_err(SourceError::from)?
        .map(|row| row.position + 1)
        .unwrap_or(0);
    Ok(import_sources::ActiveModel {
        kind: Set(values.kind),
        name: Set(values.name),
        url: Set(values.url),
        api_key: Set(values.api_key),
        target_quality_profile_id: Set(values.target_quality_profile_id),
        remote_path: Set(values.remote_path),
        local_path: Set(values.local_path),
        position: Set(position),
        resources: Set(values.resources),
        import_quality_definitions: Set(values.import_quality_definitions),
        last_import_at: Set(None),
        created_at: Set(now.clone()),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?)
}

pub async fn delete(db: &DatabaseConnection, id: i32) -> Result<(), SourceError> {
    if import_sources::Entity::delete_by_id(id)
        .exec(db)
        .await?
        .rows_affected
        == 0
    {
        return Err(SourceError::NotFound);
    }
    Ok(())
}

/// Sets each source's position to its index in `ids`.
pub async fn reorder(db: &DatabaseConnection, ids: &[i32]) -> Result<(), SourceError> {
    let txn = db.begin().await?;
    let known = import_sources::Entity::find().all(&txn).await?;
    let mut wanted_ids = ids.to_vec();
    wanted_ids.sort_unstable();
    wanted_ids.dedup();
    let mut known_ids: Vec<_> = known.iter().map(|source| source.id).collect();
    known_ids.sort_unstable();
    if wanted_ids != known_ids || ids.len() > i32::MAX as usize {
        return Err(SourceError::NotFound);
    }
    let by_id: HashMap<_, _> = known
        .into_iter()
        .map(|source| (source.id, source))
        .collect();
    for (index, id) in ids.iter().enumerate() {
        let current = by_id.get(id).ok_or(SourceError::NotFound)?.clone();
        let mut row: import_sources::ActiveModel = current.into();
        row.position = Set(index as i32);
        row.updated_at = Set(crate::now());
        row.update(&txn).await?;
    }
    txn.commit().await?;
    Ok(())
}

/// Records that a run finished, whether or not it created anything.
pub async fn mark_imported(
    db: &DatabaseConnection,
    id: i32,
) -> Result<import_sources::Model, SourceError> {
    let current = find(db, id).await?.ok_or(SourceError::NotFound)?;
    let mut source: import_sources::ActiveModel = current.into();
    let now = crate::now();
    source.last_import_at = Set(Some(now.clone()));
    source.updated_at = Set(now);
    Ok(source.update(db).await?)
}

/// The local id a source resource became, if it was imported before.
pub async fn map_get(
    db: &DatabaseConnection,
    source_id: i32,
    resource: &str,
    source_resource_id: i32,
) -> Result<Option<i32>, DbErr> {
    Ok(
        import_map::Entity::find_by_id((source_id, resource.to_owned(), source_resource_id))
            .one(db)
            .await?
            .map(|row| row.local_id),
    )
}

/// Every mapped resource of one kind for a source.
pub async fn map_for(
    db: &DatabaseConnection,
    source_id: i32,
    resource: &str,
) -> Result<HashMap<i32, i32>, DbErr> {
    let rows = import_map::Entity::find()
        .filter(import_map::Column::SourceId.eq(source_id))
        .filter(import_map::Column::Resource.eq(resource))
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.source_resource_id, row.local_id))
        .collect())
}

/// Records a mapping. A second run of the same import finds it and skips.
/// One atomic upsert: a repeated put overwrites `local_id`.
pub async fn map_put(
    db: &DatabaseConnection,
    source_id: i32,
    resource: &str,
    source_resource_id: i32,
    local_id: i32,
) -> Result<(), DbErr> {
    import_map::Entity::insert(import_map::ActiveModel {
        source_id: Set(source_id),
        resource: Set(resource.to_owned()),
        source_resource_id: Set(source_resource_id),
        local_id: Set(local_id),
    })
    .on_conflict(
        OnConflict::columns([
            import_map::Column::SourceId,
            import_map::Column::Resource,
            import_map::Column::SourceResourceId,
        ])
        .update_column(import_map::Column::LocalId)
        .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_db;

    fn values(url: &str) -> Values {
        Values {
            kind: "radarr".to_owned(),
            name: "Radarr 1080p".to_owned(),
            url: url.to_owned(),
            api_key: "encrypted".to_owned(),
            target_quality_profile_id: None,
            remote_path: None,
            local_path: None,
            resources: None,
            import_quality_definitions: false,
        }
    }

    #[tokio::test]
    async fn creates_lists_and_deletes_a_source() {
        let (db, _dir) = temp_db().await;
        let first = create(&db, values("http://radarr:7878")).await.unwrap();
        let second = create(&db, values("http://radarr:7879")).await.unwrap();
        assert_eq!(first.kind, "radarr");
        assert_eq!(second.position, first.position + 1);
        let listed = list(&db).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, first.id);
        assert_eq!(listed[1].id, second.id);
        assert_eq!(find(&db, first.id).await.unwrap().unwrap().id, first.id);
        reorder(&db, &[second.id, first.id]).await.unwrap();
        let listed = list(&db).await.unwrap();
        assert_eq!(listed[0].id, second.id);
        assert_eq!(listed[1].id, first.id);
        let positions: Vec<_> = listed.iter().map(|source| source.position).collect();
        assert!(matches!(
            reorder(&db, &[first.id, 999_999]).await,
            Err(SourceError::NotFound)
        ));
        assert_eq!(
            list(&db)
                .await
                .unwrap()
                .iter()
                .map(|source| source.position)
                .collect::<Vec<_>>(),
            positions
        );
        delete(&db, first.id).await.unwrap();
        delete(&db, second.id).await.unwrap();
        assert!(list(&db).await.unwrap().is_empty());
        assert!(matches!(
            delete(&db, first.id).await,
            Err(SourceError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_second_source_with_the_same_url_is_refused() {
        let (db, _dir) = temp_db().await;
        create(&db, values("http://radarr:7878")).await.unwrap();
        assert!(matches!(
            create(&db, values("http://radarr:7878")).await,
            Err(SourceError::UrlTaken)
        ));
    }

    #[tokio::test]
    async fn the_map_is_idempotent_and_cascades_with_its_source() {
        let (db, _dir) = temp_db().await;
        let source = create(&db, values("http://radarr:7878")).await.unwrap();
        map_put(&db, source.id, MOVIE, 1, 10).await.unwrap();
        map_put(&db, source.id, MOVIE, 1, 11).await.unwrap();
        assert_eq!(map_get(&db, source.id, MOVIE, 1).await.unwrap(), Some(11));
        assert_eq!(
            map_for(&db, source.id, MOVIE).await.unwrap(),
            HashMap::from([(1, 11)])
        );
        delete(&db, source.id).await.unwrap();
        assert_eq!(map_get(&db, source.id, MOVIE, 1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn marks_the_last_import() {
        let (db, _dir) = temp_db().await;
        let source = create(&db, values("http://radarr:7878")).await.unwrap();
        assert!(source.last_import_at.is_none());
        let updated = mark_imported(&db, source.id).await.unwrap();
        assert!(updated.last_import_at.is_some());
    }
}
