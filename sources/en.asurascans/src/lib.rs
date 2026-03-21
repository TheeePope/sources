#![no_std]
use aidoku::{
	Chapter, ContentRating, DeepLinkHandler, DeepLinkResult, FilterValue, Home, HomeComponent,
	HomeComponentValue, HomeLayout, Link, Manga, MangaPageResult, MangaStatus, MangaWithChapter,
	Page, PageContent, Result, Source, Viewer,
	alloc::{String, Vec, string::ToString, vec},
	helpers::uri::QueryParameters,
	imports::{
		net::{Request, TimeUnit, set_rate_limit},
		std::parse_date,
	},
	prelude::*,
};

mod helpers;

const BASE_URL: &str = "https://asurascans.com";

struct AsuraScans;

impl Source for AsuraScans {
	fn new() -> Self {
		set_rate_limit(2, 2, TimeUnit::Seconds);
		Self
	}

	fn get_search_manga_list(
		&self,
		query: Option<String>,
		page: i32,
		filters: Vec<FilterValue>,
	) -> Result<MangaPageResult> {
		let mut qs = QueryParameters::new();
		qs.push("page", Some(&page.to_string()));
		if query.is_some() {
			qs.push("name", query.as_deref());
		}

		for filter in filters {
			match filter {
				FilterValue::Sort { id, index, .. } => {
					qs.set(
						&id,
						Some(match index {
							0 => "update",
							1 => "rating",
							2 => "bookmarks",
							3 => "desc",
							4 => "asc",
							_ => "update",
						}),
					);
				}
				FilterValue::Select { id, value } => {
					qs.push(&id, Some(&value));
				}
				FilterValue::MultiSelect { id, included, .. } => {
					qs.push(&id, Some(&included.join(",")));
				}
				_ => continue,
			}
		}

		let url = format!("{BASE_URL}/comics?{qs}");
		let html = Request::get(url)?.html()?;

		let entries = html
			.select("div.grid > a[href]")
			.map(|els| {
				els.filter_map(|el| {
					Some(Manga {
						key: el
							.attr("abs:href")
							.and_then(|url| helpers::get_manga_key(&url))?,
						title: el.select_first("div.block > span.block")?.own_text()?,
						cover: el.select_first("img").and_then(|el| el.attr("abs:src")),
						..Default::default()
					})
				})
				.collect()
			})
			.unwrap_or_default();

		let has_next_page = html
			.select_first("div.flex > a.flex.bg-themecolor:contains(Next)")
			.is_some();

		Ok(MangaPageResult {
			entries,
			has_next_page,
		})
	}

