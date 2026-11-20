
use std::collections::HashMap;

use anyhow::Result;
use futures::{StreamExt, TryStreamExt, stream};
use librespot::{
    core::{SpotifyUri, session::Session},
    metadata::{self as meta, Metadata},
    protocol::{
        extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery},
        extension_kind::ExtensionKind,
        metadata::Track as TrackMessage,
    },
};
use protobuf::{EnumOrUnknown, Message};
use reqwest::Method;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use tunebox_core::catalog::{AlbumInfo, ArtistInfo, Page, PlaylistInfo, SearchResults, ShowInfo, Track, User};

use crate::webapi::WebApi;

pub const PAGE: u32 = 50;
pub const PAGE_CONCURRENCY: usize = 4;
pub const LIBRARY_BATCH: usize = 40;
const IMAGE_WIDTH: u32 = 300;
pub const METADATA_BATCH: usize = 100;
const METADATA_CONCURRENCY: usize = 8;

pub async fn me(api: &WebApi) -> Result<User> {
    let raw: RawUser = api.get_json("/me").await?;
    Ok(User { name: raw.display_name.unwrap_or_else(|| raw.id.clone()), id: raw.id, product: raw.product })
}

pub async fn playlists(api: &WebApi) -> Result<Vec<PlaylistInfo>> {
    all_pages(api, "/me/playlists", |raw: Option<RawPlaylist>| raw.map(PlaylistInfo::from)).await
}

pub async fn saved_albums(api: &WebApi) -> Result<Vec<AlbumInfo>> {
    all_pages(api, "/me/albums", |saved: RawSavedAlbum| Some(AlbumInfo::from(saved.album))).await
}

pub async fn followed_artists(api: &WebApi) -> Result<Vec<ArtistInfo>> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let cursor = after.as_deref().map(|a| format!("&after={a}")).unwrap_or_default();
        let raw: RawFollowing = api.get_json(&format!("/me/following?type=artist&limit={PAGE}{cursor}")).await?;
        all.extend(raw.artists.items.into_iter().map(ArtistInfo::from));
        after = raw.artists.cursors.and_then(|c| c.after);
        if after.is_none() || raw.artists.next.is_none() {
            return Ok(all);
        }
    }
}

pub async fn saved_shows(api: &WebApi) -> Result<Vec<ShowInfo>> {
    all_pages(api, "/me/shows", |saved: RawSavedShow| {
        Some(ShowInfo { uri: saved.show.uri, name: saved.show.name, image_url: pick_image(&saved.show.images) })
    })
    .await
}

async fn all_pages<R, T>(api: &WebApi, path: &str, map: impl Fn(R) -> Option<T>) -> Result<Vec<T>>
where
    R: DeserializeOwned + Send,
{
    let page = |offset: u32| format!("{path}?limit={PAGE}&offset={offset}");
    let first: RawPage<R> = api.get_json(&page(0)).await?;
    let offsets: Vec<u32> = match next_offset(&first, 0) {
        Some(next) => (next..first.total).step_by(PAGE as usize).collect(),
        None => Vec::new(),
    };
    let rest: Vec<RawPage<R>> = stream::iter(offsets)
        .map(|offset| {
            let path = page(offset);
            async move { api.get_json(&path).await }
        })
        .buffered(PAGE_CONCURRENCY)
        .try_collect()
        .await?;
    Ok(std::iter::once(first).chain(rest).flat_map(|page| page.items).filter_map(map).collect())
}

pub async fn liked_count(api: &WebApi) -> Result<u32> {
    let page: RawPage<RawSavedTrack> = api.get_json("/me/tracks?limit=1").await?;
    Ok(page.total)
}

pub async fn liked_tracks(api: &WebApi, offset: u32) -> Result<Page<Track>> {
    let page: RawPage<RawSavedTrack> = api.get_json(&format!("/me/tracks?limit={PAGE}&offset={offset}")).await?;
    let next_offset = next_offset(&page, offset);
    Ok(Page {
        items: page.items.into_iter().filter_map(|i| i.track).map(Track::from).collect(),
        total: page.total,
        next_offset,
    })
}

pub async fn playlist_tracks(api: &WebApi, id: &str, offset: u32) -> Result<Page<Track>> {
    let page: RawPage<RawPlaylistItem> =
        api.get_json(&format!("/playlists/{id}/items?limit={PAGE}&offset={offset}")).await?;
    let next_offset = next_offset(&page, offset);
    Ok(Page {
        items: page.items.into_iter().filter_map(|i| i.item).map(Track::from).collect(),
        total: page.total,
        next_offset,
    })
}

