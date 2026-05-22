use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;

use super::auth::StoredAuth;
use super::models::*;
use crate::config::AudioQuality;

const BASE_URL: &str = "https://api.beatport.com/v4";

/// Trending genre IDs (matches Beatport DJ website order).
const TRENDING_GENRE_IDS: &[i64] = &[
    11, 5, 6, 90, 1, 39, 12, 89, 14, 15, 92, 37, 7, 93, 2, 96, 81, 3, 50, 91,
];

pub struct BeatportAPI {
    /// OAuth PKCE access_token (+ refresh) loaded from ~/.mixr/auth.json.
    /// Sent as `Authorization: Bearer <token>` on every request.
    auth: StoredAuth,
    /// HTTP client — single instance, reused for connection pooling.
    client: Client,
    /// In-memory response cache keyed by `path?key=val&...`. Populated
    /// by the shared `request` chokepoint; every API method above
    /// (search, genres, top-100 variants, genre charts, …) flows
    /// through it, so this cache covers the whole surface without
    /// per-method plumbing. TTL defined by `BROWSE_CACHE_TTL`.
    browse_cache: std::collections::HashMap<String, (Arc<Value>, std::time::Instant)>,
}

/// How long a cached Beatport response stays fresh. Charts and genre
/// lists don't change minute-to-minute; one hour is comfortable for
/// a DJ session and keeps the session's second-pass browses
/// essentially free. Does not affect authenticated writes (playlists,
/// follow/unfollow — those skip the cache).
pub const BROWSE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl BeatportAPI {
    pub fn new(auth: StoredAuth) -> Self {
        Self {
            auth,
            client: Client::builder().build().unwrap(),
            browse_cache: std::collections::HashMap::new(),
        }
    }

    // -- Search --

    pub async fn search(&mut self, query: &str) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/search/", &[
            ("q", query),
            ("type", "tracks"),
            ("per_page", "20"),
        ]).await?;
        Ok(Self::parse_search_tracks(&data))
    }

    // -- Genres --

    pub async fn genres(&mut self) -> Result<Vec<BeatportGenre>> {
        let data = self.request("catalog/genres/", &[]).await?;
        Ok(Self::parse_genres(&data))
    }

    pub async fn trending_genres(&mut self) -> Result<Vec<BeatportGenre>> {
        let all = self.genres().await?;
        Ok(TRENDING_GENRE_IDS
            .iter()
            .filter_map(|id| all.iter().find(|g| g.id == *id).cloned())
            .collect())
    }

    // -- Charts --

    pub async fn global_top_100(&mut self) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/tracks/top/100/", &[
            ("enabled", "true"),
            ("is_hype", "false"),
            ("per_page", "100"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn genre_top_100(&mut self, genre_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request(&format!("catalog/genres/{genre_id}/top/100/"), &[
            ("per_page", "100"),
            ("hype", "false"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn genre_tracks(&mut self, genre_id: i64) -> Result<Vec<BeatportTrack>> {
        self.genre_tracks_page(genre_id, 1).await
    }

    pub async fn genre_tracks_page(&mut self, genre_id: i64, page: u32) -> Result<Vec<BeatportTrack>> {
        let gid = genre_id.to_string();
        let pg = page.to_string();
        let data = self.request("catalog/tracks/", &[
            ("genre_id", gid.as_str()),
            ("per_page", "100"),
            ("preorder", "false"),
            ("order_by", "-publish_date"),
            ("page", pg.as_str()),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn chart_tracks(&mut self, chart_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request(&format!("catalog/charts/{chart_id}/tracks/"), &[]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn genre_charts(&mut self, genre_id: i64) -> Result<Vec<BeatportChart>> {
        self.genre_charts_page(genre_id, 1).await
    }

    pub async fn genre_charts_page(&mut self, genre_id: i64, page: u32) -> Result<Vec<BeatportChart>> {
        let gid = genre_id.to_string();
        let pg = page.to_string();
        let data = self.request("catalog/charts/", &[
            ("genre_id", &gid),
            ("per_page", "100"),
            ("page", &pg),
        ]).await?;
        Ok(Self::parse_charts(&data))
    }

    pub async fn artist_top_100(&mut self, artist_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request(&format!("catalog/artists/{artist_id}/top/100/"), &[
            ("per_page", "100"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn label_top_100(&mut self, label_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request(&format!("catalog/labels/{label_id}/top/100/"), &[
            ("per_page", "100"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn release_tracks(&mut self, release_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request(&format!("catalog/releases/{release_id}/tracks/"), &[]).await?;
        Ok(Self::parse_tracks(&data))
    }

    // -- Hype --

    pub async fn hype_top_100(&mut self) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/tracks/top/100/", &[
            ("enabled", "true"),
            ("hype", "true"),
            ("per_page", "100"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn genre_hype(&mut self, genre_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/tracks/", &[
            ("genre_id", &genre_id.to_string()),
            ("is_hype", "true"),
            ("per_page", "100"),
            ("preorder", "false"),
            ("order_by", "-publish_date"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn genre_exclusives(&mut self, genre_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/tracks/", &[
            ("genre_id", &genre_id.to_string()),
            ("was_ever_exclusive", "true"),
            ("per_page", "100"),
            ("preorder", "false"),
            ("order_by", "-publish_date"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    // -- Releases --

    pub async fn genre_releases(&mut self, genre_id: i64) -> Result<Vec<BeatportRelease>> {
        let data = self.request("catalog/releases/", &[
            ("genre_id", &genre_id.to_string()),
            ("enabled", "true"),
            ("preorder", "false"),
            ("order_by", "-publish_date"),
            ("per_page", "100"),
        ]).await?;
        Ok(Self::parse_releases(&data))
    }

    pub async fn artist_tracks(&mut self, artist_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/tracks/", &[
            ("artist_id", &artist_id.to_string()),
            ("per_page", "100"),
            ("order_by", "-publish_date"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn artist_releases(&mut self, artist_id: i64) -> Result<Vec<BeatportRelease>> {
        let data = self.request("catalog/releases/", &[
            ("artist_id", &artist_id.to_string()),
            ("per_page", "100"),
            ("order_by", "-publish_date"),
        ]).await?;
        Ok(Self::parse_releases(&data))
    }

    pub async fn label_tracks(&mut self, label_id: i64) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/tracks/", &[
            ("label_id", &label_id.to_string()),
            ("per_page", "100"),
            ("order_by", "-publish_date"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn label_releases(&mut self, label_id: i64) -> Result<Vec<BeatportRelease>> {
        let data = self.request("catalog/releases/", &[
            ("label_id", &label_id.to_string()),
            ("per_page", "100"),
            ("order_by", "-publish_date"),
        ]).await?;
        Ok(Self::parse_releases(&data))
    }

    // -- Trending --

    pub async fn trending_artists(&mut self, genre_id: Option<i64>) -> Result<Vec<BeatportArtist>> {
        let gid_str = genre_id.map(|g| g.to_string());
        let mut params = vec![
            ("sort", "trending"),
            ("per_page", "100"),
        ];
        if let Some(ref gid) = gid_str { params.push(("genre_id", gid)); }
        let data = self.request("catalog/artists/", &params).await?;
        Ok(Self::parse_artists(&data))
    }

    pub async fn trending_labels(&mut self, genre_id: Option<i64>) -> Result<Vec<BeatportLabel>> {
        let gid_str = genre_id.map(|g| g.to_string());
        let mut params = vec![
            ("sort", "trending"),
            ("per_page", "100"),
        ];
        if let Some(ref gid) = gid_str { params.push(("genre_id", gid)); }
        let data = self.request("catalog/labels/", &params).await?;
        Ok(Self::parse_labels(&data))
    }

    // -- Decades --

    pub async fn tracks_by_date_range(&mut self, range: &str, genre_id: Option<i64>) -> Result<Vec<BeatportTrack>> {
        let gid_str = genre_id.map(|g| g.to_string());
        let mut params = vec![
            ("publish_date", range),
            ("per_page", "100"),
            ("preorder", "false"),
            ("order_by", "-publish_date"),
        ];
        if let Some(ref gid) = gid_str { params.push(("genre_id", gid)); }
        let data = self.request("catalog/tracks/", &params).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn releases_by_date_range(&mut self, range: &str, genre_id: Option<i64>) -> Result<Vec<BeatportRelease>> {
        let gid_str = genre_id.map(|g| g.to_string());
        let mut params = vec![
            ("publish_date", range),
            ("enabled", "true"),
            ("preorder", "false"),
            ("order_by", "-publish_date"),
            ("per_page", "100"),
        ];
        if let Some(ref gid) = gid_str { params.push(("genre_id", gid)); }
        let data = self.request("catalog/releases/", &params).await?;
        Ok(Self::parse_releases(&data))
    }

    pub async fn charts_by_date_range(&mut self, range: &str, genre_id: Option<i64>) -> Result<Vec<BeatportChart>> {
        let gid_str = genre_id.map(|g| g.to_string());
        let mut params = vec![
            ("publish_date", range),
            ("per_page", "50"),
        ];
        if let Some(ref gid) = gid_str { params.push(("genre_id", gid)); }
        let data = self.request("catalog/charts/", &params).await?;
        Ok(Self::parse_charts(&data))
    }

    // -- My Beatport --

    pub async fn my_tracks(&mut self) -> Result<Vec<BeatportTrack>> {
        let data = self.request("my/beatport/tracks/", &[
            ("preorder", "false"),
            ("per_page", "100"),
            ("order_by", "-publish_date"),
        ]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn my_artists(&mut self) -> Result<Vec<BeatportArtist>> {
        let data = self.request("my/beatport/artists/", &[("per_page", "100")]).await?;
        Ok(Self::parse_artists(&data))
    }

    pub async fn my_labels(&mut self) -> Result<Vec<BeatportLabel>> {
        let data = self.request("my/beatport/labels/", &[("per_page", "100")]).await?;
        Ok(Self::parse_labels(&data))
    }

    pub async fn recommendations(&mut self) -> Result<Vec<BeatportTrack>> {
        let data = self.request("catalog/v1/recommendations/user/", &[("per_page", "100")]).await?;
        Ok(Self::parse_tracks(&data))
    }

    // -- My Library --

    pub async fn my_downloads(&mut self) -> Result<Vec<BeatportTrack>> {
        let data = self.request("my/downloads/", &[("per_page", "100")]).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn my_cart(&mut self) -> Result<Vec<BeatportTrack>> {
        let data = self.request("my/default-cart/", &[]).await?;
        // Cart returns tracks in a different format
        if let Some(tracks) = data["tracks"].as_array() {
            Ok(tracks.iter().filter_map(Self::parse_track).collect())
        } else {
            Ok(Self::parse_tracks(&data))
        }
    }

    /// Add a track to the user's default Beatport cart. Returns Ok on
    /// 2xx (including 200/201/204), error on other statuses. Bypasses
    /// the read-cache since cart is mutable user state.
    pub async fn add_to_cart(&mut self, track_id: i64) -> Result<()> {
        let url = format!("{BASE_URL}/my/default-cart/items/");
        let body = serde_json::json!({"track_id": track_id});
        let mut req = self.client.post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/")
            .json(&body);
        if let Some(ref token) = self.auth.access_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        if !(200..=299).contains(&status) {
            anyhow::bail!("add to cart failed: HTTP {status}");
        }
        Ok(())
    }

    /// Remove a single track from the user's cart by its cart item id.
    /// (Beatport tracks cart items by their own item id, not track_id —
    /// callers usually loop my_cart() to find the matching item id.)
    /// Currently exposed via API only; no UI keybind yet — the cart
    /// view (when added) will use this for item-row deletion.
    #[allow(dead_code)]
    pub async fn remove_from_cart(&mut self, item_id: i64) -> Result<()> {
        let url = format!("{BASE_URL}/my/default-cart/items/{item_id}/");
        let mut req = self.client.delete(&url)
            .header("Accept", "application/json")
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/");
        if let Some(ref token) = self.auth.access_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        // 204 = success, 404 = already gone (treat as success).
        if status == 204 || status == 404 { return Ok(()); }
        if (200..=299).contains(&status) { return Ok(()); }
        anyhow::bail!("remove from cart failed: HTTP {status}")
    }

    pub async fn my_playlists(&mut self) -> Result<Vec<BeatportChart>> {
        // Playlists use the same structure as charts
        let data = self.request("my/playlists/", &[]).await?;
        Ok(Self::parse_charts(&data))
    }

    // -- Pagination helper --

    pub async fn paginated_tracks(&mut self, base_params: &[(&str, &str)], page: u32) -> Result<Vec<BeatportTrack>> {
        let pg = page.to_string();
        let mut params: Vec<(&str, &str)> = base_params.to_vec();
        params.push(("per_page", "100"));
        params.push(("page", &pg));
        let data = self.request("catalog/tracks/", &params).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn tracks_by_date_range_page(&mut self, range: &str, genre_id: Option<i64>, page: u32) -> Result<Vec<BeatportTrack>> {
        let gid_str = genre_id.map(|g| g.to_string());
        let pg = page.to_string();
        let mut params = vec![
            ("publish_date", range),
            ("per_page", "100"),
            ("preorder", "false"),
            ("order_by", "-publish_date"),
            ("page", &pg),
        ];
        if let Some(ref gid) = gid_str { params.push(("genre_id", gid)); }
        let data = self.request("catalog/tracks/", &params).await?;
        Ok(Self::parse_tracks(&data))
    }

    pub async fn paginated_charts(&mut self, params: &[(&str, &str)]) -> Result<Vec<BeatportChart>> {
        let data = self.request("catalog/charts/", params).await?;
        Ok(Self::parse_charts(&data))
    }

    pub async fn paginated_releases(&mut self, base_params: &[(&str, &str)], page: u32) -> Result<Vec<BeatportRelease>> {
        let pg = page.to_string();
        let mut params: Vec<(&str, &str)> = base_params.to_vec();
        params.push(("per_page", "100"));
        params.push(("page", &pg));
        let data = self.request("catalog/releases/", &params).await?;
        Ok(Self::parse_releases(&data))
    }

    // -- Follow / Unfollow --

    pub async fn follow_artist(&mut self, artist_id: i64) -> Result<()> {
        self.my_beatport_action("POST", &serde_json::json!({"artist_ids": [artist_id]})).await
    }

    pub async fn unfollow_artist(&mut self, artist_id: i64) -> Result<()> {
        self.my_beatport_action("DELETE", &serde_json::json!({"artist_ids": [artist_id]})).await
    }

    pub async fn follow_label(&mut self, label_id: i64) -> Result<()> {
        self.my_beatport_action("POST", &serde_json::json!({"label_ids": [label_id]})).await
    }

    pub async fn unfollow_label(&mut self, label_id: i64) -> Result<()> {
        self.my_beatport_action("DELETE", &serde_json::json!({"label_ids": [label_id]})).await
    }

    async fn my_beatport_action(&mut self, method: &str, body: &Value) -> Result<()> {
        let url = format!("{BASE_URL}/my/beatport/");
        let mut req = match method {
            "DELETE" => self.client.delete(&url),
            _ => self.client.post(&url),
        };
        req = req
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/")
            .json(body);
        if let Some(ref token) = self.auth.access_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        if !(200..=299).contains(&status) {
            anyhow::bail!("my/beatport action failed: HTTP {status}");
        }
        Ok(())
    }

    // -- Playlists (write) --

    pub async fn create_playlist(&mut self, name: &str) -> Result<i64> {
        let url = format!("{BASE_URL}/my/playlists/");
        let body = serde_json::json!({"name": name, "is_public": false});
        let mut req = self.client.post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/")
            .json(&body);
        if let Some(ref token) = self.auth.access_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await?;
        let data: Value = resp.json().await?;
        data["id"].as_i64().ok_or_else(|| anyhow::anyhow!("no playlist id in response"))
    }

    /// Delete a playlist by id. The Beatport Web API accepts
    /// `DELETE /my/playlists/{id}/` (same path as GET on a single
    /// playlist) and returns 204 on success, 404 if the id no longer
    /// exists. Used by the settings/rules flows for tearing down
    /// playlists the user opted to remove, and by the test harness
    /// for cleanup.
    pub async fn delete_playlist(&mut self, playlist_id: i64) -> Result<()> {
        let url = format!("{BASE_URL}/my/playlists/{playlist_id}/");
        let mut req = self.client.delete(&url)
            .header("Accept", "application/json")
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/");
        if let Some(ref token) = self.auth.access_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        // 204 = success, 404 = already gone (treat as success so
        // cleanup is idempotent).
        if status == 204 || status == 404 { return Ok(()); }
        if (200..=299).contains(&status) { return Ok(()); }
        anyhow::bail!("delete playlist failed: HTTP {status}")
    }

    pub async fn add_to_playlist(&mut self, playlist_id: i64, track_ids: &[i64]) -> Result<()> {
        let url = format!("{BASE_URL}/my/playlists/{playlist_id}/tracks/");
        let body = serde_json::json!({"track_ids": track_ids});
        let mut req = self.client.post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/")
            .json(&body);
        if let Some(ref token) = self.auth.access_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        if !(200..=299).contains(&status) {
            anyhow::bail!("add to playlist failed: HTTP {status}");
        }
        Ok(())
    }

    // -- Editorial --

    pub async fn editorial_playlists(&mut self, genre_id: i64) -> Result<Vec<BeatportChart>> {
        let data = self.request("catalog/charts/", &[
            ("enabled", "true"),
            ("is_published", "true"),
            ("per_page", "100"),
            ("dj_id", "36047"),
            ("genre_id", &genre_id.to_string()),
        ]).await?;
        Ok(Self::parse_charts(&data))
    }

    // -- Streaming --

    pub async fn get_track_source(&mut self, track_id: i64, quality: AudioQuality) -> Result<TrackSource> {
        // The /download/ endpoint (lossless FLAC + 256k AAC direct
        // download) requires partner-grade OAuth scope that the
        // dj.beatport.com web app's PKCE token doesn't have — every
        // attempt returns 403 on main. We skip it entirely and go
        // straight to /stream/, which returns an m3u8 manifest the
        // web app's scope DOES allow. Audio quality on main caps at
        // 256k AAC via this path.
        let _ = quality; // kept for ABI parity with branches that have FLAC scope
        let data = self.request(&format!("catalog/tracks/{track_id}/stream/"), &[]).await?;
        let mut stream_url = data["stream_url"]
            .as_str()
            .ok_or(BeatportError::InvalidStreamUrl)?
            .to_string();

        if quality != AudioQuality::Standard {
            stream_url = stream_url.replace(".128k.aac", ".256k.aac");
        }

        let url = url::Url::parse(&stream_url).map_err(|_| BeatportError::InvalidStreamUrl)?;
        tracing::info!(
            "{}k HLS source for track {track_id}",
            if stream_url.contains("256k") { "256" } else { "128" }
        );
        Ok(TrackSource::Hls(url))
    }

    // -- HTTP --

    async fn request(&mut self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        // Cache lookup. Key = path + stable-sorted query string so the
        // same call in different param orders still hits. Stale entries
        // are evicted lazily on access — no background task needed.
        let key = Self::cache_key(path, params);
        if let Some((value, at)) = self.browse_cache.get(&key)
            && at.elapsed() < BROWSE_CACHE_TTL {
                tracing::debug!("Beatport cache HIT: {path} (age {:.0}s)",
                    at.elapsed().as_secs_f64());
                return Ok(Value::clone(value));
            }
        let value = self.request_inner(path, params, false).await?;
        self.browse_cache.insert(key, (Arc::new(value.clone()), std::time::Instant::now()));
        Ok(value)
    }

    /// Stable cache key for a Beatport GET. Params are sorted so
    /// equivalent calls with reordered args land on the same entry.
    fn cache_key(path: &str, params: &[(&str, &str)]) -> String {
        let mut pairs: Vec<(&str, &str)> = params.iter().map(|&(k, v)| (k, v)).collect();
        pairs.sort_by_key(|&(k, _)| k);
        let q: String = pairs.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        if q.is_empty() { path.to_string() } else { format!("{path}?{q}") }
    }

    /// Clear all cached responses. Called on logout / credential change
    /// so stale authenticated views (My Library, Favorites) don't leak
    /// across accounts.
    pub fn clear_browse_cache(&mut self) {
        let n = self.browse_cache.len();
        self.browse_cache.clear();
        if n > 0 {
            tracing::info!("Cleared {n} cached Beatport entries");
        }
    }

    /// Count of currently-cached entries (includes stale pending lazy
    /// eviction). Exposed for diagnostics and smoke tests.
    #[allow(dead_code)] // used in #[cfg(test)] assertions only
    pub fn browse_cache_len(&self) -> usize {
        self.browse_cache.len()
    }

    fn request_inner<'a>(&'a mut self, path: &'a str, params: &'a [(&'a str, &'a str)], retried: bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let url = format!("{BASE_URL}/{path}");
            let mut req = self.client.get(&url);
            if !params.is_empty() {
                req = req.query(params);
            }
            req = req
                .header("Accept", "application/json")
                .header("Origin", "https://dj.beatport.com")
                .header("Referer", "https://dj.beatport.com/");
            if let Some(ref token) = self.auth.access_token {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            match status {
                200..=299 => Ok(resp.json().await?),
                401 | 403 => {
                    // Token may have expired between launches. Try
                    // refresh once if we have a refresh_token + cached
                    // client_id (refresh needs both).
                    if !retried
                        && let (Some(rt), Some(cid)) = (
                            self.auth.refresh_token.clone(),
                            self.auth.client_id.clone(),
                        ) {
                            tracing::info!("Got {status}, refreshing token…");
                            match super::auth::refresh(&rt, &cid).await {
                                Ok(new_auth) => {
                                    self.auth = new_auth;
                                    return self.request_inner(path, params, true).await;
                                }
                                Err(e) => tracing::warn!("Refresh failed: {e}"),
                            }
                        }
                    if status == 401 { Err(BeatportError::Unauthorized.into()) }
                    else { Err(BeatportError::Forbidden.into()) }
                }
                404 => Err(BeatportError::NotFound.into()),
                _ => Err(BeatportError::ServerError(status).into()),
            }
        })
    }

    // -- Parsing --

    fn parse_results(data: &Value) -> Vec<&Value> {
        if let Some(results) = data["results"].as_array() {
            results.iter().collect()
        } else if let Some(arr) = data.as_array() {
            arr.iter().collect()
        } else {
            Vec::new()
        }
    }

    fn parse_tracks(data: &Value) -> Vec<BeatportTrack> {
        Self::parse_results(data)
            .into_iter()
            .filter_map(Self::parse_track)
            .collect()
    }

    fn parse_search_tracks(data: &Value) -> Vec<BeatportTrack> {
        // Search endpoint returns { tracks: [...] } or { results: { tracks: [...] } }
        if let Some(tracks) = data["tracks"].as_array() {
            return tracks.iter().filter_map(Self::parse_track).collect();
        }
        if let Some(tracks) = data["results"]["tracks"].as_array() {
            return tracks.iter().filter_map(Self::parse_track).collect();
        }
        Self::parse_tracks(data)
    }

    fn parse_track(json: &Value) -> Option<BeatportTrack> {
        let id = json["id"].as_i64()?;
        let title = json["name"].as_str()
            .or_else(|| json["title"].as_str())?
            .to_string();

        let artists: Vec<BeatportTrackArtist> = json["artists"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        Some(BeatportTrackArtist {
                            id: a["id"].as_i64()?,
                            name: a["name"].as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let artists = if artists.is_empty() {
            vec![BeatportTrackArtist { id: 0, name: "Unknown Artist".into() }]
        } else {
            artists
        };

        // Key parsing
        let key = if let Some(key_obj) = json["key"].as_object() {
            if let (Some(cn), Some(cl)) = (key_obj.get("camelot_number").and_then(|v| v.as_i64()), key_obj.get("camelot_letter").and_then(|v| v.as_str())) {
                Some(format!("{cn}{cl}"))
            } else {
                key_obj.get("name").and_then(|v| v.as_str()).map(String::from)
            }
        } else {
            json["key"].as_str().map(String::from)
        };

        // Duration
        let duration = json["length_ms"]
            .as_i64()
            .map(|ms| ms as f64 / 1000.0)
            .or_else(|| json["length"].as_f64());

        // Label
        let label_id = json["label"]["id"].as_i64()
            .or_else(|| json["release"]["label"]["id"].as_i64());
        let label_name = json["label"]["name"].as_str()
            .or_else(|| json["release"]["label"]["name"].as_str())
            .map(String::from);

        // Genre
        let genre_id = json["genre"]["id"].as_i64();
        let genre_name = json["genre"]["name"].as_str().map(String::from);
        let genre_slug = json["genre"]["slug"].as_str().map(String::from);

        // Release
        let release_id = json["release"]["id"].as_i64();
        let release_date = json["publish_date"].as_str()
            .or_else(|| json["new_release_date"].as_str())
            .map(|s| s[..10.min(s.len())].to_string());

        // Remixers
        let remixers: Vec<BeatportTrackArtist> = json["remixers"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        Some(BeatportTrackArtist {
                            id: r["id"].as_i64()?,
                            name: r["name"].as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(BeatportTrack {
            id,
            title,
            mix_name: json["mix_name"].as_str().map(String::from),
            artists,
            bpm: json["bpm"].as_f64(),
            key,
            duration,
            label_id,
            label_name,
            genre_id,
            genre_name,
            genre_slug,
            release_id,
            release_date,
            remixers,
            local_path: None,
        })
    }

    fn parse_genres(data: &Value) -> Vec<BeatportGenre> {
        Self::parse_results(data)
            .into_iter()
            .filter_map(|v| {
                Some(BeatportGenre {
                    id: v["id"].as_i64()?,
                    name: v["name"].as_str()?.to_string(),
                    slug: v["slug"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect()
    }

    fn parse_charts(data: &Value) -> Vec<BeatportChart> {
        Self::parse_results(data)
            .into_iter()
            .filter_map(|v| {
                let owner_name = v["person"]["owner_name"].as_str()
                    .or_else(|| v["person"]["display_name"].as_str())
                    .or_else(|| v["person"]["name"].as_str())
                    .or_else(|| v["owner"]["display_name"].as_str())
                    .or_else(|| v["owner"]["name"].as_str())
                    .or_else(|| v["dj_name"].as_str())
                    .map(String::from);

                Some(BeatportChart {
                    id: v["id"].as_i64()?,
                    name: v["name"].as_str()?.to_string(),
                    owner_name,
                    track_count: v["track_count"].as_i64(),
                })
            })
            .collect()
    }

    fn parse_artists(data: &Value) -> Vec<BeatportArtist> {
        Self::parse_results(data)
            .into_iter()
            .filter_map(|v| {
                Some(BeatportArtist {
                    id: v["id"].as_i64()?,
                    name: v["name"].as_str()?.to_string(),
                })
            })
            .collect()
    }

    fn parse_labels(data: &Value) -> Vec<BeatportLabel> {
        Self::parse_results(data)
            .into_iter()
            .filter_map(|v| {
                Some(BeatportLabel {
                    id: v["id"].as_i64()?,
                    name: v["name"].as_str()?.to_string(),
                })
            })
            .collect()
    }

    fn parse_releases(data: &Value) -> Vec<BeatportRelease> {
        Self::parse_results(data)
            .into_iter()
            .filter_map(|v| {
                let id = v["id"].as_i64()?;
                let name = v["name"].as_str()?.to_string();
                let artist_name = v["artists"].as_array()
                    .map(|arr| arr.iter()
                        .filter_map(|a| a["name"].as_str())
                        .filter(|n| !n.is_empty())
                        .collect::<Vec<_>>()
                        .join(", "))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default();
                let label_name = v["label"]["name"].as_str().map(String::from);
                let track_count = v["track_count"].as_i64();
                let release_date = v["publish_date"].as_str()
                    .or(v["new_release_date"].as_str())
                    .map(|s| s[..10.min(s.len())].to_string());
                Some(BeatportRelease { id, name, artist_name, label_name, track_count, release_date })
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum TrackSource {
    /// Pre-signed CDN URL for a complete audio file (FLAC or 256k AAC).
    /// Unreachable on main's auth scope (web app PKCE token doesn't
    /// have /download/ permission); kept on the type so branches with
    /// partner-grade scope can produce it via `get_track_source`.
    #[allow(dead_code)]
    Download(url::Url),
    Hls(url::Url),
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_stable_across_param_order() {
        // Same effective call, different param order → same key.
        // Otherwise a call like (a=1,b=2) and (b=2,a=1) would cache
        // separately and defeat the point.
        let k1 = BeatportAPI::cache_key("catalog/top-100", &[("genre_id", "5"), ("per_page", "20")]);
        let k2 = BeatportAPI::cache_key("catalog/top-100", &[("per_page", "20"), ("genre_id", "5")]);
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_empty_params_omits_query() {
        let k = BeatportAPI::cache_key("genres", &[]);
        assert_eq!(k, "genres", "no params → no trailing ?");
    }

    #[test]
    fn cache_key_includes_both_params() {
        let k = BeatportAPI::cache_key("search", &[("q", "artbat")]);
        assert!(k.contains("q=artbat"));
        assert!(k.starts_with("search?"));
    }

    #[test]
    fn browse_cache_ttl_is_sane() {
        // 60 min: long enough to cover a full DJ session without
        // refetching charts, short enough that new weekly releases
        // eventually surface. Regression guard so the constant isn't
        // silently bumped to something absurd.
        let secs = BROWSE_CACHE_TTL.as_secs();
        assert!(secs >= 30 * 60, "TTL must be at least 30 min for session coverage, got {secs}s");
        assert!(secs <= 4 * 60 * 60, "TTL above 4h would miss daily chart updates, got {secs}s");
    }

    #[test]
    fn cache_lookup_returns_fresh_entry() {
        // A read on a freshly-inserted entry must hit (elapsed < TTL).
        // Mirrors the lookup branch in request() without spinning up a
        // WebView subprocess: we test the HashMap + TTL filter in
        // isolation, which is what request() also does internally.
        let mut cache: std::collections::HashMap<String, (Arc<Value>, std::time::Instant)>
            = std::collections::HashMap::new();
        let key = "test/path".to_string();
        cache.insert(key.clone(),
            (Arc::new(serde_json::json!({"ok": true})), std::time::Instant::now()));
        let hit = cache.get(&key)
            .filter(|(_, at)| at.elapsed() < BROWSE_CACHE_TTL)
            .map(|(v, _)| v.clone());
        assert!(hit.is_some(), "fresh entry must hit the cache");
        assert_eq!(hit.unwrap()["ok"], true);
    }

    #[test]
    fn cache_lookup_misses_on_expired_entry() {
        // An entry whose age exceeds TTL must miss.
        let mut cache: std::collections::HashMap<String, (Arc<Value>, std::time::Instant)>
            = std::collections::HashMap::new();
        let key = "test/expired".to_string();
        let stale = std::time::Instant::now()
            .checked_sub(BROWSE_CACHE_TTL + std::time::Duration::from_secs(60))
            .expect("Instant arithmetic should land in the past");
        cache.insert(key.clone(), (Arc::new(serde_json::json!({})), stale));
        let hit = cache.get(&key)
            .filter(|(_, at)| at.elapsed() < BROWSE_CACHE_TTL);
        assert!(hit.is_none(), "stale entry past TTL must miss");
    }

    #[test]
    fn clear_browse_cache_empties_and_is_idempotent() {
        let mut api = BeatportAPI::new(super::super::auth::StoredAuth::default());
        api.browse_cache.insert("x".into(),
            (Arc::new(serde_json::Value::Null), std::time::Instant::now()));
        api.browse_cache.insert("y".into(),
            (Arc::new(serde_json::Value::Null), std::time::Instant::now()));
        assert_eq!(api.browse_cache_len(), 2);
        api.clear_browse_cache();
        assert_eq!(api.browse_cache_len(), 0);
        api.clear_browse_cache();
        assert_eq!(api.browse_cache_len(), 0);
    }
}