		fn get_manga_update(
		&self,
		mut manga: Manga,
		needs_details: bool,
		needs_chapters: bool,
	) -> Result<Manga> {
		let url = helpers::get_manga_url(&manga.key);
		let html = Request::get(&url)?.html()?;

		if needs_details {
			manga.title = html
				.select_first("h1, h2, h3")
				.and_then(|el| el.text())
				.unwrap_or(manga.title);

			manga.cover = html
				.select_first("img[alt=poster]")
				.and_then(|el| el.attr("abs:src"))
				.or_else(|| {
					html.select("img").and_then(|els| {
						els.filter_map(|el| el.attr("abs:src"))
							.find(|src| src.contains("/storage/") || src.contains("/covers/"))
					})
				});

			manga.description = html
				.select("p, div, span")
				.and_then(|els| {
					els.filter_map(|el| el.text()).find(|text| {
						let t = text.trim();
						!t.is_empty()
							&& t.len() > 60
							&& !t.contains("Join Discord")
							&& !t.contains("Home")
							&& !t.contains("Browse")
							&& !t.contains("Bookmarks")
							&& !t.contains("Rankings")
							&& !t.contains("Comics")
							&& !t.contains("Users")
							&& !t.starts_with("Chapter ")
							&& !t.starts_with("Ch.")
					})
				});

			manga.url = Some(url);
			manga.status = MangaStatus::Unknown;
			manga.viewer = Viewer::Webtoon;
		}

		if needs_chapters {
			let manga_prefix = format!("{}/chapter/", helpers::get_manga_url(&manga.key));

			let mut chapters: Vec<Chapter> = html
				.select("a[href]")
				.map(|els| {
					els.filter_map(|el| {
						let raw_url = el.attr("abs:href")?;

						if !raw_url.starts_with(&manga_prefix) {
							return None;
						}

						let key = helpers::get_chapter_key(&raw_url)?;
						let raw_text = el.text()?.trim().to_string();

						if raw_text.is_empty()
							|| raw_text == "First Chapter"
							|| raw_text == "Latest Chapter"
							|| raw_text.contains("Start Reading")
							|| raw_text.contains("Join Discord")
						{
							return None;
						}

						let chapter_number = if let Some(rest) = raw_text.split("Chapter ").nth(1) {
							rest.chars()
								.take_while(|c| c.is_ascii_digit() || *c == '.')
								.collect::<String>()
								.parse()
								.ok()
						} else if let Some(rest) = raw_text.split("Ch.").nth(1) {
							rest.chars()
								.take_while(|c| c.is_ascii_digit() || *c == '.')
								.collect::<String>()
								.parse()
								.ok()
						} else {
							None
						};

						let chapter_number = match chapter_number {
							Some(n) if n < 5000.0 => Some(n),
							_ => None,
						}?;

						Some(Chapter {
							key,
							title: Some(raw_text),
							chapter_number: Some(chapter_number),
							url: Some(raw_url),
							..Default::default()
						})
					})
					.collect::<Vec<Chapter>>()
				})
				.unwrap_or_default();

			chapters.sort_by(|a, b| {
				b.chapter_number
					.partial_cmp(&a.chapter_number)
					.unwrap_or(core::cmp::Ordering::Equal)
			});

			manga.chapters = Some(chapters);
		}

		Ok(manga)
	}