pub async fn album_tracks(api: &WebApi, album: &AlbumInfo, offset: u32) -> Result<Page<Track>> {
    let page: RawPage<RawTrack> =
        api.get_json(&format!("/albums/{}/tracks?limit={PAGE}&offset={offset}", album.id)).await?;
    let next_offset = next_offset(&page, offset);
    Ok(Page {
        items: page
            .items
            .into_iter()
            .map(|raw| Track {
                album: album.name.clone(),
                album_uri: album.uri.clone(),
                image_url: album.image_url.clone(),
                ..Track::from(raw)
            })
            .collect(),
        total: page.total,
        next_offset,
    })
}

pub async fn track(api: &WebApi, id: &str) -> Result<Track> {
    let raw: RawTrack = api.get_json(&format!("/tracks/{id}")).await?;
    Ok(Track::from(raw))
}

pub async fn tracks_by_ids(api: &WebApi, ids: &[String]) -> Result<Vec<Track>> {
    let mut tracks = Vec::new();
    for chunk in ids.chunks(PAGE as usize) {
        let listed = chunk.join(",");
        let raw: RawTracks = api.get_json(&format!("/tracks?ids={listed}")).await?;
        tracks.extend(raw.tracks.into_iter().flatten().map(Track::from));
    }
    Ok(tracks)
}

pub async fn album(api: &WebApi, id: &str) -> Result<AlbumInfo> {
    let raw: RawAlbum = api.get_json(&format!("/albums/{id}")).await?;
    Ok(AlbumInfo::from(raw))
}

pub async fn artist(api: &WebApi, id: &str) -> Result<ArtistInfo> {
    let raw: RawArtist = api.get_json(&format!("/artists/{id}")).await?;
    Ok(ArtistInfo::from(raw))
}

pub async fn artist_top_tracks(api: &WebApi, id: &str) -> Result<Vec<Track>> {
    let raw: RawTracks = api.get_json(&format!("/artists/{id}/top-tracks?market=from_token")).await?;
    Ok(raw.tracks.into_iter().flatten().map(Track::from).collect())
}

pub async fn artist_albums(api: &WebApi, id: &str) -> Result<Vec<AlbumInfo>> {
    let page: RawPage<RawAlbum> =
        api.get_json(&format!("/artists/{id}/albums?include_groups=album,single&limit={PAGE}")).await?;
    Ok(page.items.into_iter().map(AlbumInfo::from).collect())
}

pub async fn recommendations(api: &WebApi, seed: &str, limit: u32) -> Result<Vec<Track>> {
    let raw: RawTracks = api.get_json(&format!("/recommendations?seed_tracks={seed}&limit={limit}")).await?;
    Ok(raw.tracks.into_iter().flatten().map(Track::from).collect())
}

pub async fn search(api: &WebApi, query: &str) -> Result<SearchResults> {
    let query = encode(query);
    let raw: RawSearch = api.get_json(&format!("/search?q={query}&type=track,album,artist,playlist&limit=10")).await?;
    Ok(SearchResults {
        tracks: items(raw.tracks).map(Track::from).collect(),
        albums: items(raw.albums).map(AlbumInfo::from).collect(),
        artists: items(raw.artists).map(ArtistInfo::from).collect(),
        playlists: items(raw.playlists).map(PlaylistInfo::from).collect(),
    })
}

pub async fn save(api: &WebApi, uris: &[String]) -> Result<()> {
    library_call(api, Method::PUT, uris).await
}

pub async fn unsave(api: &WebApi, uris: &[String]) -> Result<()> {
    library_call(api, Method::DELETE, uris).await
}

async fn library_call(api: &WebApi, method: Method, uris: &[String]) -> Result<()> {
    for batch in uris.chunks(LIBRARY_BATCH) {
        api.call(method.clone(), &format!("/me/library?uris={}", encode(&batch.join(","))), None).await?;
    }
    Ok(())
}

pub async fn contains(api: &WebApi, uris: &[String]) -> Result<Vec<bool>> {
    let mut saved = Vec::with_capacity(uris.len());
    for batch in uris.chunks(LIBRARY_BATCH) {
        let flags: Vec<bool> = api.get_json(&format!("/me/library/contains?uris={}", encode(&batch.join(",")))).await?;
        saved.extend(flags);
    }
    Ok(saved)
}

pub async fn queue(api: &WebApi, uri: &str) -> Result<()> {
    api.call(Method::POST, &format!("/me/player/queue?uri={}", encode(uri)), None).await
}

