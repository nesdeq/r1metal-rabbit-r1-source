# News

Every day at the hour the settings set the device makes an edition: a rundown for each rundown the owner keeps and has made, one of the day's news and one of finance on a new device, each written by the news' model from the stories of its own feeds, stored as Markdown and PDF, and read on the device. The owner adds, changes and deletes rundowns by a request said on their list or to the robot. Screens: [Interface](interface.md#notes-and-texts), [Rundowns](interface.md#rundowns).

| Fact | Value |
|---|---|
| When | `EDITION` under `FEATURES`, `NEWS`: an hour of the on-screen clock, daily, asleep or awake, 08:00 on a new device; or never (`Edition`, kept as `news 8` or `news off`) |
| Rundowns | `RUNDOWNS` under `FEATURES`, `NEWS`: the rundowns the owner keeps, each made with every edition or off; `news` and `finance` on a new device, both made ([The rundowns kept](#the-rundowns-kept)) |
| Model | `MODEL` and `REASONING` under `FEATURES`, `NEWS`, for a day's rundowns; `REQUEST MODEL` and `REQUEST REASONING` there, for the reading of a request for the rundowns; each `gpt-6.1-terra` at `high` on a new device ([OpenAI](openai.md#language-models)) |
| Sources | the firmware's 21 news feeds and 26 finance feeds; the feeds a rundown keeps of its own |
| Files | `news/yyyymmdd/yyyymmdd_NAME.md` and `.pdf` for each rundown made, `NAME` its name (`yyyymmdd_news.md`, `yyyymmdd_finance.md`) |
| Reading | `NEWS` on the top level of the screen tree; touching the robot over a rundown reads it aloud ([Audio](audio.md#reading-aloud)). Once today's edition holds a rundown's Markdown (`memos::has_rundown`), home's news line shows `TODAY'S READY` and Companion's greeting is `Today's news is ready.` |

## The rundowns kept

`r1_funcore::domain::news::topics` holds the rundowns as the owner keeps them (`Topics`, each a `Topic`): a rundown's name, whether it is made, what it is about in the owner's words, its icon, and its feeds. The interface holds them (`Ui::kept_rundowns`). The settings' thread reads them at the start and hands them over (`Topics::read`, `Ui::recall_rundowns`) and writes their file whenever they change (`Ui::take_changed_rundowns`, `Keeper::keep_rundowns`); the main loop hands the editor each state of them (`Editor::set_topics`), and no edition is made before it has them.

Their file is `.r1metal/interface/rundowns` ([Storage](../firmware/storage.md#layout)), a line each:

| Line | Tells |
|---|---|
| `rundown NAME on\|off` | a rundown, and whether an edition makes it; the rundowns stand in the order of these lines, and of a name's lines the last that reads counts |
| `about NAME WORDS` | what the rundown is about, on one line: the subject, what to stress or leave out, the language, the length |
| `icon NAME WORD` | the icon its lines show it by: one of the 82 of `topics::ICONS`, by its name among Material Symbols (`sports_motorsports`); a word that is none of them is passed over |
| `feed NAME LINE` | a feed of its own, as a line of a feeds list ([Feeds](#feeds)): a URL, or a name, a space and a URL |

- **Names.** A name is one word of lowercase letters and digits that begins with a letter (`paper::names::is_name`). It names the rundown on screen, in capitals, in its day's files and in the log. A line whose name is none, and an `about` or `feed` line of a rundown without a `rundown` line, is passed over.
- **A new device** keeps `news` and `finance`, both made (`Topics::default`, `STANDARD`). While one of the two keeps no feed of its own it reads the firmware's list of its name, and while the owner says nothing of it its instructions are the firmware's (`Topic::feed_lines`, `Topic::task`). The two switches an earlier firmware kept among the settings (`rundown news on`) are read once, while the rundowns have no file, and kept in the rundowns' file from then on (`Topics::read`).
- **Any other rundown** keeps its own feeds and is written under `crates/funcore/assets/news/topic.txt`, which holds its name and the owner's words of it (`{NAME}`, `{ABOUT}`); one of the two a new device has that the owner tells about is written under it too.
- **Icons.** `topics::ICONS` holds the icons a rundown may go by, each under its name with its drawing, of the subjects a rundown is commonly about: the world, politics, markets, technology, science, each sport, the arts, travel, the weather. The model that reads a request picks a new rundown's from their names ([Requests](#requests)). A rundown that keeps none, as `news` and `finance` on a new device and one an earlier firmware kept, goes by `finance` where that is its name, else by `newspaper` (`Topic::drawing`); so does a rundown of an earlier edition that is kept no longer (`Topics::drawing_of`). The rundowns' list and a day's page show it ([Interface](interface.md#rundowns)).
- **Changes** (`Topics::switch`, `put`, `remove`, `remove_feed`). A rundown is switched, kept in the place of the one of its name or behind the others, or taken away. A feed is taken away by its place among the rundown's lines, never its last; one taken from the firmware's list leaves the rest of that list as the rundown's own. One of the two a new device has that is kept no longer is kept again as a new device has it, from its line on the list ([Interface](interface.md#rundowns)).
- **Silent feeds.** After every gathering the editor keeps, for each rundown it read, the list lines of the feeds that gave no story, in place of what it kept of that rundown: `.r1metal/news/silent`, a line `feed NAME LINE` each (`r1_funcore::domain::news::silent`, `State::note_silent`). Where that changed it tells the interface in its report (`Report.silent`, `Ui::take_editor_report`): the rundowns' screens mark those feeds, and a request is read against them. Only a feed its rundown reads now counts as silent on the list and is told to the model (`Silent::count_in`, `Silent::to_told`): what the file keeps of a feed replaced or removed since, or of a rundown deleted, shows nowhere.
- **The log** tells how many are kept and made as they are read and as they are written: `news: 3 rundowns kept, 2 of them made, as kept`, `news: kept 3 rundowns, 2 of them made`.

A change has the editor try an edition as a news screen that opens does (`State::set_topics`, [Schedule](#schedule)): a rundown kept or switched on once the day's edition is due is made the same day. Nothing deletes an edition's files: a rundown deleted or switched off stays in the days it was made on.

## Requests

The owner changes the rundowns by a request said on their list ([Interface](interface.md#rundowns)) or handed over by the robot (`change_rundowns`, [Chat](chat.md#functions)): a new rundown on a subject of theirs, or one the device keeps changed, switched on or off, or deleted. Code: `r1_funcore::domain::news::request`; the threads `rundowns` and `rundowns asked`, `crates/impshell/src/news/request.rs` (`Curator`, whose worker is `Taker`).

The job is a job said as the bell's is ([Interface](interface.md#the-bell)), wanted while the rundowns' list or a rundown's page shows and the device is awake (`Ui::are_rundowns_wanted`). Only the newest request is read (`Keep::Newest`), and one in words, the robot's, holds the network through the key as one said does (`Job::IS_KEYED`). With each request the interface hands the job the rundowns as they stand and the silent feeds (`request::Standing`, `State::set_standing`).

1. **Written down** (`TRANSCRIBING`). The speech model writes a request said down, prompted that it is a request about the rundowns of a daily news edition (`request::PROMPT`, [OpenAI](openai.md#models)).
2. **Read** (`THINKING`; `write::write_rundown_lines`). One background request at the request's model and effort (`REQUEST MODEL` and `REQUEST REASONING` under `FEATURES`, `NEWS`; `gpt-6.1-terra` at `high` on a new device, [OpenAI](openai.md#language-models)) under `crates/funcore/assets/news/request.txt`, the web search among its tools, told where the device is where the place service said so, as a step of the browser's is. Its input (`Standing::to_input`) is `The rundowns the device keeps:` over their file's lines with every feed each reads, the firmware's too (`Topics::to_told`), or `The device keeps no rundown.`; then, where there are any, `Feeds that gave no story when the device last read them:` over the lines of the silent feeds the rundowns still read (`Silent::to_told`); then `Request: ` and the request on one line. The task (`request::task`) has the model search the web for a new rundown's feeds, RSS or Atom at `https` addresses, or give the address of a site's page on the subject where it knows no feed, and for the new place of a feed that gave no story, and pick the icon that fits a new rundown's subject best among the names it lists, those of `topics::ICONS` in the place of `{ICONS}`. It answers in the lines of the rundowns' file, `rundown NAME on|off`, `about NAME WORDS`, `icon NAME WORD` and `feed NAME ADDRESS`, and `delete NAME`, or `NOTHING`.
3. **The changes** (`request::Reading`, `Standing::parse_reply`, `Change`). From the reply on funcore holds the request's progress: it hands the device the feeds to read for each rundown that adds any (`Step::Read`), takes what they gave (`Reading::take_found`) and ends with the count of the changes asked and those that stand (`Step::Done`). Each rundown the reply names is that rundown as it is kept from then on (`Change::Put`). Of one the device keeps, what the reply leaves out stays: its words, its icon, and with no feed line its feeds; its feed lines are every feed it reads from then on. `delete NAME` deletes a rundown the device keeps (`Change::Delete`). What changes nothing is passed over.
4. **The feeds read** (`SEARCHING`; `gather::check_feeds`). The device reads every feed a change adds to what its rundown reads (`Standing::added_feeds`), at once, as a gathering reads them ([Gathering](#gathering)), and follows one thing more: a page that is no feed and names one as its own, in a `link` of `rel` `alternate` and type `application/rss+xml` or `application/atom+xml` (`document::find_feeds`), sends the device on to the first such feed as a redirect does (`document::to_outcome`, `FetchOutcome::Named`), and that feed's address is kept in the page's place (`fetch::to_found`). A feed that gives no story is left out and logged (`news: a feed asked for is left out: example.org: status 404`), and no feed is kept twice, whether the reply lists it twice or a page names one the rundown reads. A change left with no feed, or with nothing new, is none (`Standing::settle`).
5. **Kept.** What stands goes to the main loop in the job's report (`request::Report.changes`), and the interface takes each (`Ui::take_rundowns_report`). A rundown, new or changed, is kept at once, whatever it costs: a new one, other words, another icon, more feeds or fewer, one switched on or off. A deletion alone shows on the rundown's page for the owner's click ([Interface](interface.md#rundowns)). A request that asks no change fails as `NO ANSWER`; one whose changes all fell away with their feeds as `NO FEED FOUND` (`Cause::NoFeed`).

The log tells `news: a request for the rundowns asks 2 changes, 1 of them stand with their feeds read`, and the reading as all background work is logged, by its characters, its time and its tokens (`openai: a request for the rundowns read in 214 characters in 9120 ms, asked after 4 times`, [OpenAI](openai.md#models)), never its words. A request is booked under `NEWS` with the rundowns and counts none: the reading at its tokens and a cent for each web search, one that came to nothing as far as the service counted it, and the writing down of a request said (`Entry::work(Kind::Rundown)`, `Subject::RundownsRequest`, [Interface](interface.md#cost)). The robot listens while a request is said, thinks while it is written down and read, and searches while the feeds are read. A recording or a request in progress keeps the device awake.

## Feeds

`crates/funcore/assets/news/news.feeds` and `finance.feeds` hold one feed per line: a URL, or a name, a space and a URL for stories that belong to someone else. Every URL is `https`, which a test checks. News covers technology, science, world affairs, ideas and essays; finance covers markets, business, the economy, central banks and crypto. A feed a rundown keeps of its own is a line of the same form (`Feed::parse`, `Feed::to_line`). A story's source is the server name's last two labels without `www.` (`arstechnica.com`), a Feedburner feed's first path segment (`zerohedge`), or a named line's name.

| Feed | Its own URL returns | Read from |
|---|---|---|
| VentureBeat | 429 behind a JavaScript check | a Google News search for its stories of the last two days, named `venturebeat.com` |
| Symmetry | 403 | its Feedburner copy |
| Yahoo Finance | 404 | its S&P 500 headline feed |

`r1_funcore::domain::news::document` parses RSS 2.0 and RDF items and Atom entries with the tokenizer in `r1_funcore::base::markup` (CDATA, comments, named and numeric entities), without building a tree. A document is UTF-8 unless its first line declares Latin-9, or Latin-1 or Windows-1252, which are both read as Windows-1252, as browsers do. In the document and in a field's escaped markup alike, `<` starts a tag only before a letter, `/`, `!` or `?`, as in HTML's tokenizer, a tag ends at the first `>` outside a quoted value, and a comment at its own close (`markup::begins_tag`, `markup::markup_length`). An element inside an entry ends at its own closing tag whatever is left open inside it, and an entry ends at its own, so unclosed markup costs one element and no later story.

| Field | From |
|---|---|
| Title | `title` |
| Link | the text of `link`; in Atom the `href` of the alternate link (`rel` absent or `alternate`, type absent or HTML); else a `guid` that is a URL. A link starting with `/` gets the server the document was read at, which after a redirect or a page that names its feed is another than the feed line's (`Target::to_origin`) |
| Published | `pubDate`, `published` or `issued`; else `updated`, `modified` or `dc:date` |
| Summary | `description` or `summary`; else inline text `content` or `content:encoded` |

Each field becomes one plain line: entities decoded, tags removed, whitespace collapsed. A field whose `type` is `text` or `xhtml` (RFC 4287, 3.1) has no escaped markup and only has whitespace collapsed; one without `type` is read as escaped HTML, as in RSS. Missing fields read `No title`, `No date` and `No summary available`. Elements in other namespaces are skipped; a truncated document yields what it contains.

`TopicStories::to_report` in `r1_funcore::domain::news::stories` builds the model's input for a rundown. It starts with a sources line (`stories::sources_line`, sources sorted and deduplicated), such as `Sources (21/21 feeds, 682 stories): aeon.co, arstechnica.com, ...`. Then follows every story of every feed in list order, uncut, as `Title:`, `Source:`, `Link:`, `Published:` and `Summary:`, with a blank line between stories. Without stories there is no report and no request.

## Gathering

`crates/impshell/src/news/gather.rs` fetches the feeds of every rundown to make at once (`fetch::to_feeds`), as many together as the two rundowns of a new device read, 47, and the rest in turns of as many (`fetch::most_at_once`), so that no list, one a model's reply made among them, sets how many threads and connections stand together. Each turn goes in rounds (`fetch::Rounds` in `r1_funcore::domain::news`):

1. All server names still needed are resolved over one connection ([Network](../firmware/network.md#dns)), because a resolver queried over many connections at once drops some.
2. Each feed is fetched on its own `feed` thread with a `GET` and the `fetch::HEADERS` headers: `User-Agent: r1metal/` plus the workspace version (more feeds answer this than a browser's), `Accept: application/rss+xml, ...`, `Accept-Encoding: identity`, `Connection: close`.
3. A redirect (301, 302, 303, 307 or 308 with `Location`, resolved by `Target::resolve`) is followed in the next round, at most `net::fetch::MAX_HOPS` (3) times, and only to an `https` address.

A feed yields nothing (`net::fetch::check_head` and `check_read` with a `net::fetch::Fault` reason, the rounds and `document::parse_document` with a `FeedError`) for: a URL that does not parse; a redirect to an invalid address or beyond `MAX_HOPS`; a status other than 200; a `Content-Encoding` other than `identity` (nothing is decompressed); more than `net::fetch::MAX_BYTES` (4 MiB); a document without stories; no complete response 30 s after the request to one address (`net::fetch::TIMEOUT`). The browser fetches a page under the same checks of an answer's head, size and time and the same count of redirects, which `r1_funcore::net::fetch` holds for both ([Interface](interface.md#web)). The log names each feed that yielded nothing and why, how many answered per rundown (`news: 20 of 21 feeds of news answered, with 652 stories`), the time taken, the largest document and the most redirects. The feeds that yielded nothing are kept as their rundown's silent feeds ([The rundowns kept](#the-rundowns-kept)). A day's gathering reads a feed at its own address and follows no page to a feed it names; only the reading of the feeds a request adds does ([Requests](#requests)).

Every morning the feed servers, Google (for VentureBeat) and the resolver see the device's network address; OpenAI receives the stories.

## The day's rundowns

The rundowns to make are requested at once, each on its own `rundown` thread: `POST /v1/responses` to the news' model at its reasoning as a background request (`r1_funcore::openai::summary::request` over `stories::to_input`, `write::ask_rundown` in `crates/impshell/src/openai/write.rs`). The `instructions` come from `Topic::task`, plus the reasoning modules: `crates/funcore/assets/news/news.txt` or `finance.txt` for the two a new device has while the owner says nothing of them, else `topic.txt` with the rundown's name and the owner's words of it. The `input` is `Date: YYYY-MM-DD` and the report (`stories::to_input`).

The instructions ask for a rundown for a reader who has seen none of the stories. It is thorough, grouped by theme under second level headings, most important first. A story covered by several sources is reported once; names and figures are bold; the source is named where it matters who reports; older stories weigh less; advertisements are left out. It states only what the stories say; for finance it gives figures as reported, no advice and no forecasts beyond the stories' own. It is in English with plain punctuation, using only the Markdown the text screen renders: paragraphs, `##` headings, hyphen lists, bold, and italics for titles of works. A rundown the owner tells about also leaves out what has nothing to do with what they say of it, and is written in the language their words name and as long or as short as they ask.

A rundown is booked when its response is read, from the service's counts; one that ended without text is booked as far as the service counted it ([OpenAI](openai.md#ledger)). `r1_funcore::domain::paper::sheet` writes its documents to `news/yyyymmdd/`, named by the day and the rundown's name (`names::rundown_file`):

| File | Content |
|---|---|
| `yyyymmdd_NAME.md` | a heading of the name, its first letter a capital, and the day (`# News of 13 October 2026`, `# Finance of ...`; `dates::to_rundown_heading`), a sentence stating what the file is and the model its answer names, the sources line, a rule, and the rundown as written. Written first, as soon as the response is read: it is what was paid for, and its existence marks the rundown as made |
| `yyyymmdd_NAME.pdf` | the rundown in the interface's look ([Audio](audio.md#documents)), with the name in capitals (`NEWS`, `FINANCE`) and the day (`tuesday 13 october 2026`) in the bar and the Markdown file's name and the page number in the footer |

A day's edition is every file of its folder that is the day, `_`, a rundown's name and `.md` (`names::rundown_name`), whichever rundowns the owner keeps by now; they are listed with the two a new device has first, then by name (`memos::read_rundowns`, `topics::compare`).

## Schedule

The news editor (the `news` thread, `crates/impshell/src/news/mod.rs`, lowest priority) sets the clock alarm to the next edition's hour on the on-screen clock (`Edition::next_alarm` in `r1_funcore::domain::news`), or, with no daily edition, to the next poll alone, and clears it where there is none. It resets the alarm whenever the clocks are set, the time zone offset changes or the settings change the edition (`Editor::set_schedule`). Nothing is due before the settings and the rundowns the owner keeps are read (`State::take_request`). `state::State` in `r1_funcore::domain::news` carries the messages between the thread and the main loop; `editor::Schedule` decides when the editor tries on its own.

| Trigger | Action |
|---|---|
| The alarm | makes the day's edition, in sleep too ([Sleep and clock](../firmware/sleep.md#the-alarms-list)) |
| Start | makes an edition that is past its hour and incomplete |
| A news screen opens | the same |
| The rundowns the owner keeps change | the same |
| `news` on the console | makes the day's edition again whatever the hour and the files, of the rundowns that are made, except one already in progress |

On its own (alarm or start) the editor tries each day once: it writes the day to `.r1metal/news/tried` before it turns on the radio or calls a model (`EditionRequest::is_noted`, `editor::note_try`), so no outcome, a restart included, makes it pay twice. A news screen, a change of the rundowns and the console try whenever they ask. A try does nothing when the edition has every rundown that is made, or its hour has not come, or there is no daily edition (`Rundowns::missing`, over `Topics::made`). Otherwise the editor notifies the main loop that it has work, waits for permission, and then:

1. Reads the key; without one, it stops.
2. Holds the network. It reads the date from the clock only now, since the first stay sets the clocks.
3. Finds what is missing (`Rundowns::missing`): a rundown whose Markdown exists, or one in progress, is not requested again.
4. Fetches the feeds of the missing rundowns, keeps which of them gave no story, and sends the requests.
5. Stores the response ids on the partition and releases the network; the device may sleep.
6. Stores each rundown as its two files at the first poll that finds it ready.

Meanwhile a news screen shows `GATHERING` and `WRITING`, and the robot nods when a rundown is stored. A try that stored no complete rundown shows `NO NETWORK`, `NO OPENAI KEY`, `LIMIT REACHED`, `REFUSED` or `NOT WRITTEN`: the failure of the first rundown that failed, or of the attempt (`editor::EditionResult`). A newer attempt's failure replaces one the screen has not shown yet.

A `news` thread that can wait no more, its `epoll_wait` failing, ends until the app starts again, as Signal's does: from then on no edition is made or looked after, nothing is asked of it, and nothing shows as gathered or written (`State::end_thread`).

## Fetched later

The network is not held while the model writes ([OpenAI](openai.md#models)): a poll is one request in a wake, where a link held through the writing would keep the radio up for as long as the model writes.

1. During the edition's stay the editor fetches the feeds and sends the requests; each response is `queued` or `in_progress` with an id (`resp_...`).
2. Each id is written at once to `.r1metal/news/pending` (`r1_funcore::domain::news::rundowns`, via `Data::put`): one tab-separated line per rundown with its name, id, request time in UTC seconds, day and sources line. A newer request for the same day and rundown replaces its line; a damaged line, one whose first field is no rundown's name among them, is dropped. An id that cannot be stored is kept in memory all the same and polled as the others, until a restart.
3. With the requests sent, the editor releases the network and notifies the main loop, so the device may suspend; a news screen still shows `WRITING`. The single alarm is set to the earlier of the next edition and the next poll (`Edition::next_alarm`).
4. `FIRST_LOOK_S` (120 s) after the request, the alarm wakes the device. The editor holds the network and polls every pending rundown over one connection (`write::WorkPoller`). A finished rundown is booked, stored and removed from the file. A rundown still `queued` or `in_progress`, which the log tells of each poll, or whose poll failed or got 429 or 5xx, is polled again every `SLOW_POLL_SECONDS` (30 s) through `PATIENCE_SECONDS` (300 s) from the request. One still on its way at the poll at 300 s is cancelled at OpenAI (`POST /v1/responses/ID/cancel`, `WorkPoller::cancel`) and, where the owner keeps it and has it made still, asked for anew in the same stay, its feeds read again, once a day for each rundown, which the tried file notes (`editor::note_anew`); one asked anew before, or kept or made no more, is `NOT WRITTEN`. `failed`, `cancelled`, `incomplete`, an unknown id or a missing key ends it at once (`Poll::Failed`). What each poll means for its rundown is funcore's (`Rundowns::take_look`, a `rundowns::Seen`: written, on its way still, ended or overdue), and so is which overdue rundown is asked for anew (`rundowns::to_anew`); the editor's thread stores, logs, cancels and asks as they say.
5. After a restart the editor reads the file and polls each rundown when due, at once if overdue; one older than 300 s gets one poll. Nothing is requested twice: the day is recorded first, and a pending rundown never counts as missing.
