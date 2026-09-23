//! Incremental NFS directory generations over delimiter-based S3 listing.
//!
//! nfsserve 0.11 puts each inode ID on the wire as its READDIR cookie; its VFS
//! has no independent cookie/generation field. Bind emitted cookies to a name
//! and the active listing generation, and reject stale/foreign continuations.
//! Reissued inode IDs cannot identify an old client's generation, so this is
//! stable enumeration of an unchanged directory, not snapshot isolation while
//! other clients modify its namespace.

use super::*;
use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Output;
use std::collections::HashSet;

type EntryAttributes = (EntryKind, u64, u32);

pub(super) struct DirListing {
    pub(super) children: Arc<Vec<DirChild>>,
    pub(super) fetched_at: Instant,
    key: String,
    generation: u64,
    complete: bool,
    continuation_token: Option<String>,
    seen_tokens: HashSet<String>,
    // Names whose kind/order may still be changed by a later common prefix.
    pending: BTreeMap<String, EntryAttributes>,
    // Last raw object key/common prefix, before dropping the delimiter.
    watermark: Option<String>,
}

pub(super) struct DirectoryCookie {
    name: String,
    generation: u64,
}

struct DirectoryView {
    children: Arc<Vec<DirChild>>,
    complete: bool,
    generation: u64,
}

struct ProviderPage {
    entries: BTreeMap<String, EntryAttributes>,
    first_key: Option<String>,
    last_key: Option<String>,
    continuation: Option<String>,
}

impl DirListing {
    fn new(key: &str, generation: u64) -> Self {
        Self {
            children: Arc::new(Vec::new()),
            fetched_at: Instant::now(),
            key: key.to_string(),
            generation,
            complete: false,
            continuation_token: None,
            seen_tokens: HashSet::new(),
            pending: BTreeMap::new(),
            watermark: None,
        }
    }

    #[cfg(test)]
    pub(super) fn complete(children: Arc<Vec<DirChild>>) -> Self {
        Self {
            children,
            complete: true,
            ..Self::new("", 0)
        }
    }

    fn view_if_ready(&self, after_name: Option<&str>, all: bool) -> Option<DirectoryView> {
        if self.complete || (!all && resume_index(&self.children, after_name) < self.children.len())
        {
            Some(DirectoryView {
                children: self.children.clone(),
                complete: self.complete,
                generation: self.generation,
            })
        } else {
            None
        }
    }

    fn child_named(&self, name: &str) -> Option<DirChild> {
        self.children
            .binary_search_by(|child| child.name.as_str().cmp(name))
            .ok()
            .and_then(|index| self.children.get(index).cloned())
    }

    /// Whether any page received so far returned `name`, emitted or still
    /// held back until the next page settles its order.
    fn mentions(&self, name: &str) -> bool {
        self.child_named(name).is_some() || self.pending.contains_key(name)
    }
}

/// A future common prefix can sort before an already received file when its
/// trailing slash is removed. For example `a!`, `a#`, ... precede `a/` in S3,
/// but the eventual directory `a` precedes all of them in NFS name order.
/// Keep every name from the earliest such ambiguous prefix onward. With only
/// ordinary letters this is simply the last name (one-entry lookahead).
fn safe_name_frontier(raw_watermark: &str) -> &str {
    // `/` is an unrepresentable empty directory name, but remains a real
    // provider watermark. Do not turn it into an empty ordering frontier.
    let name = raw_watermark
        .strip_suffix('/')
        .filter(|name| !name.is_empty())
        .unwrap_or(raw_watermark);
    for (index, character) in name.char_indices().skip(1) {
        if character < '/' {
            return &name[..index];
        }
    }
    name
}