pub async fn upcoming(api: &WebApi) -> Result<Vec<Track>> {
    let raw: RawQueue = api.get_json("/me/player/queue").await?;
    let tracks = raw.queue.into_iter().filter_map(|item| serde_json::from_value::<RawTrack>(item).ok());
    Ok(tracks.filter(|t| t.uri.starts_with("spotify:track:")).map(Track::from).collect())
}

pub async fn add_to_playlist(api: &WebApi, playlist_id: &str, uris: &[String]) -> Result<()> {
    let body = json!({ "uris": uris });
    api.call(Method::POST, &format!("/playlists/{playlist_id}/items"), Some(&body)).await
}

pub async fn playlist_track_uris(session: &Session, id: &str) -> Result<Vec<SpotifyUri>> {
    let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}"))?;
    let playlist = meta::Playlist::get(session, &uri).await?;
    Ok(playlist.tracks().filter(|uri| matches!(uri, SpotifyUri::Track { .. })).cloned().collect())
}

pub async fn tracks_metadata(session: &Session, uris: &[SpotifyUri]) -> Vec<Track> {
    match batched_metadata(session, uris).await {
        Ok(tracks) => tracks,
        Err(err) => {
            tracing::warn!("batched track metadata failed, asking one by one: {err:#}");
            stream::iter(uris.to_vec())
                .map(|uri| async move { meta::Track::get(session, &uri).await })
                .buffered(METADATA_CONCURRENCY)
                .filter_map(|track| async move { track.ok().map(metadata_track) })
                .collect()
                .await
        }
    }
}

async fn batched_metadata(session: &Session, uris: &[SpotifyUri]) -> Result<Vec<Track>> {
    let query = ExtensionQuery { extension_kind: EnumOrUnknown::new(ExtensionKind::TRACK_V4), ..Default::default() };
    let request = BatchedEntityRequest {
        entity_request: uris
            .iter()
            .map(|uri| EntityRequest { entity_uri: uri.to_uri(), query: vec![query.clone()], ..Default::default() })
            .collect(),
        ..Default::default()
    };
    let response = session.spclient().get_extended_metadata(request).await?;
    let mut found: HashMap<String, Track> = response
        .extended_metadata
        .iter()
        .flat_map(|array| &array.extension_data)
        .filter(|data| matches!(data.header.status_code, 0 | 200))
        .filter_map(|data| {
            let message = TrackMessage::parse_from_bytes(&data.extension_data.as_ref()?.value).ok()?;
            let uri = SpotifyUri::from_uri(&data.entity_uri).ok()?;
            let track = <meta::Track as Metadata>::parse(&message, &uri).ok()?;
            Some((data.entity_uri.clone(), metadata_track(track)))
        })
        .collect();
    Ok(uris.iter().filter_map(|uri| found.remove(&uri.to_uri())).collect())
}

