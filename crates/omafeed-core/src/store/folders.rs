//! Folders: creating, renaming, moving and removing them.
use super::Store;
use crate::{Error, model::FolderId};
use anyhow::Result;
use rusqlite::params;

const FOLDER_CYCLE: &str = "
WITH RECURSIVE descendants(id) AS (
    SELECT ?1
    UNION ALL
    SELECT f.id FROM folders f JOIN descendants d ON f.parent = d.id
)
SELECT EXISTS(SELECT 1 FROM descendants WHERE id = ?2)";

impl Store {
    /// Create a folder in `parent`, or at the top level.
    pub fn add_folder(&self, name: &str, parent: Option<FolderId>) -> Result<FolderId> {
        self.conn
            .execute(
                "INSERT INTO folders(name, parent) VALUES(?1, ?2)",
                params![Self::name(name)?, parent],
            )
            .map_err(Error::folder_name)?;
        Ok(FolderId(self.conn.last_insert_rowid()))
    }

    /// Rename a folder; the name must be unique among its siblings. Fails with
    /// [`Error::Managed`] for a folder that is a Miniflux category, unless the name stays.
    pub fn rename_folder(&self, id: FolderId, name: &str) -> Result<()> {
        let unchanged: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1 AND name = ?2)",
            params![id, Self::name(name)?],
            |r| r.get(0),
        )?;
        if !unchanged {
            self.require_local_folder(id)?;
        }
        self.rename_folder_now(id, name)
    }

    /// [`Store::rename_folder`], whoever decides the folder: for mirroring a category renamed
    /// on the Miniflux server.
    pub(crate) fn rename_folder_now(&self, id: FolderId, name: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE folders SET name = ?1 WHERE id = ?2",
                params![Self::name(name)?, id],
            )
            .map_err(Error::folder_name)?;
        Ok(())
    }

    /// Move a folder into `parent`, or to the top level, but never inside itself.
    pub fn move_folder(&self, id: FolderId, parent: Option<FolderId>) -> Result<()> {
        if let Some(p) = parent
            && self
                .conn
                .query_row(FOLDER_CYCLE, params![id, p], |r| r.get::<_, bool>(0))?
        {
            return Err(Error::Validation(
                "A folder cannot be moved inside itself or its children".into(),
            )
            .into());
        }
        self.conn
            .execute(
                "UPDATE folders SET parent = ?1 WHERE id = ?2",
                params![parent, id],
            )
            .map_err(Error::folder_name)?;
        Ok(())
    }

    /// Rename and move a folder atomically.
    pub fn edit_folder(&self, id: FolderId, name: &str, parent: Option<FolderId>) -> Result<()> {
        self.immediate(|s| {
            s.rename_folder(id, name)?;
            s.move_folder(id, parent)
        })
    }

    /// Removing a folder keeps all feeds and promotes child folders to its parent. Fails with
    /// [`Error::Managed`] for a folder that is a Miniflux category.
    pub fn delete_folder(&self, id: FolderId) -> Result<()> {
        self.immediate(|s| {
            s.require_local_folder(id)?;
            s.delete_folder_now(id)
        })
    }

    pub(crate) fn delete_folder_now(&self, id: FolderId) -> Result<()> {
        let tx = &self.conn;
        let parent: Option<FolderId> =
            tx.query_row("SELECT parent FROM folders WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
        let children = tx
            .prepare("SELECT id, name FROM folders WHERE parent = ?1")?
            .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        // Disambiguate names when promoting children into an existing sibling group.
        for (child, name) in children {
            let mut candidate = name.clone();
            let mut suffix = 2;
            while tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM folders WHERE parent IS ?1 AND name = ?2 AND id <> ?3)",
                params![parent, candidate, id],
                |r| r.get::<_, bool>(0),
            )? {
                candidate = format!("{name} ({suffix})");
                suffix += 1;
            }
            tx.execute(
                "UPDATE folders SET parent = ?1, name = ?2 WHERE id = ?3",
                params![parent, candidate, child],
            )?;
        }
        tx.execute(
            "UPDATE feeds SET folder = ?1 WHERE folder = ?2",
            params![parent, id],
        )?;
        tx.execute("DELETE FROM folders WHERE id = ?1", [id])?;
        Ok(())
    }
}
