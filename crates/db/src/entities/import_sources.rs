use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "import_sources")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub kind: String,
    pub name: String,
    pub url: String,
    pub api_key: String,
    pub target_quality_profile_id: Option<i32>,
    pub remote_path: Option<String>,
    pub local_path: Option<String>,
    pub position: i32,
    pub resources: Option<String>,
    pub import_quality_definitions: bool,
    pub last_import_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
