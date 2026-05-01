# media-purger

Delete watched-but-not-favorited media from Jellyfin to free up space.

The tool finds media that every specified user has watched but no specified user has marked as favorite, then gives you a way to delete it. It handles episodes, seasons, and series grouping so you can see and delete by season or series.

## Build

```
cargo build --release
```

The binary ends up at `target/release/media-purger`.

## Setup

You need two things: your Jellyfin URL and an API key.

To get an API key, log into Jellyfin as an admin and go to Dashboard > API Keys.

Set them in your shell or a `.env` file:

```
JELLYFIN_URL=https://jellyfin.example.com
JELLYFIN_API_KEY=your-api-key-here
```

Or pass them on the command line with `--jellyfin-url` and `--jellyfin-api-key`.

## Usage modes

### Dry-run (default)

Lists what would be deleted without deleting anything:

```
./media-purger
```

### Interactive TUI

Lets you pick which items to delete:

```
./media-purger --interactive
```

Use space to toggle selection, enter to confirm, q to quit.

### Actually delete

```
./media-purger --delete-watched-but-not-favourited-yes-i-am-really-sure
```

The long flag name is intentional. You have to type it out.

## Options
`--jellyfin-url` / `JELLYFIN_URL` - Jellyfin server URL.
`--jellyfin-api-key` / `JELLYFIN_API_KEY` - Jellyfin API key.
`--watched-by`, `-w` - Users who must all have watched an item for it to be considered. Default is all users.
`--protected-by`, `-p` - Users where any having favorited an item protects it from deletion. Default is all users.
`--min-days-watched-ago N` - Only include items not watched in the last N days.
`--ignore-favorites` - Delete even if favorited. Cannot be used with `--protected-by`.
`--interactive`, `-i` - Interactive TUI mode.
`--delete-watched-...` - Actually perform deletion. Default is dry-run.

## Examples

Delete items watched by alice and bob, regardless of favorite status:

```
./media-purger --watched-by alice bob --ignore-favorites
```

Delete items that alice has watched and bob has not favorited:

```
./media-purger --watched-by alice --protected-by bob
```

Delete items all users have watched and no user has favorited, if watched more than 30 days ago:

```
./media-purger --min-days-watched-ago 30 --delete-watched-but-not-favourited-yes-i-am-really-sure
```

Delete everything regardless of favorite status:

```
./media-purger --ignore-favorites --delete
```

## How it works

1. Fetch all users and all media items from Jellyfin.
2. For each item, check if every user in `--watched-by` has it in their played history.
3. Check if any user in `--protected-by` has it (or a parent item) in their favorites.
4. Skip items that are protected.
5. Group episodes into seasons, then seasons into series when all items in that group are deletable.
6. Show or delete the results.

## Requirements

Rust edition 2024. Install via rustup if you do not have it:

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```