use aes::cipher::{BlockDecryptMut, KeyIvInit};
use anyhow::Result;
use reqwest::Client;

use super::api::TrackSource;
use super::models::BeatportError;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// Encoded audio bytes + a hint at the container/codec extension
/// ("flac" / "aac"). The receiver decodes once via symphonia and
/// drops the `Vec<u8>` — nothing touches the disk, so there's no
/// durable cache of copyrighted content (a cleaner ToS posture).
/// Same `Downloaded` is reused for both FLAC (downloads) and HLS
/// (streams); the extension just tells symphonia which probe to use.
pub struct Downloaded {
    pub bytes: Vec<u8>,
    pub ext: &'static str,
}

pub struct StreamDownloader {
    client: Client,
}

impl StreamDownloader {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    /// Fetch track audio entirely into RAM. The pre-signed CDN URLs
    /// in `TrackSource` need no auth headers — auth happened earlier
    /// when the URL was minted. Returned bytes are typically 30-60MB
    /// (FLAC) or 5-10MB (256k AAC) and live only as long as the
    /// caller keeps them; the audio decks decode once and discard.
    pub async fn download(&self, source: &TrackSource, _track_id: i64) -> Result<Downloaded> {
        match source {
            TrackSource::Download(url) => {
                let ext = match url.path().rsplit('.').next() {
                    Some("flac") => "flac",
                    _ => "aac",
                };
                let bytes = self.download_direct(url.as_str()).await?;
                Ok(Downloaded { bytes, ext })
            }
            TrackSource::Hls(url) => {
                let bytes = self.download_hls(url.as_str()).await?;
                if bytes.len() < 10_000 {
                    return Err(BeatportError::InvalidStreamUrl.into());
                }
                Ok(Downloaded { bytes, ext: "aac" })
            }
        }
    }

    async fn download_direct(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(BeatportError::ServerError(resp.status().as_u16()).into());
        }
        let data = resp.bytes().await?;
        if data.len() < 10_000 {
            return Err(BeatportError::InvalidStreamUrl.into());
        }
        tracing::info!("Downloaded: {}KB", data.len() / 1024);
        Ok(data.to_vec())
    }

    fn download_hls<'a>(&'a self, manifest_url: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
        let manifest = self.fetch_string(manifest_url).await?;
        let lines: Vec<&str> = manifest.lines().collect();

        let base_url = &manifest_url[..manifest_url.rfind('/').unwrap_or(0) + 1];

        // Check for sub-playlist (master manifest)
        for line in &lines {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') { continue; }
            if t.contains(".m3u8") {
                let sub_url = Self::resolve_url(t, base_url);
                return self.download_hls(&sub_url).await;
            }
        }

        // Parse encryption key
        let mut key_data: Option<Vec<u8>> = None;
        let mut iv: Option<Vec<u8>> = None;

        for line in &lines {
            if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
                let attrs = Self::parse_hls_attributes(rest);
                if attrs.get("METHOD").map(|s| s.as_str()) == Some("AES-128") {
                    if let Some(key_uri) = attrs.get("URI") {
                        let key_uri = key_uri.trim_matches('"');
                        let key_url = Self::resolve_url(key_uri, base_url);
                        let key = self.fetch_bytes(&key_url).await?;
                        if key.len() == 16 {
                            key_data = Some(key);
                        }
                    }
                    if let Some(iv_hex) = attrs.get("IV") {
                        iv = Some(Self::hex_to_bytes(iv_hex));
                    }
                }
            }
        }

        // Collect segment URLs
        let segment_urls: Vec<String> = lines
            .iter()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#')
            })
            .map(|l| Self::resolve_url(l.trim(), base_url))
            .collect();

        if segment_urls.is_empty() {
            return Err(BeatportError::InvalidStreamUrl.into());
        }

        // Download and decrypt segments
        let mut all_data = Vec::new();
        for (i, seg_url) in segment_urls.iter().enumerate() {
            let mut seg_data = self.fetch_bytes(seg_url).await?;

            if let Some(ref key) = key_data {
                let seg_iv = iv.clone().unwrap_or_else(|| Self::sequence_number_iv(i));
                seg_data = Self::decrypt_aes128(&seg_data, key, &seg_iv)?;
            }

            all_data.extend_from_slice(&seg_data);
        }

        Ok(all_data)
        }) // Box::pin
    }

    // -- Network helpers --

    async fn fetch_string(&self, url: &str) -> Result<String> {
        let req = self.client.get(url)
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/");
        Ok(req.send().await?.text().await?)
    }

    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let req = self.client.get(url)
            .header("Origin", "https://dj.beatport.com")
            .header("Referer", "https://dj.beatport.com/");
        Ok(req.send().await?.bytes().await?.to_vec())
    }

    // -- HLS helpers --

    fn resolve_url(path: &str, base: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{base}{path}")
        }
    }

    fn parse_hls_attributes(s: &str) -> std::collections::HashMap<String, String> {
        let mut attrs = std::collections::HashMap::new();
        let mut remaining = s;

        while !remaining.is_empty() {
            let eq_pos = match remaining.find('=') {
                Some(p) => p,
                None => break,
            };
            let key = remaining[..eq_pos].trim().to_string();
            remaining = &remaining[eq_pos + 1..];

            let value;
            if remaining.starts_with('"') {
                remaining = &remaining[1..];
                if let Some(close) = remaining.find('"') {
                    value = remaining[..close].to_string();
                    remaining = &remaining[close + 1..];
                } else {
                    value = remaining.to_string();
                    remaining = "";
                }
            } else if let Some(comma) = remaining.find(',') {
                value = remaining[..comma].to_string();
                remaining = &remaining[comma + 1..];
            } else {
                value = remaining.to_string();
                remaining = "";
            }

            attrs.insert(key, value);
            if remaining.starts_with(',') {
                remaining = &remaining[1..];
            }
        }

        attrs
    }

    // -- Crypto --

    fn decrypt_aes128(data: &[u8], key: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
        if key.len() != 16 || iv.len() != 16 {
            return Err(BeatportError::DecryptionFailed("invalid key/iv size".into()).into());
        }

        let mut buf = data.to_vec();
        // Pad to block size if needed
        let block_size = 16;
        let padded_len = buf.len().div_ceil(block_size) * block_size;
        buf.resize(padded_len, 0);

        let decryptor = Aes128CbcDec::new_from_slices(key, iv)
            .map_err(|e| BeatportError::DecryptionFailed(e.to_string()))?;

        let decrypted = decryptor
            .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
            .map_err(|e| BeatportError::DecryptionFailed(e.to_string()))?;

        Ok(decrypted.to_vec())
    }

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        let hex = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X")).unwrap_or(hex);
        let hex = if !hex.len().is_multiple_of(2) {
            format!("0{hex}")
        } else {
            hex.to_string()
        };

        (0..hex.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect()
    }

    fn sequence_number_iv(seq_num: usize) -> Vec<u8> {
        let mut iv = vec![0u8; 16];
        let mut n = seq_num;
        for i in (0..16).rev() {
            iv[i] = (n & 0xFF) as u8;
            n >>= 8;
        }
        iv
    }
}
