# Sites

Moekura knows the sites Danbooru supports (listed
[below](#every-site)): wherever a link to one appears (a post's source,
an [artist's](artists.md#urls) URLs), it shows the site's icon and name,
and it can tell a work's page from an artist's profile and a file.

## What a link gives

[Uploading from a link](posts.md#uploading-from-a-link) to a work on one
of these sites reads the work for its files (the originals, not the
samples a page shows), the artist and their profiles, the site's tags
and the artist's commentary. A link to one of the work's files, even a
thumbnail, downloads the file at its best and makes the work's page the
post's source. Short links (`t.co`, `b23.tv`, `pin.it` and the like) are
followed to the page they lead to.

When a site has changed, refuses the server or wants a login it doesn't
have, Moekura falls back to the page's preview (OpenGraph) image when
that image is a file on a site it knows, and otherwise downloads the
link as it is.

Google's and Kakao's image servers (behind YouTube, Blogger, Tistory
and others), Skland and TikTok are only recognised: their files are
downloaded at full size, and their pages' preview images are read.

## Artist URLs

An artist entry's URLs on these sites are kept in their canonical form
(`artstation.com/artist/someone` becomes
`https://www.artstation.com/someone`), and finding an artist by URL
also tries the profile a work's page belongs to, so
`https://someone.deviantart.com/art/a-cat-123` finds the artist with
`https://www.deviantart.com/someone`.

## Logins

Some sites only show some works (or anything at all) to members. Give
Moekura an account's cookies, API key or token in the
[configuration](../configuration.md#sources), by domain; requests to that
domain and its subdomains carry them:

```toml
[sources.logins."pixiv.net"]
cookie = "PHPSESSID=…"

[sources.logins."gelbooru.com"]
query = { user_id = "…", api_key = "…" }

[sources.logins."baraag.net"]
headers = { Authorization = "Bearer …" }
```

Use an account made for this: anyone with access to the configuration
can act as it.

## X

X shows age-restricted posts only to logged-in accounts, and little at
all to servers. Moekura reads posts on X:

1. through the API of an [FxEmbed](https://github.com/FxEmbed/FxEmbed)
   instance, which needs no account here and sees age-restricted posts
   too. It's `https://api.fixupx.com` unless `[sources.x]` names another
   (such as `https://api.fxtwitter.com`, or your own):

   ```toml
   [sources.x]
   fxembed_api = "https://api.fxtwitter.com"
   ```

   Set it to `""` to not use one;
2. then, if that fails and there's a login for `x.com`, from X itself as
   that account. It needs the `auth_token` and `ct0` cookies, and the
   account must be allowed to see sensitive media (in X's settings):

   ```toml
   [sources.logins."x.com"]
   cookie = "auth_token=…; ct0=…"
   ```

   X changes how its API is asked from time to time, which can break this,
   and suspends accounts it finds reading it automatically;
3. last, through the API X's own embedded posts use, which only sees
   public posts that aren't age-restricted.

When none of them gives the post, the upload is refused, saying it may not
exist, may be hidden or may be age-restricted; the server's log says what
each answered.

## Every site

### Boorus

| Site | Notes |
|---|---|
| Danbooru | the post's own source fills in the artist and commentary, as on every booru |
| e621 |  |
| Gelbooru | its API wants a login: `query = { user_id = "…", api_key = "…" }` (without one, the page's preview image) |
| Konachan |  |
| Rule34.us |  |
| Rule34.xxx | its API wants a login: `query = { user_id = "…", api_key = "…" }` |
| Safebooru |  |
| TBIB |  |
| Yande.re |  |
| Zerochan | the `z_id` and `z_hash` cookies help |

### Pixiv's sites

| Site | Notes |
|---|---|
| Pixiv | some works only show to members: the `PHPSESSID` cookie |
| Pixiv Comic |  |
| Pixiv Factory |  |
| Pixiv Sketch |  |
| pixivFANBOX | posts for supporters have no files for visitors |
| Booth |  |

### Misskey, Mastodon and Bluesky

| Site | Notes |
|---|---|
| Misskey | any Misskey instance, for its `/notes/<id>` links |
| Misskey.io |  |
| Misskey.art |  |
| Misskey.design |  |
| Pawoo | some statuses need an access token: `headers = { Authorization = "Bearer …" }` |
| Baraag | some statuses need an access token: `headers = { Authorization = "Bearer …" }` |
| Bluesky |  |

### Other sites

| Site | Notes |
|---|---|
| 4chan |  |
| Adobe Portfolio |  |
| Apple Music |  |
| Arca.live |  |
| Artistree |  |
| ArtStation |  |
| ArtStreet | mature works need the `MSID` cookie |
| Behance | often refuses servers; the `iat0` cookie helps |
| Bilibili |  |
| Blogger |  |
| Carrd |  |
| Ci-En | articles for supporters need the `ci_en_session` cookie |
| DC Inside |  |
| DeviantArt |  |
| Dotpict |  |
| Fandom |  |
| Fantia | needs the `_session_id` cookie |
| FC2 |  |
| Foriio |  |
| Furaffinity | mature works need the `a` and `b` cookies |
| Galleria |  |
| Grafolio |  |
| Gumroad |  |
| Hentai Foundry |  |
| Huajia |  |
| Huashijie | needs your `userId` and `token`: `query = { userId = "…", token = "…" }` for app.huashijie.art |
| Imgur |  |
| Inkbunny | members-only works need a session id: `query = { sid = "…" }` |
| Itaku |  |
| Ko-fi |  |
| Lofter |  |
| Mihuashi |  |
| Minitokyo |  |
| Miyoushe |  |
| Naver Blog |  |
| Naver Cafe |  |
| Newgrounds | mature works need the `ng_remember` cookie |
| Nico Seiga | needs the `user_session` cookie |
| Nijie | needs the `NIJIEIJIEID` and `nijie_tok` cookies |
| Note |  |
| Odaibako |  |
| OpenSea |  |
| Patreon | posts for patrons need the `session_id` cookie |
| Piapro.jp | the illustration as shown; downloads need the `piapro_s` cookie |
| Pinterest |  |
| Plurk |  |
| Poipiku | most posts need the `POIPIKU_LK` cookie |
| Postype | paid and adult posts need the `PSE3` cookie |
| Privatter |  |
| Reddit | often refuses servers; the `reddit_session` cookie helps |
| Redgifs |  |
| Skeb |  |
| Tinami | originals need the `Tinami2SESSID` cookie |
| Tistory |  |
| Toyhouse |  |
| Tumblr | an API key reads more: `query = { api_key = "…" }` for api.tumblr.com |
| Vk |  |
| Weibo | read as a visitor |
| X | read through an FxEmbed instance; see [X](#x) |
| Xfolio | needs the `xfolio_session` cookie |
| Xiaohongshu | needs the `webId`, `web_session` and `gid` cookies |
| Yachiyo's Room |  |
| Youtube |  |
