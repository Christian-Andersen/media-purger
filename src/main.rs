use chrono::{DateTime, Utc};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use dotenvy::dotenv;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Style, Stylize},
    widgets::{Cell, Row, Table, TableState},
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::env;
use std::thread;

#[derive(Parser, Debug)]
#[command(name = "media-purger")]
#[command(about = "Delete watched but not favorited media from Jellyfin", long_about = None)]
struct Args {
    #[arg(long, env, default_value = "")]
    jellyfin_url: String,

    #[arg(long, env, default_value = "")]
    jellyfin_api_key: String,

    #[arg(long, short, help = "Users who must all have watched this to delete")]
    watched_by: Vec<String>,

    #[arg(
        long,
        short,
        help = "Users where any have favorited to protect from deletion"
    )]
    protected_by: Vec<String>,

    #[arg(short, long, help = "Interactive mode (TUI)")]
    interactive: bool,

    #[arg(long, env, default_value = "false", help = "Actually perform deletion")]
    delete_watched_but_not_favourited_yes_i_am_really_sure: bool,

    #[arg(
        long,
        env,
        default_value = "false",
        help = "Ignore favorite status; include all watched items even if favorited by any user"
    )]
    ignore_favorites: bool,

    #[arg(long, help = "Only include items not watched in the last N days")]
    min_days_watched_ago: Option<u32>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "PascalCase")]
struct JellyfinUser {
    id: String,
    name: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "PascalCase")]
struct ItemsResponse {
    items: Vec<MediaItem>,
    #[allow(dead_code)]
    total_record_count: Option<u32>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "PascalCase")]
struct MediaItem {
    id: String,
    name: String,
    #[serde(rename = "Type")]
    item_type: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    index_number: Option<u32>,
    #[serde(default)]
    parent_index_number: Option<u32>,
    #[allow(dead_code)]
    #[serde(default)]
    last_played_date: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    play_count: Option<u32>,
    #[serde(default)]
    user_data: Option<UserData>,
    #[allow(dead_code)]
    #[serde(default)]
    media_sources: Option<Vec<MediaSourceInfo>>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "PascalCase")]
struct MediaSourceInfo {
    #[allow(dead_code)]
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "PascalCase")]
struct UserData {
    #[allow(dead_code)]
    #[serde(default)]
    played: bool,
    #[allow(dead_code)]
    #[serde(default)]
    is_favorite: bool,
    #[serde(default)]
    last_played_date: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    play_count: Option<u32>,
}

struct Config {
    base_url: String,
    api_key: String,
    watched_by: Vec<String>,
    protected_by: Vec<String>,
    delete: bool,
    interactive: bool,
    ignore_favorites: bool,
    min_days_watched_ago: Option<u32>,
}

fn resolve_config() -> Config {
    dotenv().ok();

    let args = Args::parse();

    let base_url = if !args.jellyfin_url.is_empty() {
        args.jellyfin_url
    } else if let Ok(url) = env::var("JELLYFIN_URL") {
        url
    } else {
        eprintln!("Error: No Jellyfin URL provided. Use --jellyfin-url or JELLYFIN_URL");
        std::process::exit(1);
    };

    let api_key = if !args.jellyfin_api_key.is_empty() {
        args.jellyfin_api_key
    } else if let Ok(key) = env::var("JELLYFIN_API_KEY") {
        key
    } else {
        eprintln!("Error: No API key provided. Use --jellyfin-api-key or JELLYFIN_API_KEY");
        std::process::exit(1);
    };

    if args.delete_watched_but_not_favourited_yes_i_am_really_sure && args.interactive {
        eprintln!("Error: --delete-watched-but-not-favourited-yes-i-am-really-sure cannot be used with --interactive");
        std::process::exit(1);
    }

    if args.ignore_favorites && !args.protected_by.is_empty() {
        eprintln!("Error: --ignore-favorites cannot be used with --protected-by");
        std::process::exit(1);
    }

    Config {
        base_url,
        api_key,
        watched_by: args.watched_by,
        protected_by: args.protected_by,
        delete: args.delete_watched_but_not_favourited_yes_i_am_really_sure,
        interactive: args.interactive,
        ignore_favorites: args.ignore_favorites,
        min_days_watched_ago: args.min_days_watched_ago,
    }
}

