#[cfg(test)]
mod tests {
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn presets_table_exists_after_migration() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rewrite_presets")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
}
