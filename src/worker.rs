use anyhow::Result;
use crate::db::DBConnection;

pub async fn execute_worker(mut _db: DBConnection) -> Result<()> {
    Ok(())
}