#[must_use]
fn create_client(api_key: &str) -> Result<Client, Box<dyn std::error::Error>> {
    let auth_header_value = format!("MediaBrowser Token=\"{}\"", api_key);
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Emby-Authorization",
        HeaderValue::from_str(&auth_header_value)?,
    );
    Ok(Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

fn handle_api_error(
    response: reqwest::blocking::Response,
    context: &str,
) -> Box<dyn std::error::Error> {
    let status = response.status();
    if status.as_u16() == 401 {
        format!(
            "{}: Authentication failed (401). Check your API key.",
            context
        )
        .into()
    } else if status.as_u16() == 403 {
        format!(
            "{}: Permission denied (403). User may not have delete permission.",
            context
        )
        .into()
    } else if status.as_u16() == 404 {
        format!("{}: Item not found (404).", context).into()
    } else if status.is_server_error() {
        format!("{}: Server error ({})", context, status).into()
    } else if status.is_client_error() {
        format!("{}: Client error ({})", context, status).into()
    } else {
        format!("{}: Unknown error ({})", context, status).into()
    }
}

#[must_use]
fn fetch_users(
    client: &Client,
    base_url: &str,
) -> Result<Vec<JellyfinUser>, Box<dyn std::error::Error>> {
    let url = format!("{}/Users", base_url);
    let response = client.get(url).send()?;
    if response.status().is_success() {
        let users: Vec<JellyfinUser> = response.json()?;
        Ok(users)
    } else {
        Err(handle_api_error(response, "Failed to fetch users"))
    }
}

#[must_use]
fn fetch_all_items(
    client: &Client,
    base_url: &str,
) -> Result<Vec<MediaItem>, Box<dyn std::error::Error>> {
    let url = format!(
        "{}/Items?Recursive=true&Fields=Path,UserData,MediaSources,Size,ParentId,SeasonName,IndexNumber,ParentIndexNumber",
        base_url
    );
    let response = client.get(url).send()?;
    if response.status().is_success() {
        let items_response: ItemsResponse = response.json()?;
        Ok(items_response.items)
    } else {
        Err(handle_api_error(response, "Failed to fetch items"))
    }
}

#[must_use]
fn fetch_played_items(
    client: &Client,
    base_url: &str,
    user_id: &str,
) -> Result<HashMap<String, Option<String>>, Box<dyn std::error::Error>> {
    let mut all_items = Vec::new();
    let mut start_index = 0;
    let limit = 500;

    loop {
        let url = format!(
            "{}/Items?userId={}&IsPlayed=true&Recursive=true&Fields=Path,MediaSources,UserData,Size&startIndex={}&limit={}",
            base_url, user_id, start_index, limit
        );

        let response = client.get(url).send()?;
        if !response.status().is_success() {
            return Err(handle_api_error(response, "Failed to fetch played items"));
        }
        let items_response: ItemsResponse = response.json()?;
        let count = items_response.items.len();
        all_items.extend(items_response.items);
        start_index += limit;
        if count < limit {
            break;
        }
    }

    let mut result = HashMap::new();
    for item in all_items {
        let last_played = item
            .user_data
            .as_ref()
            .and_then(|u| u.last_played_date.clone())
            .or(item.last_played_date.clone());
        result.insert(item.id, last_played);
    }
    Ok(result)
}

fn build_ancestor_map(items: &[MediaItem]) -> HashMap<String, HashSet<String>> {
    let item_by_id: HashMap<String, &MediaItem> = items.iter().map(|i| (i.id.clone(), i)).collect();
    let mut ancestor_map: HashMap<String, HashSet<String>> = HashMap::new();

    for item in items {
        let mut ancestors = HashSet::new();
        let mut current_parent_id = item.parent_id.as_ref();

        while let Some(parent_id) = current_parent_id {
            if ancestors.contains(parent_id) {
                break;
            }
            ancestors.insert(parent_id.clone());
            current_parent_id = item_by_id.get(parent_id).and_then(|p| p.parent_id.as_ref());
        }

        if !ancestors.is_empty() {
            ancestor_map.insert(item.id.clone(), ancestors);
        }
    }

    ancestor_map
}

#[must_use]
fn fetch_favorite_items(
    client: &Client,
    base_url: &str,
    user_id: &str,
) -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let mut all_items = Vec::new();
    let mut start_index = 0;
    let limit = 500;

    loop {
        let url = format!(
            "{}/Items?userId={}&IsFavorite=true&Recursive=true&startIndex={}&limit={}",
            base_url, user_id, start_index, limit
        );

        let response = client.get(url).send()?;
        if !response.status().is_success() {
            return Err(handle_api_error(response, "Failed to fetch favorite items"));
        }
        let items_response: ItemsResponse = response.json()?;
        let count = items_response.items.len();
        all_items.extend(items_response.items);
        start_index += limit;
        if count < limit {
            break;
        }
    }

    let favorites: HashSet<String> = all_items.into_iter().map(|i| i.id).collect();
    Ok(favorites)
}