fn pick_image(images: &[RawImage]) -> Option<String> {
    images
        .iter()
        .min_by_key(|image| image.width.unwrap_or(IMAGE_WIDTH).abs_diff(IMAGE_WIDTH))
        .map(|image| image.url.clone())
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn next_offset<T>(page: &RawPage<T>, offset: u32) -> Option<u32> {
    let next = offset + page.items.len() as u32;
    (page.next.is_some() && next < page.total).then_some(next)
}

fn items<T>(page: Option<RawPage<Option<T>>>) -> impl Iterator<Item = T> {
    page.into_iter().flat_map(|p| p.items).flatten()
}

impl From<RawTrack> for Track {
    fn from(raw: RawTrack) -> Self {
        let (album, album_uri, image_url) =
            raw.album.map(|a| (a.name, a.uri, pick_image(&a.images))).unwrap_or_default();
        Self {
            uri: raw.uri,
            name: raw.name,
            artist_uris: raw.artists.iter().map(|a| a.uri.clone()).collect(),
            artists: raw.artists.into_iter().map(|a| a.name).collect(),
            album,
            album_uri,
            duration_ms: raw.duration_ms,
            image_url,
        }
    }
}

fn metadata_track(track: meta::Track) -> Track {
    Track {
        uri: track.id.to_uri(),
        name: track.name,
        artists: track.artists.iter().map(|a| a.name.clone()).collect(),
        artist_uris: track.artists.iter().map(|a| a.id.to_uri()).collect(),
        album: track.album.name.clone(),
        album_uri: track.album.id.to_uri(),
        duration_ms: track.duration.max(0) as u32,
        image_url: track
            .album
            .covers
            .iter()
            .min_by_key(|cover| cover.width.unsigned_abs().abs_diff(IMAGE_WIDTH))
            .map(|cover| format!("https://i.scdn.co/image/{}", cover.id)),
    }
}

impl From<RawPlaylist> for PlaylistInfo {
    fn from(raw: RawPlaylist) -> Self {
        Self {
            id: raw.id,
            uri: raw.uri,
            name: raw.name,
            owner_name: raw.owner.display_name.unwrap_or_else(|| raw.owner.id.clone()),
            owner_id: raw.owner.id,
            collaborative: raw.collaborative,
            total: raw.items.or(raw.tracks).map(|c| c.total),
            image_url: pick_image(&raw.images),
        }
    }
}

impl From<RawAlbum> for AlbumInfo {
    fn from(raw: RawAlbum) -> Self {
        Self {
            id: raw.id,
            uri: raw.uri,
            name: raw.name,
            artists: raw.artists.into_iter().map(|a| a.name).collect(),
            year: raw.release_date.chars().take(4).collect(),
            total: raw.total_tracks,
            image_url: pick_image(&raw.images),
        }
    }
}

impl From<RawArtist> for ArtistInfo {
    fn from(raw: RawArtist) -> Self {
        Self { image_url: pick_image(&raw.images), uri: raw.uri, name: raw.name }
    }
}

#[derive(Deserialize)]
struct RawPage<T> {
    items: Vec<T>,
    #[serde(default)]
    total: u32,
    next: Option<String>,
}

#[derive(Deserialize)]
struct RawUser {
    id: String,
    display_name: Option<String>,
    #[serde(default)]
    product: Option<String>,
}

#[derive(Deserialize)]
struct RawSavedTrack {
    track: Option<RawTrack>,
}

#[derive(Deserialize)]
struct RawPlaylistItem {
    item: Option<RawTrack>,
}

#[derive(Deserialize)]
struct RawQueue {
    #[serde(default)]
    queue: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawTrack {
    uri: String,
    name: String,
    #[serde(default)]
    artists: Vec<RawNamed>,
    album: Option<RawAlbumRef>,
    #[serde(default)]
    duration_ms: u32,
}

#[derive(Deserialize)]
struct RawTracks {
    #[serde(default)]
    tracks: Vec<Option<RawTrack>>,
}

#[derive(Deserialize)]
struct RawNamed {
    name: String,
    #[serde(default)]
    uri: String,
}

#[derive(Deserialize)]
struct RawAlbumRef {
    name: String,
    uri: String,
    #[serde(default, deserialize_with = "nullable")]
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawAlbum {
    id: String,
    uri: String,
    name: String,
    #[serde(default)]
    artists: Vec<RawNamed>,
    #[serde(default)]
    release_date: String,
    #[serde(default)]
    total_tracks: u32,
    #[serde(default, deserialize_with = "nullable")]
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawArtist {
    uri: String,
    name: String,
    #[serde(default, deserialize_with = "nullable")]
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawImage {
    url: String,
    width: Option<u32>,
}

#[derive(Deserialize)]
struct RawSavedAlbum {
    album: RawAlbum,
}

#[derive(Deserialize)]
struct RawSavedShow {
    show: RawShow,
}

#[derive(Deserialize)]
struct RawShow {
    uri: String,
    name: String,
    #[serde(default, deserialize_with = "nullable")]
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawFollowing {
    artists: RawCursorPage<RawArtist>,
}

#[derive(Deserialize)]
struct RawCursorPage<T> {
    items: Vec<T>,
    next: Option<String>,
    cursors: Option<RawCursors>,
}

#[derive(Deserialize)]
struct RawCursors {
    after: Option<String>,
}

#[derive(Deserialize)]
struct RawPlaylist {
    id: String,
    uri: String,
    name: String,
    owner: RawOwner,
    #[serde(default)]
    collaborative: bool,
    #[serde(default, deserialize_with = "nullable")]
    images: Vec<RawImage>,
    items: Option<RawCount>,
    tracks: Option<RawCount>,
}

#[derive(Deserialize)]
struct RawOwner {
    id: String,
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct RawCount {
    total: u32,
}

#[derive(Deserialize)]
struct RawSearch {
    tracks: Option<RawPage<Option<RawTrack>>>,
    albums: Option<RawPage<Option<RawAlbum>>>,
    artists: Option<RawPage<Option<RawArtist>>>,
    playlists: Option<RawPage<Option<RawPlaylist>>>,
}

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
