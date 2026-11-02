use crate::models::Track;
use crate::util::native_path_string;
use anyhow::{bail, Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::collections::HashMap;
use std::path::Path;

const TRACKS: TableDefinition<u64, &[u8]> = TableDefinition::new("tracks");
const PATHS: TableDefinition<&str, u64> = TableDefinition::new("paths");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const NEXT_ID: &str = "next_id";

pub struct Store {
    db: Database,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::create(path)
            .with_context(|| format!("无法打开索引数据库: {}", path.display()))?;
        let write = db.begin_write()?;
        {
            let _ = write.open_table(TRACKS)?;
            let _ = write.open_table(PATHS)?;
            let mut meta = write.open_table(META)?;
            if meta.get(NEXT_ID)?.is_none() {
                meta.insert(NEXT_ID, 1)?;
            }
        }
        write.commit()?;
        let store = Self { db };
        store.normalize_index()?;
        Ok(store)
    }

    /// Repair indexes written by older versions where a rename could store
    /// `a/b` instead of `a\b` on Windows. This also collapses duplicate rows
    /// that point at the same physical file, keeping the newest one.
    fn normalize_index(&self) -> Result<()> {
        let tracks = self.all()?;
        let mut unique: HashMap<String, Track> = HashMap::new();
        let mut needs_rewrite = false;
        for mut track in tracks {
            if track.artist.trim().is_empty() && !track.artists.is_empty() {
                // Migrate records written before `artist` and `artists` were
                // stored separately.
                track.artist = track.artists.first().cloned().unwrap_or_default();
                needs_rewrite = true;
            }
            let normalized = native_path_string(Path::new(&track.path));
            if normalized != track.path {
                needs_rewrite = true;
            }
            match unique.get(&normalized) {
                Some(existing)
                    if (existing.updated_at, existing.id) >= (track.updated_at, track.id) =>
                {
                    needs_rewrite = true;
                }
                Some(_) => {
                    needs_rewrite = true;
                    track.path = normalized.clone();
                    unique.insert(normalized, track);
                }
                None => {
                    track.path = normalized.clone();
                    unique.insert(normalized, track);
                }
            }
        }
        if !needs_rewrite {
            return Ok(());
        }
        let write = self.db.begin_write()?;
        {
            let mut tracks_table = write.open_table(TRACKS)?;
            let mut paths_table = write.open_table(PATHS)?;
            tracks_table.retain(|_, _| false)?;
            paths_table.retain(|_, _| false)?;
            for track in unique.values() {
                let value = serde_json::to_vec(track)?;
                tracks_table.insert(track.id, value.as_slice())?;
                paths_table.insert(track.path.as_str(), track.id)?;
            }
        }
        write.commit()?;
        Ok(())
    }

    pub fn count(&self) -> Result<usize> {
        let read = self.db.begin_read()?;
        let table = read.open_table(TRACKS)?;
        Ok(table.len()? as usize)
    }

    pub fn get(&self, id: u64) -> Result<Option<Track>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(TRACKS)?;
        match table.get(id)? {
            Some(value) => Ok(Some(
                serde_json::from_slice(value.value())
                    .with_context(|| format!("索引记录损坏: {id}"))?,
            )),
            None => Ok(None),
        }
    }

    pub fn all(&self) -> Result<Vec<Track>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(TRACKS)?;
        let mut out = Vec::with_capacity(table.len()? as usize);
        for item in table.iter()? {
            let (_, value) = item?;
            match serde_json::from_slice::<Track>(value.value()) {
                Ok(track) => out.push(track),
                Err(err) => tracing::warn!("跳过损坏的索引记录: {err}"),
            }
        }
        Ok(out)
    }

    pub fn save(&self, track: &Track) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut tracks = write.open_table(TRACKS)?;
            let mut paths = write.open_table(PATHS)?;
            let value = serde_json::to_vec(track)?;
            tracks.insert(track.id, value.as_slice())?;
            paths.insert(track.path.as_str(), track.id)?;
        }
        write.commit()?;
        Ok(())
    }

    /// Insert or update tracks by path, allocating IDs in one transaction.
    pub fn upsert_many(&self, tracks: &mut [Track]) -> Result<()> {
        if tracks.is_empty() {
            return Ok(());
        }
        let write = self.db.begin_write()?;
        let mut next_id;
        {
            let mut tracks_table = write.open_table(TRACKS)?;
            let mut paths_table = write.open_table(PATHS)?;
            let mut meta = write.open_table(META)?;
            next_id = meta.get(NEXT_ID)?.map(|v| v.value()).unwrap_or(1);
            for track in tracks.iter_mut() {
                let existing = paths_table
                    .get(track.path.as_str())?
                    .map(|value| value.value());
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let id = next_id;
                        next_id += 1;
                        paths_table.insert(track.path.as_str(), id)?;
                        id
                    }
                };
                track.id = id;
                let value = serde_json::to_vec(track)?;
                tracks_table.insert(id, value.as_slice())?;
            }
            meta.insert(NEXT_ID, next_id)?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn delete(&self, id: u64) -> Result<()> {
        let track = self.get(id)?;
        let write = self.db.begin_write()?;
        {
            let mut tracks = write.open_table(TRACKS)?;
            let mut paths = write.open_table(PATHS)?;
            tracks.remove(id)?;
            if let Some(track) = track {
                paths.remove(track.path.as_str())?;
            }
        }
        write.commit()?;
        Ok(())
    }

    pub fn move_path(
        &self,
        id: u64,
        new_path: &str,
        root: &str,
        rel: &str,
        filename: &str,
    ) -> Result<()> {
        let mut track = self.get(id)?.context("歌曲不存在")?;
        let old_path = track.path.clone();
        let new_path = native_path_string(Path::new(new_path));
        let write = self.db.begin_write()?;
        {
            let mut tracks = write.open_table(TRACKS)?;
            let mut paths = write.open_table(PATHS)?;
            if let Some(existing) = paths.get(new_path.as_str())?.map(|v| v.value()) {
                if existing != id {
                    bail!("目标路径已被索引: {new_path}");
                }
            }
            paths.remove(old_path.as_str())?;
            paths.insert(new_path.as_str(), id)?;
            track.path = new_path;
            track.root = root.to_string();
            track.rel_path = rel.to_string();
            track.filename = filename.to_string();
            let value = serde_json::to_vec(&track)?;
            tracks.insert(id, value.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn delete_missing(&self, root: &str) -> Result<usize> {
        let tracks = self.all()?;
        let mut ids = Vec::new();
        for track in tracks {
            if track.root == root && !Path::new(&track.path).exists() {
                ids.push(track.id);
            }
        }
        for id in &ids {
            self.delete(*id)?;
        }
        Ok(ids.len())
    }
}