#[must_use]
fn delete_item(
    client: &Client,
    base_url: &str,
    item_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{}/Items/{}", base_url, item_id);
    let response = client.delete(url).send()?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(handle_api_error(
            response,
            &format!("Failed to delete item {}", item_id),
        ))
    }
}

#[derive(Clone)]
struct MatchedItem {
    id: String,
    name: String,
    item_type: String,
    path: String,
    last_played_date: Option<String>,
    played_by: Vec<String>,
    #[allow(dead_code)]
    protected_by: Vec<String>,
    size: u64,
    series_name: Option<String>,
    season_number: Option<u32>,
    episode_number: Option<u32>,
    series_id: Option<String>,
    season_id: Option<String>,
}

#[derive(Clone)]
enum GroupedItem {
    Individual(MatchedItem),
    Season {
        series_name: String,
        season_number: u32,
        items: Vec<MatchedItem>,
    },
    Series {
        series_name: String,
        total_episodes: u32,
        seasons: Vec<u32>,
        items: Vec<MatchedItem>,
    },
}

impl GroupedItem {
    fn items(&self) -> Vec<&MatchedItem> {
        match self {
            GroupedItem::Individual(item) => vec![item],
            GroupedItem::Season { items, .. } => items.iter().collect(),
            GroupedItem::Series { items, .. } => items.iter().collect(),
        }
    }

    fn display_name(&self) -> String {
        match self {
            GroupedItem::Individual(item) => format_display_name(item),
            GroupedItem::Season {
                series_name,
                season_number,
                items,
            } => {
                let total_size: u64 = items.iter().map(|i| i.size).sum();
                format!(
                    "{} - Season {} ({} episodes, {})",
                    series_name,
                    season_number,
                    items.len(),
                    format_size(total_size)
                )
            }
            GroupedItem::Series {
                series_name,
                total_episodes,
                seasons,
                items,
            } => {
                let total_size: u64 = items.iter().map(|i| i.size).sum();
                format!(
                    "{} ({} seasons, {} episodes, {})",
                    series_name,
                    seasons.len(),
                    total_episodes,
                    format_size(total_size)
                )
            }
        }
    }
}