	fn get_page_list(&self, manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
		let url = helpers::get_chapter_url(&chapter.key, &manga.key);
		let response = Request::get(url)?.string()?;

		// Remove script tags from hydration that can cut up the page list
		let html_text = response.replace(r#""])</script><script>self.__next_f.push([1,""#, "");

		// Find bounds of the page list JSON
		let page_list_marker = r#"\"pages\":[{\"order\":1,\"url\":\"https://"#;
		let page_list_start = html_text.find(page_list_marker).unwrap_or(0);
		let page_list_end = html_text[page_list_start..].find(r#"}]"#).unwrap_or(0);

		let page_list_slice = &html_text[page_list_start..page_list_start + page_list_end];

		let mut pages = Vec::new();
		let mut search_start = 0;

		while let Some(pos) =
			page_list_slice[search_start..].find("https://gg.asuracomic.net/storage/media/")
		{
			let url_start = search_start + pos;
			let rest = &page_list_slice[url_start..];
			if let Some(url_end) = rest.find('"') {
				let url = rest[..url_end].replace("\\", "");
				pages.push(Page {
					content: PageContent::url(url),
					..Default::default()
				});
				search_start = url_start + url_end;
			} else {
				break;
			}
		}

		Ok(pages)
	}
}

impl Home for AsuraScans {
	fn get_home(&self) -> Result<HomeLayout> {
		let html = Request::get(BASE_URL)?.html()?;
		let mut components = Vec::new();

		// Trending Today
		let trending_entries: Vec<Link> = html
			.select("a[href*='/comics/']:not([href*='/chapter/'])")
			.map(|els| {
				els.filter_map(|el| {
					let href = el.attr("abs:href")?;
					let key = helpers::get_manga_key(&href)?;

					let raw_title = el.text()?.trim().to_string();

					// Clean titles like "9.8 Omniscient Reader’s Viewpoint"
					let title = if let Some((first, rest)) = raw_title.split_once(' ') {
						if first.chars().all(|c| c.is_ascii_digit() || c == '.') {
							rest.trim().to_string()
						} else {
							raw_title.clone()
						}
					} else {
						raw_title.clone()
					};

					if title.is_empty()
						|| title == "Home"
						|| title == "Browse"
						|| title == "Bookmarks"
						|| title == "Rankings"
						|| title == "Comics"
						|| title == "Users"
						|| title.starts_with("Chapter ")
					{
						return None;
					}

					let cover = el
						.parent()
						.and_then(|p| p.select_first("img"))
						.and_then(|img| img.attr("abs:src"))
						.or_else(|| el.select_first("img").and_then(|img| img.attr("abs:src")));

					Some(
						Manga {
							key,
							title,
							cover,
							..Default::default()
						}
						.into(),
					)
				})
				.collect()
			})
			.unwrap_or_default();

		if !trending_entries.is_empty() {
			components.push(HomeComponent {
				title: Some("Trending Today".into()),
				subtitle: None,
				value: HomeComponentValue::Scroller {
					entries: trending_entries,
					listing: None,
				},
			});
		}

		// Latest Updates
		let latest_entries: Vec<MangaWithChapter> = html
			.select("a[href*='/comics/']:not([href*='/chapter/'])")
			.map(|els| {
				els.filter_map(|el| {
					let href = el.attr("abs:href")?;
					let manga_key = helpers::get_manga_key(&href)?;

					let raw_title = el.text()?.trim().to_string();

					let title = if let Some((first, rest)) = raw_title.split_once(' ') {
						if first.chars().all(|c| c.is_ascii_digit() || c == '.') {
							rest.trim().to_string()
						} else {
							raw_title.clone()
						}
					} else {
						raw_title.clone()
					};

					if title.is_empty()
						|| title == "Home"
						|| title == "Browse"
						|| title == "Bookmarks"
						|| title == "Rankings"
						|| title == "Comics"
						|| title == "Users"
						|| title.starts_with("Chapter ")
					{
						return None;
					}

					let cover = el
						.parent()
						.and_then(|p| p.select_first("img"))
						.and_then(|img| img.attr("abs:src"))
						.or_else(|| el.select_first("img").and_then(|img| img.attr("abs:src")));

					let chapter_link = el
						.parent()
						.and_then(|p| p.select_first("a[href*='/chapter/']"))
						.or_else(|| {
							el.parent()
								.and_then(|p| p.parent())
								.and_then(|gp| gp.select_first("a[href*='/chapter/']"))
						});

					let chapter = if let Some(ch_el) = chapter_link {
						let chapter_href = ch_el.attr("abs:href").unwrap_or_default();
						let chapter_key = helpers::get_chapter_key(&chapter_href).unwrap_or_default();
						let raw_chapter_title = ch_el.text().unwrap_or_else(|| "Chapter".into());

						let chapter_number = raw_chapter_title
							.strip_prefix("Chapter ")
							.and_then(|s| s.split([' ', '-']).next())
							.and_then(|s| s.parse().ok());

						Chapter {
							key: chapter_key,
							title: Some(raw_chapter_title),
							chapter_number,
							url: Some(chapter_href),
							..Default::default()
						}
					} else {
						Chapter {
							key: "".into(),
							..Default::default()
						}
					};

					Some(MangaWithChapter {
						manga: Manga {
							key: manga_key,
							title,
							cover,
							..Default::default()
						},
						chapter,
					})
				})
				.collect()
			})
			.unwrap_or_default();

		if !latest_entries.is_empty() {
			components.push(HomeComponent {
				title: Some("Latest Updates".into()),
				subtitle: None,
				value: HomeComponentValue::MangaChapterList {
					page_size: None,
					entries: latest_entries,
					listing: None,
				},
			});
		}

		Ok(HomeLayout { components })
	}
}

impl DeepLinkHandler for AsuraScans {
	fn handle_deep_link(&self, url: String) -> Result<Option<DeepLinkResult>> {
		let Some(manga_key) = helpers::get_manga_key(&url) else {
			return Ok(None);
		};

		if let Some(chapter_key) = helpers::get_chapter_key(&url) {
			Ok(Some(DeepLinkResult::Chapter {
				manga_key,
				key: chapter_key,
			}))
		} else {
			Ok(Some(DeepLinkResult::Manga { key: manga_key }))
		}
	}
}

register_source!(AsuraScans, Home, DeepLinkHandler);