impl ProviderPage {
    fn parse(response: &ListObjectsV2Output, dir_key: &str) -> Result<Self, nfsstat3> {
        let mut raw_entries = BTreeMap::new();
        for prefix in response.common_prefixes() {
            let prefix = prefix.prefix().ok_or(nfsstat3::NFS3ERR_IO)?;
            let name = prefix
                .strip_prefix(dir_key)
                .and_then(|relative| relative.strip_suffix('/'))
                .filter(|name| !name.contains('/'))
                .ok_or(nfsstat3::NFS3ERR_IO)?;
            raw_entries.insert(format!("{name}/"), (EntryKind::Dir, DIR_SIZE, 0));
        }
        for object in response.contents() {
            let key = object.key().ok_or(nfsstat3::NFS3ERR_IO)?;
            if key == dir_key {
                continue; // This directory's own marker is not its child.
            }
            let relative = key.strip_prefix(dir_key).ok_or(nfsstat3::NFS3ERR_IO)?;
            let name = relative.strip_suffix('/').unwrap_or(relative);
            if name.contains('/') {
                return Err(nfsstat3::NFS3ERR_IO);
            }
            let attributes = if relative.ends_with('/') {
                (EntryKind::Dir, DIR_SIZE, 0)
            } else {
                let size = object
                    .size()
                    .filter(|size| *size >= 0)
                    .ok_or(nfsstat3::NFS3ERR_IO)?;
                let seconds = object.last_modified().map(|time| time.secs()).unwrap_or(0);
                (
                    EntryKind::File,
                    size as u64,
                    u32::try_from(seconds.max(0)).unwrap_or(u32::MAX),
                )
            };
            raw_entries.insert(relative.to_string(), attributes);
        }
        let continuation = if response.is_truncated().unwrap_or(false) {
            Some(
                response
                    .next_continuation_token()
                    .filter(|token| !token.is_empty())
                    .ok_or(nfsstat3::NFS3ERR_IO)?
                    .to_string(),
            )
        } else {
            None
        };
        let first_key = raw_entries.first_key_value().map(|(key, _)| key.clone());
        let last_key = raw_entries.last_key_value().map(|(key, _)| key.clone());
        let mut entries = BTreeMap::new();
        for (raw, attributes) in raw_entries {
            let name = raw.strip_suffix('/').unwrap_or(&raw).to_string();
            // Empty path components exist in S3 (e.g. `/` or `a//`) but
            // cannot be emitted as an NFS child. Their raw keys still bound
            // the continuation above, so skipping them cannot reset paging.
            if name.is_empty() {
                continue;
            }
            let entry = entries.entry(name).or_insert(attributes);
            if attributes.0 == EntryKind::Dir {
                *entry = attributes;
            }
        }
        Ok(Self {
            entries,
            first_key,
            last_key,
            continuation,
        })
    }
}

impl S3NfsFs {
    /// Absence-sensitive callers complete the same generation used by READDIR.
    /// A partial cache can never establish that a name does not exist.
    pub(super) async fn children_of(
        &self,
        dirid: fileid3,
        dir_key: &str,
    ) -> Result<Arc<Vec<DirChild>>, nfsstat3> {
        Ok(self
            .directory_view(dirid, dir_key, None, None, true)
            .await?
            .children)
    }

    async fn directory_view(
        &self,
        dirid: fileid3,
        dir_key: &str,
        after_name: Option<&str>,
        expected_generation: Option<u64>,
        all: bool,
    ) -> Result<DirectoryView, nfsstat3> {
        let flight = {
            let mut flights = self.inner.directory_flights.lock().await;
            flights.retain(|_, value| value.strong_count() != 0);
            let flight = flights
                .get(&dirid)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
            flights.insert(dirid, Arc::downgrade(&flight));
            flight
        };
        loop {
            if let Some(view) =
                self.cached_directory_view(dirid, dir_key, after_name, expected_generation, all)?
            {
                return Ok(view);
            }
            // Own at most one provider page. A complete-list caller yields
            // between pages so an already waiting READDIR gets the first page.
            let _flight = flight.lock().await;
            if let Some(view) =
                self.cached_directory_view(dirid, dir_key, after_name, expected_generation, all)?
            {
                return Ok(view);
            }
            let (generation, continuation_token) = {
                let mut dirs = self
                    .inner
                    .dirs
                    .write()
                    .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
                let refresh = dirs.get(&dirid).is_none_or(|listing| {
                    listing.key != dir_key || listing.fetched_at.elapsed() >= DIR_CACHE_TTL
                });
                if refresh {
                    if expected_generation.is_some() {
                        return Err(nfsstat3::NFS3ERR_BAD_COOKIE);
                    }
                    // Strictly newer than any generation a lookup has read so
                    // far, so a cache entry recorded before this listing
                    // existed can never pass for one of its own.
                    let generation = self
                        .inner
                        .directory_generation
                        .fetch_add(1, Ordering::SeqCst)
                        + 1;
                    dirs.insert(dirid, DirListing::new(dir_key, generation));
                    self.inner
                        .directory_cookies
                        .write()
                        .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?
                        .retain(|(directory, _), _| *directory != dirid);
                }
                let listing = dirs.get(&dirid).ok_or(nfsstat3::NFS3ERR_SERVERFAULT)?;
                (listing.generation, listing.continuation_token.clone())
            };
            self.advance_directory_page(dirid, dir_key, generation, continuation_token)
                .await?;
        }
    }