#[must_use]
fn find_deletable_items(
    api_key: &str,
    _client: &Client,
    base_url: &str,
    users: &[JellyfinUser],
    items: &[MediaItem],
    watched_by: &[String],
    protected_by: &[String],
    ignore_favorites: bool,
    min_days_watched_ago: Option<u32>,
) -> Result<Vec<MatchedItem>, Box<dyn std::error::Error>> {
    let user_map: HashMap<String, &JellyfinUser> =
        users.iter().map(|u| (u.name.clone(), u)).collect();

    let watched_ids: Vec<&str> = if watched_by.is_empty() {
        users.iter().map(|u| u.id.as_str()).collect()
    } else {
        watched_by
            .iter()
            .filter_map(|n| user_map.get(n).map(|u| u.id.as_str()))
            .collect()
    };

    // Warn if --watched-by specified but no valid users found
    if !watched_by.is_empty() && watched_ids.is_empty() {
        eprintln!("Warning: No valid users found for --watched-by, no items will be processed.");
        return Ok(vec![]);
    }

    let protected_ids: Vec<&str> = if ignore_favorites {
        vec![]
    } else if protected_by.is_empty() {
        users.iter().map(|u| u.id.as_str()).collect()
    } else {
        protected_by
            .iter()
            .filter_map(|n| user_map.get(n).map(|u| u.id.as_str()))
            .collect()
    };

    // Warn if --protected-by specified but no valid users found
    if !protected_by.is_empty() && protected_ids.is_empty() {
        eprintln!("Warning: No valid users found for --protected-by, no items will be protected.");
    }

    println!("Fetching played items for {} users...", watched_ids.len());
    let mut user_played: HashMap<String, HashMap<String, Option<String>>> = HashMap::new();
    if !watched_ids.is_empty() {
        let api_key = api_key.to_string();
        let base_url = base_url.to_string();
        let thread_watched_ids: Vec<String> = watched_ids.iter().map(|s| s.to_string()).collect();
        let handles: Vec<_> = thread_watched_ids.iter().map(|user_id| {
            let user_id = user_id.clone();
            let api_key = api_key.clone();
            let base_url = base_url.clone();
            thread::spawn(move || {
                let client = create_client(&api_key).map_err(|e| e.to_string())?;
                fetch_played_items(&client, &base_url, &user_id)
                    .map(|played| (user_id, played))
                    .map_err(|e| e.to_string())
            })
        }).collect();
        for handle in handles {
            let (user_id, played) = handle.join().map_err(|e| format!("thread panicked: {:?}", e))??;
            println!("  User {} has {} played items", user_id, played.len());
            user_played.insert(user_id, played);
        }
    }

    let mut user_favorites: HashMap<String, HashSet<String>> = HashMap::new();
    if !protected_ids.is_empty() {
        println!(
            "Fetching favorite items for {} users...",
            protected_ids.len()
        );
        let api_key = api_key.to_string();
        let base_url = base_url.to_string();
        let thread_protected_ids: Vec<String> = protected_ids.iter().map(|s| s.to_string()).collect();
        let handles: Vec<_> = thread_protected_ids.iter().map(|user_id| {
            let user_id = user_id.clone();
            let api_key = api_key.clone();
            let base_url = base_url.clone();
            thread::spawn(move || {
                let client = create_client(&api_key).map_err(|e| e.to_string())?;
                fetch_favorite_items(&client, &base_url, &user_id)
                    .map(|favorites| (user_id, favorites))
                    .map_err(|e| e.to_string())
            })
        }).collect();
        for handle in handles {
            let (user_id, favorites) = handle.join().map_err(|e| format!("thread panicked: {:?}", e))??;
            user_favorites.insert(user_id, favorites);
        }
    }

    let mut matched_items = Vec::new();

    let ancestor_map = build_ancestor_map(items);

    let item_by_id: HashMap<String, &MediaItem> = items.iter().map(|i| (i.id.clone(), i)).collect();

    for item in items {
        // Initialize as true: vacuous truth if no watched_ids, else prove false
        let mut all_watched = true;
        let mut played_by = Vec::new();
        let mut latest_date: Option<String> = None;

        if !watched_ids.is_empty() {
            for user_id in &watched_ids {
                if let Some(played_map) = user_played.get(*user_id) {
                    if let Some(date) = played_map.get(&item.id) {
                        if let Some(user) = users.iter().find(|u| u.id == *user_id) {
                            played_by.push(user.name.clone());
                        }
                        if let Some(d) = date {
                            if latest_date.is_none() || d > latest_date.as_ref().unwrap() {
                                latest_date = Some(d.clone());
                            }
                        }
                    } else {
                        all_watched = false;
                        break;
                    }
                } else {
                    all_watched = false;
                    break;
                }
            }
        }

        if !all_watched {
            continue;
        }

        if let Some(min_days) = min_days_watched_ago {
            let date_str = match &latest_date {
                Some(d) => d,
                None => {
                    continue;
                }
            };
            if let Ok(date) = DateTime::parse_from_rfc3339(date_str) {
                let now = Utc::now();
                let days_ago = (now - date.with_timezone(&Utc)).num_days();
                if days_ago < min_days as i64 {
                    continue;
                }
            } else {
                eprintln!(
                    "Warning: Could not parse date '{}' for item '{}', including anyway",
                    date_str, item.name
                );
            }
        }

        let mut protected = false;
        let mut protected_by_users = Vec::new();

        let ancestors = ancestor_map.get(&item.id).cloned().unwrap_or_default();

        for user_id in &protected_ids {
            if let Some(fav_set) = user_favorites.get(*user_id) {
                if fav_set.contains(&item.id) || ancestors.iter().any(|a| fav_set.contains(a)) {
                    protected = true;
                    if let Some(user) = users.iter().find(|u| u.id == *user_id) {
                        protected_by_users.push(user.name.clone());
                    }
                }
            }
        }

        if protected {
            continue;
        }

        let size = item
            .media_sources
            .as_ref()
            .and_then(|sources| sources.iter().find_map(|s| s.size))
            .unwrap_or(0);

        let series_name = if item.item_type == "Episode" {
            let mut current_id = item.parent_id.clone();
            let mut series: Option<&MediaItem> = None;
            let mut visited = HashSet::new();
            while let Some(pid) = current_id {
                if visited.contains(&pid) {
                    break;
                }
                visited.insert(pid.clone());
                if let Some(parent) = item_by_id.get(&pid) {
                    if parent.item_type == "Series" {
                        series = Some(parent);
                        break;
                    }
                    current_id = parent.parent_id.clone();
                } else {
                    break;
                }
            }
            series.map(|s| s.name.clone())
        } else {
            None
        };

        let mut series_id: Option<String> = None;
        let mut season_id: Option<String> = None;

        if item.item_type == "Episode" {
            let mut current_id = item.parent_id.clone();
            let mut visited = HashSet::new();
            while let Some(pid) = current_id {
                if visited.contains(&pid) {
                    break;
                }
                visited.insert(pid.clone());
                if let Some(parent) = item_by_id.get(&pid) {
                    if parent.item_type == "Season" {
                        season_id = Some(pid.clone());
                        current_id = parent.parent_id.clone();
                    } else if parent.item_type == "Series" {
                        series_id = Some(pid.clone());
                        break;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }

        matched_items.push(MatchedItem {
            id: item.id.clone(),
            name: item.name.clone(),
            item_type: item.item_type.clone(),
            path: item.path.clone().unwrap_or_default(),
            last_played_date: latest_date,
            played_by,
            protected_by: protected_by_users,
            size,
            series_name,
            season_number: item.parent_index_number,
            episode_number: item.index_number,
            series_id,
            season_id,
        });
    }

    matched_items.sort_by(|a, b| b.size.cmp(&a.size));

    Ok(matched_items)
}

fn group_items(all_items: &[MediaItem], deletable_items: Vec<MatchedItem>) -> Vec<GroupedItem> {
    let mut result: Vec<GroupedItem> = Vec::new();

    // Build a map of all episode IDs per season
    let mut all_episodes_by_season: HashMap<String, Vec<String>> = HashMap::new();
    for item in all_items {
        if item.item_type == "Episode" {
            if let Some(season_id) = &item.parent_id {
                all_episodes_by_season
                    .entry(season_id.clone())
                    .or_default()
                    .push(item.id.clone());
            }
        }
    }

    // Build set of deletable episode IDs
    let deletable_ids: HashSet<String> = deletable_items.iter().map(|i| i.id.clone()).collect();

    let mut episodes_by_season: HashMap<String, Vec<MatchedItem>> = HashMap::new();
    let mut series_seasons: HashMap<String, HashSet<String>> = HashMap::new();
    let mut series_info: HashMap<String, String> = HashMap::new();

    for item in &deletable_items {
        if item.item_type == "Episode" {
            if let (Some(series_id), Some(season_id)) = (&item.series_id, &item.season_id) {
                episodes_by_season
                    .entry(season_id.clone())
                    .or_default()
                    .push(item.clone());
                series_seasons
                    .entry(series_id.clone())
                    .or_default()
                    .insert(season_id.clone());
                if let Some(name) = &item.series_name {
                    series_info
                        .entry(series_id.clone())
                        .or_insert_with(|| name.clone());
                }
            }
        }
    }

    // Group seasons - only if ALL episodes in that season are in deletable set
    let mut grouped_season_ids: HashSet<String> = HashSet::new();
    for (season_id, episodes) in &episodes_by_season {
        if episodes.len() > 1 {
            // Check if ALL episodes in this season are in the deletable set
            let all_season_episode_ids = all_episodes_by_season.get(season_id);
            let all_deletable = all_season_episode_ids
                .map(|all_ids| all_ids.iter().all(|id| deletable_ids.contains(id)))
                .unwrap_or(true); // If we can't find all episodes, assume all are deletable

            if all_deletable {
                let season_number = episodes[0].season_number.unwrap_or(0);
                let series_name = episodes[0].series_name.clone().unwrap_or_default();
                grouped_season_ids.insert(season_id.clone());
                result.push(GroupedItem::Season {
                    series_name,
                    season_number,
                    items: episodes.clone(),
                });
            }
        }
    }

    // Group series - only if ALL seasons in that series are grouped
    let mut grouped_series_ids: HashSet<String> = HashSet::new();
    for (series_id, season_ids) in &series_seasons {
        // Check if ALL seasons in this series are grouped
        let all_seasons_grouped = season_ids
            .iter()
            .all(|sid| grouped_season_ids.contains(sid));
        if season_ids.len() > 1 && all_seasons_grouped {
            // Verify all episodes in all seasons are in deletable set
            let mut all_episodes_deletable = true;
            for season_id in season_ids {
                if let Some(all_ep_ids) = all_episodes_by_season.get(season_id) {
                    if !all_ep_ids.iter().all(|id| deletable_ids.contains(id)) {
                        all_episodes_deletable = false;
                        break;
                    }
                }
            }

            if all_episodes_deletable {
                let series_name = series_info.get(series_id).cloned().unwrap_or_default();
                let all_episodes: Vec<MatchedItem> = season_ids
                    .iter()
                    .filter_map(|sid| episodes_by_season.get(sid))
                    .flatten()
                    .cloned()
                    .collect();
                let total_episodes = all_episodes.len() as u32;
                let seasons: Vec<u32> = all_episodes
                    .iter()
                    .filter_map(|e| e.season_number)
                    .collect::<HashSet<u32>>()
                    .into_iter()
                    .collect();
                grouped_series_ids.insert(series_id.clone());
                result.push(GroupedItem::Series {
                    series_name,
                    total_episodes,
                    seasons,
                    items: all_episodes,
                });
            }
        }
    }

    for item in deletable_items {
        let is_grouped = if let Some(series_id) = &item.series_id {
            grouped_series_ids.contains(series_id)
        } else if let Some(season_id) = &item.season_id {
            grouped_season_ids.contains(season_id)
        } else {
            false
        };

        if !is_grouped {
            result.push(GroupedItem::Individual(item));
        }
    }

    result.sort_by(|a, b| {
        let a_size: u64 = a.items().iter().map(|i| i.size).sum();
        let b_size: u64 = b.items().iter().map(|i| i.size).sum();
        b_size.cmp(&a_size)
    });

    result
}

fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if size >= TB {
        format!("{:.2} TB", size as f64 / TB as f64)
    } else if size >= GB {
        format!("{:.2} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.2} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.2} KB", size as f64 / KB as f64)
    } else {
        format!("{} B", size)
    }
}

fn format_days_ago(date_str: &str) -> String {
    if let Ok(date) = DateTime::parse_from_rfc3339(date_str) {
        let now = Utc::now();
        let days = (now - date.with_timezone(&Utc)).num_days();
        if days == 0 {
            "Today".to_string()
        } else if days == 1 {
            "Yesterday".to_string()
        } else {
            format!("{} days ago", days)
        }
    } else {
        date_str.to_string()
    }
}

fn format_display_name(item: &MatchedItem) -> String {
    if let (Some(series), Some(season), Some(episode)) =
        (&item.series_name, item.season_number, item.episode_number)
    {
        format!("{} - S{:02}E{:02} - {}", series, season, episode, item.name)
    } else {
        item.name.clone()
    }
}

fn run_dry_run(items: &[GroupedItem]) {
    println!("\n=== Items to be deleted (dry-run) ===\n");
    for item in items {
        println!("{}", item.display_name());
        match item {
            GroupedItem::Individual(i) => {
                println!("    Path: {}", i.path);
                if let Some(date) = &i.last_played_date {
                    println!("    Last played: {}", format_days_ago(date));
                }
                println!("    Watched by: {}", i.played_by.join(", "));
            }
            GroupedItem::Season {
                items: ep_items, ..
            } => {
                if let Some(first) = ep_items.first() {
                    println!("    Path: {}", first.path);
                    println!("    Watched by: {}", first.played_by.join(", "));
                }
            }
            GroupedItem::Series { .. } => {}
        }
        println!();
    }
    let total: usize = items.iter().map(|g| g.items().len()).sum();
    println!("Total: {} items", total);
}

fn run_delete(
    client: &Client,
    base_url: &str,
    items: &[GroupedItem],
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut deleted = 0;
    let total_items: usize = items.iter().map(|g| g.items().len()).sum();
    let mut current = 0;

    for group in items {
        match group {
            GroupedItem::Individual(item) => {
                current += 1;
                print!(
                    "[{}/{}] Deleting [{}] {}...",
                    current,
                    total_items,
                    item.item_type,
                    format_display_name(item)
                );
                match delete_item(client, base_url, &item.id) {
                    Ok(_) => {
                        println!("\x1b[32m done\x1b[0m");
                        deleted += 1;
                    }
                    Err(e) => {
                        println!("\x1b[31m failed: {}\x1b[0m", e);
                    }
                }
            }
            GroupedItem::Season {
                series_name,
                season_number,
                items: episodes,
            } => {
                println!(
                    "  {} - Season {} ({} episodes)",
                    series_name,
                    season_number,
                    episodes.len()
                );
                for ep in episodes {
                    current += 1;
                    print!(
                        "    [{}/{}] Deleting {}...",
                        current,
                        total_items,
                        format_display_name(ep)
                    );
                    match delete_item(client, base_url, &ep.id) {
                        Ok(_) => {
                            println!("\x1b[32m done\x1b[0m");
                            deleted += 1;
                        }
                        Err(e) => {
                            println!("\x1b[31m failed: {}\x1b[0m", e);
                        }
                    }
                }
            }
            GroupedItem::Series {
                series_name,
                total_episodes,
                seasons,
                items,
            } => {
                println!(
                    "  {} ({} seasons, {} episodes)",
                    series_name,
                    seasons.len(),
                    total_episodes
                );
                for season_num in seasons {
                    println!("    Season {}", season_num);
                }
                for ep in items {
                    current += 1;
                    print!(
                        "      [{}/{}] Deleting {}...",
                        current,
                        total_items,
                        format_display_name(ep)
                    );
                    match delete_item(client, base_url, &ep.id) {
                        Ok(_) => {
                            println!("\x1b[32m done\x1b[0m");
                            deleted += 1;
                        }
                        Err(e) => {
                            println!("\x1b[31m failed: {}\x1b[0m", e);
                        }
                    }
                }
            }
        }
    }
    println!("\nDeleted {} items", deleted);
    Ok(deleted)
}

fn run_tui(items: Vec<GroupedItem>) -> Result<Vec<GroupedItem>, Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut selected: Vec<bool> = vec![false; items.len()];
    let mut cursor: usize = 0;
    let mut table_state = TableState::new();
    let mut show_help = false;
    let page_size = 20;

    loop {
        table_state.select(Some(cursor));

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(3),
                ])
                .split(f.area());

f.render_widget(
                ratatui::text::Line::from(" Media Purger - Select items to delete (space to toggle, enter to confirm, esc to cancel) ")
                    .bold()
                    .white()
                    .on_blue(),
                chunks[0],
            );

            let rows: Vec<Row> = items.iter().enumerate().map(|(i, item)| {
                let checkbox = if selected[i] { "[x]" } else { "[ ]" };
                let display_name = item.display_name();
                let total_size: u64 = item.items().iter().map(|i| i.size).sum();
                let size = format_size(total_size);
                let item_type = match item {
                    GroupedItem::Individual(i) => i.item_type.clone(),
                    GroupedItem::Season { .. } => "Season".to_string(),
                    GroupedItem::Series { .. } => "Series".to_string(),
                };
                let date = item.items().first()
                    .and_then(|i| i.last_played_date.as_deref())
                    .map(format_days_ago)
                    .unwrap_or("Never".to_string());
                let row = Row::new(vec![
                    Cell::from(checkbox),
                    Cell::from(display_name),
                    Cell::from(item_type),
                    Cell::from(size),
                    Cell::from(date),
                ]);
                if i == cursor {
                    row.style(Style::new().white().on_black())
                } else {
                    row
                }
            }).collect();

            let widths = [
                Constraint::Length(4),
                Constraint::Min(30),
                Constraint::Length(10),
                Constraint::Length(20),
                Constraint::Min(20),
            ];
            let table = Table::new(rows, widths)
                .column_spacing(1);

            f.render_stateful_widget(table, chunks[1], &mut table_state);

            if show_help {
                let help_text = vec![
                    ratatui::text::Line::from("").bold().white().on_black(),
                    ratatui::text::Line::from(" Keyboard Shortcuts ").bold().white().on_blue(),
                    ratatui::text::Line::from("").bold().white().on_black(),
                    ratatui::text::Line::from(" j / ↓    - Move down ").bold().white().on_black(),
                    ratatui::text::Line::from(" k / ↑    - Move up ").bold().white().on_black(),
                    ratatui::text::Line::from(" f / PgDn - Page down ").bold().white().on_black(),
                    ratatui::text::Line::from(" b / PgUp - Page up ").bold().white().on_black(),
                    ratatui::text::Line::from(" g / Home - Go to start ").bold().white().on_black(),
                    ratatui::text::Line::from(" G / End  - Go to end ").bold().white().on_black(),
                    ratatui::text::Line::from("").bold().white().on_black(),
                    ratatui::text::Line::from(" Space    - Toggle selection ").bold().white().on_black(),
                    ratatui::text::Line::from(" Enter    - Confirm selection ").bold().white().on_black(),
                    ratatui::text::Line::from(" ?        - Toggle this help ").bold().white().on_black(),
                    ratatui::text::Line::from(" q / Esc  - Quit ").bold().white().on_black(),
                    ratatui::text::Line::from("").bold().white().on_black(),
                    ratatui::text::Line::from(" Press any key to close ").bold().white().on_red(),
                ];
                let help_block = ratatui::widgets::Paragraph::new(help_text)
                    .block(ratatui::widgets::Block::default().title(" Help ").borders(ratatui::widgets::Borders::ALL))
                    .alignment(ratatui::layout::Alignment::Left);
                f.render_widget(help_block, chunks[1]);
            }

            let selected_count = selected.iter().filter(|&&s| s).count();
            let help_hint = if show_help { " (press any key)" } else { " | ? for help" };
            f.render_widget(
                ratatui::text::Line::from(format!(" Selected: {} | {}/{}{} ", selected_count, cursor + 1, items.len(), help_hint))
                    .bold()
                    .white()
                    .on_black(),
                chunks[2],
            );
        })?;

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                if show_help {
                    show_help = false;
                } else {
                    match key.code {
                        KeyCode::Char(' ') => {
                            selected[cursor] = !selected[cursor];
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if cursor < items.len() - 1 {
                                cursor += 1;
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if cursor > 0 {
                                cursor -= 1;
                            }
                        }
                        KeyCode::PageDown | KeyCode::Char('f') => {
                            cursor = (cursor + page_size).min(items.len() - 1);
                        }
                        KeyCode::PageUp | KeyCode::Char('b') => {
                            cursor = cursor.saturating_sub(page_size);
                        }
                        KeyCode::Home | KeyCode::Char('g') => {
                            cursor = 0;
                        }
                        KeyCode::End | KeyCode::Char('G') => {
                            cursor = items.len().saturating_sub(1);
                        }
                        KeyCode::Char('?') => {
                            show_help = !show_help;
                        }
                        KeyCode::Char('q') | KeyCode::Esc => {
                            execute!(std::io::stdout(), LeaveAlternateScreen)?;
                            disable_raw_mode()?;
                            return Ok(vec![]);
                        }
                        KeyCode::Enter => {
                            let final_items: Vec<GroupedItem> = items
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| selected[*i])
                                .map(|(_, item)| item.clone())
                                .collect();

                            execute!(std::io::stdout(), LeaveAlternateScreen)?;
                            disable_raw_mode()?;
                            return Ok(final_items);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = resolve_config();
    let client = create_client(&config.api_key)?;

    println!("--- Fetching Jellyfin users ---");
    let users = fetch_users(&client, &config.base_url)?;
    println!("Found {} users:", users.len());
    for user in &users {
        println!("  - {} ({})", user.name, user.id);
    }

    let user_names: HashSet<&str> = users.iter().map(|u| u.name.as_str()).collect();
    for name in &config.watched_by {
        if !user_names.contains(name.as_str()) {
            eprintln!("Error: watched-by user '{}' not found", name);
            std::process::exit(1);
        }
    }
    for name in &config.protected_by {
        if !user_names.contains(name.as_str()) {
            eprintln!("Error: protected-by user '{}' not found", name);
            std::process::exit(1);
        }
    }

    println!("\n--- Fetching all media items ---");
    let items = fetch_all_items(&client, &config.base_url)?;
    println!("Found {} items", items.len());

    let watched_spec = if config.watched_by.is_empty() {
        "all users".to_string()
    } else {
        config.watched_by.join(", ")
    };
    let protected_spec = if config.ignore_favorites {
        "none (ignoring favorites)".to_string()
    } else if config.protected_by.is_empty() {
        "anyone".to_string()
    } else {
        config.protected_by.join(", ")
    };

    println!("\n--- Finding deletable items ---");
    println!("  Must be watched by: {}", watched_spec);
    println!("  Protected by: {}", protected_spec);

    let matched_items = find_deletable_items(
        &config.api_key,
        &client,
        &config.base_url,
        &users,
        &items,
        &config.watched_by,
        &config.protected_by,
        config.ignore_favorites,
        config.min_days_watched_ago,
    )?;

    let grouped_items = group_items(&items, matched_items);
    let individual_count: usize = grouped_items.iter().map(|g| g.items().len()).sum();

    println!(
        "Found {} deletable items ({} groups)\n",
        individual_count,
        grouped_items.len()
    );

    if config.delete {
        let deleted = run_delete(&client, &config.base_url, &grouped_items)?;
        println!("\n=== Done: {} items deleted ===", deleted);
    } else if config.interactive {
        let selected = run_tui(grouped_items)?;
        if selected.is_empty() {
            println!("No items selected, nothing deleted.");
        } else {
            let mut all_items: Vec<String> = Vec::new();
            for item in &selected {
                match item {
                    GroupedItem::Individual(i) => {
                        all_items.push(format!(
                            "[{}] {}",
                            i.item_type,
                            format_display_name(i)
                        ));
                    }
                    GroupedItem::Season { items, .. } => {
                        for ep in items {
                            all_items
                                .push(format!("[Episode] {}", format_display_name(ep)));
                        }
                    }
                    GroupedItem::Series { seasons, items, .. } => {
                        let mut sorted_seasons = seasons.clone();
                        sorted_seasons.sort();
                        for season_num in sorted_seasons {
                            let season_eps: Vec<&MatchedItem> = items
                                .iter()
                                .filter(|e| e.season_number == Some(season_num))
                                .collect();
                            for ep in season_eps {
                                all_items
                                    .push(format!("[Episode] {}", format_display_name(ep)));
                            }
                        }
                    }
                }
            }
            all_items.sort();
            println!("\nItems to be deleted:");
            for name in &all_items {
                println!("  - {}", name);
            }
            let total = all_items.len();
            eprint!(
                "\nDelete {} items ({} groups)? (y/N) ",
                total,
                selected.len()
            );
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            if input.trim().eq_ignore_ascii_case("y") {
                println!("\n--- Deleting {} selected items ---", selected.len());
                let deleted = run_delete(&client, &config.base_url, &selected)?;
                println!("\n=== Done: {} items deleted ===", deleted);
            } else {
                println!("Deletion cancelled.");
            }
        }
    } else {
        run_dry_run(&grouped_items);
    }

    Ok(())
}