    fn cached_directory_view(
        &self,
        dirid: fileid3,
        dir_key: &str,
        after_name: Option<&str>,
        expected_generation: Option<u64>,
        all: bool,
    ) -> Result<Option<DirectoryView>, nfsstat3> {
        let dirs = self
            .inner
            .dirs
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        let fresh = dirs.get(&dirid).filter(|listing| {
            listing.key == dir_key && listing.fetched_at.elapsed() < DIR_CACHE_TTL
        });
        if expected_generation
            .is_some_and(|expected| fresh.is_none_or(|listing| listing.generation != expected))
        {
            return Err(nfsstat3::NFS3ERR_BAD_COOKIE);
        }
        Ok(fresh.and_then(|listing| listing.view_if_ready(after_name, all)))
    }

    pub(super) fn cached_directory_child(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> Result<Option<(Option<DirChild>, bool, u64)>, nfsstat3> {
        let dirs = self
            .inner
            .dirs
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        Ok(dirs
            .get(&dirid)
            .filter(|listing| {
                listing.key == dir_key && listing.fetched_at.elapsed() < DIR_CACHE_TTL
            })
            .map(|listing| {
                (
                    listing.child_named(name),
                    listing.complete,
                    listing.generation,
                )
            }))
    }

    /// Whether the cached listing of `dirid` has seen `name` in any page so
    /// far. A listing's generation is fixed before its pages arrive, so a
    /// cached miss recorded under it may be older than the page that later
    /// returned the name; such a name is never answered from that cache.
    pub(super) fn cached_listing_mentions(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> bool {
        // An unreadable registry cannot rule the name out.
        self.inner.dirs.read().map_or(true, |dirs| {
            dirs.get(&dirid).is_some_and(|listing| {
                listing.key == dir_key
                    && listing.fetched_at.elapsed() < DIR_CACHE_TTL
                    && listing.mentions(name)
            })
        })
    }

    async fn advance_directory_page(
        &self,
        dirid: fileid3,
        dir_key: &str,
        generation: u64,
        continuation: Option<String>,
    ) -> Result<(), nfsstat3> {
        let mut request = self
            .inner
            .client
            .list_objects_v2()
            .bucket(&self.inner.bucket)
            .delimiter("/")
            .max_keys(LIST_PAGE_SIZE);
        if !dir_key.is_empty() {
            request = request.prefix(dir_key);
        }
        if let Some(token) = &continuation {
            request = request.continuation_token(token);
        }
        // Cancellation drops this request before any listing/cursor mutation;
        // the next caller resumes from the last committed provider page.
        let scope = self.storage_scope(dir_key);
        let context =
            self.read_operation_context(OperationKind::List, &scope, "", Duration::from_secs(30));
        let response = execute_storage_operation(&context, || {
            let request = request.clone();
            async move {
                request
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))
            }
        })
        .await
        .map_err(|error| self.map_operation_error("List", error))?;
        let page = ProviderPage::parse(&response, dir_key)
            .inspect_err(|_| self.io_failed("List returned an invalid directory page".into()))?;
        let mut dirs = self
            .inner
            .dirs
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        let listing = dirs
            .get_mut(&dirid)
            .filter(|listing| listing.generation == generation && listing.key == dir_key)
            .ok_or(nfsstat3::NFS3ERR_BAD_COOKIE)?;
        if listing.continuation_token != continuation || listing.complete {
            return Err(nfsstat3::NFS3ERR_BAD_COOKIE);
        }
        // The optimization requires S3's ordered general-purpose-bucket LIST
        // contract. A backend that returns keys behind its continuation frontier
        // fails explicitly instead of silently skipping or reclassifying rows.
        if page
            .continuation
            .as_ref()
            .is_some_and(|next| listing.seen_tokens.contains(next))
            || page
                .first_key
                .as_ref()
                .zip(listing.watermark.as_ref())
                .is_some_and(|(first, previous)| first <= previous)
        {
            self.io_failed("List returned a repeated cursor or regressing key range".into());
            return Err(nfsstat3::NFS3ERR_IO);
        }
        // Acquire all fallible locks before changing the generation. No await
        // follows, so cancellation cannot leave a half-committed page/cursor.
        let mut inodes = self
            .inner
            .inodes
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        for (name, attributes) in page.entries {
            let entry = listing.pending.entry(name).or_insert(attributes);
            if attributes.0 == EntryKind::Dir {
                *entry = attributes;
            }
        }
        if page.last_key.is_some() {
            listing.watermark = page.last_key;
        }
        listing.complete = page.continuation.is_none();
        if let Some(next) = &page.continuation {
            listing.seen_tokens.insert(next.clone());
        }
        listing.continuation_token = page.continuation;
        let frontier = listing.watermark.as_deref().map(safe_name_frontier);
        let ready: Vec<_> = listing
            .pending
            .iter()
            .take_while(|(name, attributes)| {
                listing.complete
                    || frontier.is_some_and(|frontier| {
                        name.as_str() < frontier
                            || (name.as_str() == frontier && attributes.0 == EntryKind::Dir)
                    })
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in ready {
            let (kind, size, mtime) = listing.pending.remove(&name).expect("ready entry exists");
            let key = child_key(dir_key, &name, kind == EntryKind::Dir);
            let fileid = inodes.intern(&key, dirid, kind, size, mtime);
            Arc::make_mut(&mut listing.children).push(DirChild { fileid, name });
        }
        listing.fetched_at = Instant::now();
        self.io_succeeded();
        Ok(())
    }

    pub(super) async fn readdir_page(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let dir = self.dir_inode(dirid)?;
        let dir_key = normalize_dir_key(&dir.key);
        let cookie = if start_after == 0 {
            None
        } else {
            let cookies = self
                .inner
                .directory_cookies
                .read()
                .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
            let cookie = cookies
                .get(&(dirid, start_after))
                .ok_or(nfsstat3::NFS3ERR_BAD_COOKIE)?;
            let name = cookie.name.clone();
            let generation = cookie.generation;
            drop(cookies);
            let inode = self
                .inode(start_after)
                .map_err(|_| nfsstat3::NFS3ERR_BAD_COOKIE)?;
            if inode.parent != dirid || entry_name(&inode.key) != name {
                return Err(nfsstat3::NFS3ERR_BAD_COOKIE);
            }
            Some((name, generation))
        };
        let view = self
            .directory_view(
                dirid,
                &dir_key,
                cookie.as_ref().map(|(name, _)| name.as_str()),
                cookie.as_ref().map(|(_, generation)| *generation),
                false,
            )
            .await?;
        let start = resume_index(
            &view.children,
            cookie.as_ref().map(|(name, _)| name.as_str()),
        );
        let (end, exhausted) = page_end(view.children.len(), start, max_entries);
        let mut entries = Vec::with_capacity(end.saturating_sub(start));
        for child in &view.children[start..end] {
            let inode = self.inode(child.fileid)?;
            let attr = self
                .try_staged_attr(child.fileid, &inode)
                .await
                .unwrap_or_else(|| self.attr_of(child.fileid, &inode));
            entries.push(DirEntry {
                fileid: child.fileid,
                name: child.name.as_bytes().into(),
                attr,
            });
        }
        // A local namespace mutation may have landed while attributes awaited a
        // stage. Do not hand the client a cookie for that invalidated listing.
        let dirs = self
            .inner
            .dirs
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        if dirs
            .get(&dirid)
            .is_none_or(|listing| listing.generation != view.generation)
        {
            return Err(nfsstat3::NFS3ERR_BAD_COOKIE);
        }
        let mut cookies = self
            .inner
            .directory_cookies
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        for child in &view.children[start..end] {
            cookies.insert(
                (dirid, child.fileid),
                DirectoryCookie {
                    name: child.name.clone(),
                    generation: view.generation,
                },
            );
        }
        Ok(ReadDirResult {
            entries,
            end: exhausted && view.complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_frontier_holds_punctuation_until_a_later_directory_can_be_excluded() {
        assert_eq!(safe_name_frontier("apple"), "apple");
        assert_eq!(safe_name_frontier("a!x"), "a");
        assert_eq!(safe_name_frontier("a#z"), "a");
        assert_eq!(safe_name_frontier("a!/"), "a");
        assert_eq!(safe_name_frontier("a/"), "a");
        assert_eq!(safe_name_frontier("a0"), "a0");
        assert_eq!(safe_name_frontier("中文!a"), "中文");
    }
}
