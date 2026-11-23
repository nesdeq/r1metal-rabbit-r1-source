# OpenAI

Every paid service r1metal uses is OpenAI's, called with the owner's API key (`.r1metal/openai/key`, [Storage](../firmware/storage.md)). `r1_funcore::openai` builds requests and parses responses, one file per service. `crates/impshell/src/openai/` sends them:

| File | Does |
|---|---|
| `mod.rs` | `Account`: key, ledger (below) |
| `request.rs` | requests over the connections a stay leaves open |
| `session.rs` | WebSocket sessions |
| `transcript.rs` | the connection a recording's words go over, made ahead (`transcript::prepare`); transcription, whole or streamed: the one `transcript::transcribe` every recording is written down by, a note's, a name said to the Pokédex, a request said to the browser, a request said to the bell, a request said for the rundowns, a message said for Signal and one said to the robot, each named in the log and booked as its `transcript::Subject` says |
| `write.rs` | the language model's background work: summaries, recollections, rundowns, Signal's auto mode, the browser's steps, the reading of a request to the bell and of one for the rundowns |
| `chat.rs` | the conversation's turns, and their WebSocket opened ahead for a message being recorded |
| `image.rs` | pixel cam |
| `live.rs` | translator sessions |
| `aloud.rs` | reader sessions |
| `keys.rs` | the `keys` thread, which checks and stores the key |

## The key

The key file holds one line: the key as OpenAI issued it. Only a string shaped like a key is checked: `sk-` followed by letters, digits, `-` and `_` (`parse_key` in `r1_funcore::openai::key`). The check is `GET /v1/models` (`models::check::ADDRESS`) with the key as bearer token, which costs nothing; `parse_verdict` reads the response:

| Response | Meaning | Key |
|---|---|---|
| 200 | works | accepted |
| 401 with code `invalid_api_key` | unknown to OpenAI (mistyped, deleted, revoked) | rejected |
| other 401 (such as `missing_scope` for a restricted key), or 403 (such as a key without the permission to list the models, whose error is its words alone) | known to OpenAI | accepted |
| anything else, or no response | undecided | not stored |

| Source | Checked by | Stored |
|---|---|---|
| `r1 flash`, pasted or typed, unless the R1 already runs r1metal with a key and the flash erases no user data: neither `--wipe` nor an unlock ([r1metal](../../README.md#use)) | `r1`, before anything is written | in the boot image's `flashed` file ([Logs](../firmware/logs.md#the-logging-switch)); the device stores it on the first boot of that flash, replacing any key |
| `r1 key` | `r1` | sent over the console (`key KEY`, [Console](../firmware/console.md#verbs)), replacing any key |
| QR code from `r1 key --qr`, scanned in the key window ([Interface](interface.md#the-openai-key)) | the device, over its network | if accepted, replacing any key |

**On the computer** (`crates/flasher/src/key/`):

- Input is not echoed; Ctrl-C restores the terminal. If the terminal cannot hide input, `r1` says so before the prompt.
- At a terminal, `r1` prompts again until a key is accepted or the line is empty. With non-terminal input it reads the first line, and a rejected key is an error.
- The check uses its own TLS (`rustls` with `aws-lc-rs`, roots from `webpki-roots`) and sends the device's request: its headers from `request::headers` and the bearer from `key::to_bearer` in `r1_funcore::openai`, and it accepts a key where `Verdict::should_store` does. Timeout: 10 s per connection attempt and per read or write (`TIMEOUT` in `r1_funcore::net::race`). At most 64 KiB of the response is read. `r1` prints the key's length, never the key.
- Before a flash that does not wipe, `r1` asks a running r1metal whether it has a key and waits 2 s for the answer (`Console::ask_at_once`). Whether the flash erases user data is decided once the R1 is in fastboot, by `--wipe` and by whether the bootloader has to be unlocked, and `r1` prompts for a key then, before anything is written, where that erases the key or the R1 did not say in time that it keeps one; an R1 whose bootloader may not be unlocked is refused before that.
- `r1 key --qr` draws the key as a QR code: one byte-mode segment, error correction level M (`LEVEL`, about 15 % recoverable, for a photo of a screen with glare and moiré), smallest fitting version (version 9, 53 modules, holds 180 bytes), dark on bright white, 4 module quiet zone, 122 × 61 characters at version 9. Every module is two spaces in its colour as their background, one line to a row of modules (`ACROSS_MODULE` in `terminal.rs`). Every terminal fills a character's whole cell with its background, whereas block glyphs depend on the terminal and the font: macOS's Terminal draws them in part of the line only, so half blocks leave slivers of the other module's colour that break the finders. While the window is smaller than the code and the two lines below it, `r1` asks for it larger and waits for Enter. The code and the scrollback contain the key in clear text.

**On the device** the `keys` thread stores a new flash's key once the settings are read, then reports to the UI whether a key is stored, a partition that cannot tell counting as none (`key::Report::with_start`); while none is, the key window prompts for one. It checks a scanned key over the device's network and stores it if accepted, and a key so stored is reported as stored whether or not the window still waits for that check (`key::Report::from_check`). A console key is stored unchecked (`r1` checked it) and acknowledged once written. The log records check status and storage, never the key.

**A key rejected later.** A 401 with `invalid_api_key` on any request or session with the stored key (HTTP, WebSocket opening, or the conversation's socket) makes the `keys` thread check `/v1/models` again. If that also rejects the key, the key is deleted (`Verdict::should_remove`) and the key window opens over home with `KEY REFUSED` ([Interface](interface.md#the-openai-key)). No other error deletes a key: another 401 may be a missing scope, a 403 a missing permission. A translator or reader session OpenAI refuses to open shows `REFUSED`; the reader does not retry it, nor without a key.

## Account

`Account` in `crates/impshell/src/openai/mod.rs` is the only path to the services: no code outside `crates/impshell/src/openai/` holds a key, an OpenAI address, a connection, a response status or the ledger. Callers (scribe, conversation, news editor, the rundowns' request, pixel cam, browser, bell, Pokédex, translator, reader) pass in their input and get back text, a picture, a session, or a `Fault` (`r1_funcore::openai::fault`): `NoKey`, `Limit` ([Spending limit](#spending-limit)), `Offline`, `Refused` or `Temporary`. An answer that refuses is `Refused`, except one of status 429 or 5xx (`is_temporary`), which is `Temporary` (`Fault::from_refusal`). Each subject's funcore state takes the `Fault` and makes it a `domain::cause::Cause`, its own for a failure this time (`Cause::from_fault`); the interface picks the word on screen (`cause_word` in `ui/words.rs`): `NO OPENAI KEY`, `LIMIT REACHED`, `NO NETWORK`, `REFUSED`, or `NOT WRITTEN`, `NO ANSWER`, `FAILED`, `NO PIXELS`. A `Temporary` is treated as any failure this time. The account is made with the way to the network (`Account::new(data, online)`), and no caller is handed one for its work with a service (the Pokédex keeps its own for a name typed; the browser has none, and holds through the account for the pages it fetches too): work for a service holds the network through the account, which reads the key first. `Account::hold`, `hold_quietly` (a quiet hold, for the reader's stays after its first) and `keep_hold` read it, then hold; the model's background work reads it in `write::ask_work`, and the news' polls in `WorkPoller::new` before the editor holds through `Account::hold`. The `keys` thread holds through the account's way to check a key read off a code, which needs no key kept, and to check the kept key anew; the key's writers and the check are private to this module.

- **Key.** `Key`, private to this module and built from the stored key, sends every request and session with the headers from `headers` and `session_headers` in `r1_funcore::openai::request` (`User-Agent`, bearer, `Accept`, `Content-Type`, `Content-Encoding` when compressed). It reports every `invalid_api_key` refusal to the account, including one at WebSocket opening. Only the key check (`Account::check`) bypasses it.
- **Connections.** Connections opened during one stay remain in the account's pool until the stay ends ([Network](../firmware/network.md#http-and-websocket)). A request takes a connection from the pool or opens a new one. A response read to its end returns its connection to the pool if the server keeps it open (`Answer::is_reusable`). If the server closed a pooled connection before the request was sent, or before the response to a `GET`, the request is sent once more on a new connection. A `POST` the server received is never resent, since it may have been processed and billed (RFC 9110, 9.2.2; `should_retry` in `r1_funcore::codec::http`).
- **Pre-connect.** While a job said records (the conversation, the browser, the bell's request, the rundowns', the Pokédex, a message for Signal), the connection its words go over is opened into the pool as soon as its stay stands (`voiced::take_stay`, `transcript::prepare`, `Account::prepare_connection`), so the transcription waits for no handshake after release. Funcore decides it (`Voice::should_connect_ahead`): for a recording of the job's whose words the speech model is asked for, so nothing is opened for a message said that goes as a voice note (`Job::is_written_down`), nor where no key is kept or the month's cost is at the limit. The conversation opens its turns' WebSocket ahead as well (`Conversation::prepare`). A note's recording opens nothing: it may last as long as a meeting, and its sound is written down in the background once it is kept ([Audio](audio.md#the-scribe)).
- **Compression.** A JSON body posted to `/v1/responses` is compressed with zstd (`Content-Encoding: zstd`, `r1_funcore::codec::zstd`, RFC 8878) when that makes it smaller, unless it would shrink below a hundredth of its size (`MAX_RATIO`, OpenAI's limit). The compressor is r1metal's own: one frame per body, 128 KiB blocks, match finding with zstd level 3 parameters, Huffman-coded literals of at most 11 bits, predefined sequence distributions. The test `zstd_unpacks_frames` decompresses every test frame with the host's `zstd`, and `frame_hashes` holds each frame to the hash it had then.
- **Answers.** An answer over HTTPS is read up to 4 MiB (`ANSWER_BYTES` in `r1_funcore::openai::request`), which the largest answer asked for, a redrawn photo in base64, would fill were its JPEG no smaller than its pixels; each event of an answer that streams is held to the same. One that holds more is read no further and fails this time (`check_length`, `Body::take`).
- **Sessions.** `Session<S>` is a WebSocket with the key (`Session::reach`, over `Socket::reach`), carrying text messages (`Socket::next_message` with `Message::text`), with `S`, what its messages are made with. `Session::begin` sends the setup message and reads until a message of the kind that confirms (`live::SESSION_STARTED`, `aloud::SESSION_UPDATED`) or a refusal comes (`session_status` in `r1_funcore::openai::refusal`). A refused opening is `Refused`, or `Temporary` for 429 or 5xx (`Fault::from_refusal`). `ReadingSession` (`aloud.rs`) requests and cancels passages by response id. `Interpreter` (`live.rs`) streams microphone audio and hands delegated work back to the model. Both hand each service message, or the session's close, straight to the funcore state that reads it: `domain::aloud::reading::Reading::take_message` (audio, the response id, what was said, cost, refusal) and `domain::translator::session::Session::take_message` (audio, transcripts, billed seconds, end, refusal, other message types counted for the log); once close is requested no more audio goes up (`Session::piece`).
- **Ledger.** Only the service files in this directory write the ledger (`Account::add`, which writes the file where what it was handed says it entered anything): an answer where it is read, the language model's work as its answer to the request or to a poll is read, whatever it came to, at what funcore says the answer cost (`summary::status_cost`, `Poll::cost`, a `summary::Cost` that `write::book` books) and as the entry funcore gives its kind of work (`summary::Work::entry`, `summary::RUNDOWN` for a day's rundown); a translator session and a reading aloud at their end, by their threads, as funcore counted them (`live::book_session`, `aloud::book_reading`); a note's count once its transcript is kept (`transcript::book_note`). A failed write is logged.

## The conversation's turns

`Conversation` in `crates/impshell/src/openai/chat.rs` sends the conversation's requests during one stay. Turns use the Responses API's WebSocket mode (`wss://api.openai.com/v1/responses`, `models::chat::ADDRESS`).

- **Socket.** Opened while a message is recorded, or on first use, and kept for the stay. Each request is a `response.create` (`response_create` in `r1_funcore::openai::chat::request`).
- **Cache.** Every request asks OpenAI to keep its prompt in the prompt cache for at least 12 hours (`prompt_cache_options`, mode `implicit`, ttl `12h`: `CACHE_HOURS` in `r1_funcore::openai::models::chat`), since the robot is asked many times a day, often more than the default 30 minutes apart. A write for 12 hours costs twice one for 30 minutes; a read costs the same. No request goes with `generate` false: the service neither reads nor writes the cache for one and bills its whole input at the plain rate.
- **Incremental input.** With `store` false the service keeps the socket's last response in memory only. A request references it (`previous_response_id`) and sends only new items: the message, or a function call's output. The socket tracks how many thread items belong to that response and the last of them (`ThreadMark`, in `SocketThread`). A new socket holds none, so a turn's first request on it carries the full thread, and so does one whose thread no longer starts with those items (for example after compaction), without an id. Instructions and tools go with every request.
- **Failures.** `previous_response_not_found` resends with the full thread (`chat::turn::decide_retry`, which also says what any other refusal fails the turn as; an answer that failed comes with no status and is refused, `Fault::from_refusal`). Any other failure of the socket fails the turn, and the next turn opens a new one: a socket that cannot be opened, one that fails before the request is sent (closed while idle, detected by `TurnSocket::is_open`; broken during the send; `websocket_connection_limit_reached` after 60 minutes), and one that closes after the request is sent, which is not resent: the service may have processed and billed it.

The log records for each response how many items were sent and its token counts.

## Models

All models are set in `r1_funcore::openai::models`; every caller reads them from there.

| Use | Endpoint | Model and settings |
|---|---|---|
| Pixel cam | `POST /v1/images/edits`, multipart form | `gpt-image-2.5-flare`, quality `medium`, 1024x1024, JPEG at 90 |
| Transcription | `POST /v1/audio/transcriptions`, multipart form: `model`, `stream` for the conversation, `prompt` when there are names to spell, the file as `note.m4a` (`audio/mp4`), at most 25 MB (`transcript::MAX_BYTES`) | `gpt-transcribe`, no effort or verbosity |
| Summaries, rundowns, recollections, the conversation | `POST /v1/responses` with `instructions` and `input`; conversation turns over `wss://api.openai.com/v1/responses` | each use's language model and effort ([Language models](#language-models)), `gpt-6.1-terra` at `high` on a new device; verbosity `low` |
| Signal's auto mode | `POST /v1/responses` in the background, as summaries are, without reasoning modules | its language model and effort, `gpt-6.1-terra` at `high` on a new device; verbosity `low` ([Signal](signal.md#auto-mode)) |
| The browser's steps | `POST /v1/responses` in the background, as summaries are, with the web search among its `tools` (`summary::search_request`) | its language model and effort, `gpt-6.1-terra` at `high` on a new device; verbosity `low` ([Interface](interface.md#web)) |
| The reading of a request to the bell | `POST /v1/responses` in the background, as summaries are (`summary::request` under `bell::task`), with no tool: the two functions' declarations stand in its instructions, and it answers a line a call | its language model and effort, `gpt-6.1-terra` at `high` on a new device; verbosity `low` ([Interface](interface.md#the-bell)) |
| The reading of a request for the rundowns | `POST /v1/responses` in the background, as a step of the browser's is, with the web search among its `tools` (`summary::search_request` under `news::request::task`), and it answers in the lines that change the rundowns | its language model and effort, `gpt-6.1-terra` at `high` on a new device; verbosity `low` ([News](news.md#requests)) |
| Translator | `wss://api.openai.com/v1/live/sessions` | `gpt-live-1`, raw 16 bit samples at 24 kHz both ways |
| Reading aloud | `wss://api.openai.com/v1/realtime?model=gpt-realtime-2.1-mini` | `gpt-realtime-2.1-mini`, effort `minimal`: it reads word for word, and audio starts soonest this way |

- **Photos.** A photo the owner shows the robot goes as an `input_image` of the message, a JPEG data URL with `detail` `high` (`models::chat::DETAIL`, [Chat](chat.md#photos)).
- **Voice.** `VOICE` is `marin` for every model that speaks.
- **Reasoning modules.** `with_modules` in `r1_funcore::openai::request` appends `MODULES` (`crates/funcore/assets/openai/modules.txt`, about 5 KB) to a system prompt: the 39 reasoning modules of SELF-DISCOVER (arXiv 2402.03620), introduced as a private aid for hard tasks that never appears in a reply. Summaries, rundowns, recollections, the browser's steps, the reading of a request to the bell and of one for the rundowns, the conversation and the translator get them. The reader and the conversation's voice do not. The speech model has no system prompt, only a `prompt` with names to spell (the user's and the robot's; the Pokédex's `pokedex::prompt`, which names the language set) or with what the recording is (the browser's `web::state::PROMPT`: a request to a web browser; the bell's `bell::state::PROMPT`: a request to set or cancel an alarm, a reminder or a timer; the rundowns' `news::request::PROMPT`: a request about the rundowns of a daily news edition). The pixel cam's prompt is not a system prompt.
- **Background requests.** Summaries, rundowns, recollections, the browser's steps and the reading of a request to the bell or for the rundowns use `background` true and `store` false. A step of the browser's and the reading of a request for the rundowns also carry the web search as a turn of the conversation does (`search_context_size` `high`, `return_token_budget` `unlimited`, `external_web_access`, and an approximate `user_location` where the place service has one; `chat::request::build_tools`), and no function. The response returns an id, polled with `GET /v1/responses/ID` while `queued` or `in_progress`, until `completed`; the work continues without the client and is billed by the response's usage, work that carries the web search by the web searches among its items too, whether it finished or came to nothing ([Prices](#prices)). With `store` false OpenAI's guide has it deleted after roughly 10 minutes and does not say from when; the firmware waits for it `PATIENCE_SECONDS` (300 s) from the request, the owner's choice. 429, 5xx (`is_temporary`) and a broken connection mean poll again; any other error, 404 included, is final. Work unfinished when a stay ends, or when the network cannot be had again for a poll, is recorded as owed (`Jobs` in `r1_funcore::openai::summary`, in memory, keyed by the request's SHA-256); the same request made again polls that id instead of creating a new response, so it is billed once. The news editor stores rundown ids on the data partition instead ([News](news.md#fetched-later)).
- **Timeouts.** How long a connection is given is in `r1_funcore::openai::models`; how long a session or background work waits on the service's word sits beside its state. HTTP requests and the conversation's WebSocket: 120 s per read (`TIMEOUT`), from the request and from the socket's opening on: a connection is made and a WebSocket opened under the connection's own limit ([Network](../firmware/network.md#connections-and-tls)). Translator: 15 s per read of the session's start (`interpreter::TIMEOUT`), then 3 s for the service to take each send and to answer the session's close (`r1_funcore::domain::translator::session::CLOSING_TIMEOUT`, `Session::wait_limit`), while the wait on a session that stands has no limit, the microphone's blocks waking the thread; a close the service does not answer in that time, or that cannot be sent, is logged and ends the session without a failure, at the seconds it lasted (`session::UNCLOSED`, `to_unanswered_line`). Reader: 15 s per read (`reader::TIMEOUT`); a passage the service says nothing of for 10 s ends the reading (`STALL_TIMEOUT` in `r1_funcore::domain::aloud::reading`), and a reading left waits up to 5 s for the cost of the passage on its way (`SETTLE_TIMEOUT`). Background polling: every 2 s over the network held since the request for its first 20 s (`HOLD_SECONDS` in `r1_funcore::openai::summary`), then every 30 s (`SLOW_POLL_SECONDS`), the network let go of between polls and held again for each, up to 300 s (`Watch`); work owed from an earlier stay is polled at once.
- **Logging.** A refusal logs the HTTP status if any, the error code and the message. For a 401 the message is left out, since it may show part of the key (`Refusal::to_log_line`). Within an open session a refusal logs code and message. The key is never logged (test `key_words_stay_out_of_log`).

## Language models

Each use of the language model has a model and an effort of its own (`Task`, `Terms::thinking` in `r1_funcore::openai::account`), set under `FEATURES` in the settings ([Interface](interface.md#settings)) and kept as `model USE MODEL EFFORT` ([Storage](../firmware/storage.md#layout)). The main loop hands them to the account as they change (`Account::set_terms`); each request takes its use's as it is made.

| Use (`Task`) | Word | Set by | New device |
|---|---|---|---|
| the conversation (`Chat`) | `robot` | `CHAT MODEL`, `CHAT REASONING` | `gpt-6.1-terra`, `high` |
| recollections (`Recollect`) | `recollection` | `RECOLLECTION MODEL`, `RECOLLECTION REASONING` | `gpt-6.1-terra`, `high` |
| the summaries of notes and translations (`Summary`) | `notes` | `SUMMARIZE MODEL`, `SUMMARIZE REASONING` | `gpt-6.1-terra`, `high` |
| the day's rundowns (`News`) | `news` | `MODEL`, `REASONING` under the news' settings | `gpt-6.1-terra`, `high` |
| auto mode's replies (`Auto`) | `auto` | `AUTO MODEL`, `AUTO REASONING` | `gpt-6.1-terra`, `high` |
| the browser's steps, a request resolved or a page written (`Web`) | `web` | `MODEL`, `REASONING` under the web's settings | `gpt-6.1-terra`, `high` |
| the reading of a request to the bell (`Bell`) | `bell` | `MODEL`, `REASONING` under the bell's settings | `gpt-6.1-terra`, `high` |
| the reading of a request for the rundowns, with the search for a new one's feeds (`Rundowns`) | `rundowns` | `REQUEST MODEL`, `REQUEST REASONING` under the news' settings | `gpt-6.1-terra`, `high` |

The models, the cheapest first (`Model`): `gpt-6-luna`, `gpt-6.1-terra`, `gpt-6.1-sol`, `gpt-6-astra`. The efforts, the least first (`Effort`): `none`, `low`, `medium`, `high`, `xhigh`, `max`; `gpt-6.1-sol` and `gpt-6-astra` take `low` at the least, and an effort below a model's least is that least (`Thinking::new`). A summary's or a rundown's Markdown names the model its answer names ([Audio](audio.md#documents)).

## Prices

Prices are constants next to each request, taken from OpenAI's price pages; when a price changes, its constant must change too.

| Service | Price |
|---|---|
| `gpt-image-2.5-flare` | $5 per million text tokens in, $8 image in, $30 image out (`TEXT_IN`, `IMAGE_IN`, `IMAGE_OUT` in `r1_funcore::openai::image`); no cached rate, which applies only to images made through the Responses API |
| `gpt-transcribe` | $0.0045 per minute, by the service's seconds |
| `gpt-6-luna` | per million tokens: $0.10 in, $0.01 cached, $0.125 cache write for 30 minutes and $0.25 for 12 hours, $0.50 out including reasoning; above 272,000 tokens in (`LONG_CONTEXT_TOKENS`) $0.20, $0.02, $0.25, $0.50 and $0.75 for the whole response |
| `gpt-6.1-terra` | per million tokens: $1.00, $0.05 cached, $1.25 and $2.50 cache writes, $5.00 out; above 272,000 tokens in $2.00, $0.10, $2.50, $5.00 and $7.50 |
| `gpt-6.1-sol` | per million tokens: $2.00, $0.10 cached, $2.50 and $5.00 cache writes, $10.00 out; above 272,000 tokens in $4.00, $0.20, $5.00, $10.00 and $15.00 |
| `gpt-6-astra` | per million tokens: $10.00, $1.00 cached, $12.50 and $25.00 cache writes, $50.00 out; above 272,000 tokens in $20.00, $2.00, $25.00, $50.00 and $75.00 |
| Web search | $10 per thousand searches (`output::SEARCH_MICROS`), a search being a `web_search_call` item of an answer whose action is `search`, in a turn of the conversation, in a step of the browser's and in the reading of a request for the rundowns alike (`output::search_count`); a page the model opened is none, and pages read count as response tokens |
| `gpt-live-1` | $0.05 per minute |
| `gpt-realtime-2.1-mini` | per million tokens: $0.60 text in, $0.06 cached, $2.40 text out, $20 audio out |

Costs use the service's own counts: `input_tokens` (of which `cached_tokens` and `cache_write_tokens`, of those `ttl_12h_tokens` for 12 hours) and `output_tokens`, each at its rate, or seconds for services billed by time. Tokens are priced at the rates of the model the answer names, a snapshot's date after its name included, and at the cheapest model's for a name not known (`Model::named`, `r1_funcore::openai::tokens`). A response that failed or ended without text is booked as far as the service counted it, the language model's background work with what its web searches cost beside its tokens (`summary::Cost::Failed`); one with no counts and no search, or that never completed, is not (`Usage::is_counted`, `Ledger::add_failed_work`). A completed response is booked even if its connection fails afterwards (`finished_stream` in `r1_funcore::openai::request`).

## Spending limit

`LIMIT`, a line of the cost screen ([Interface](interface.md#cost)), is the most OpenAI's services may cost a month: $1, $2, $5, $10, $20, $25, $50 or $100, or none; $25 on a new device (`LIMITS`, kept as `limit N` or `limit none`). Where one is set, work paid for anew reads the ledger first and is not asked while the month's total by the clock on screen, the kinds an unlink forgot included, is at the limit (`Terms::check_limit`, `Ledger::is_within`; `Ledger::spent` is that total, which the cost screen shows as `THIS MONTH`): a transcription, a picture, the opening of a translator's or a reader's session, a rundown or other new background work (`Account::paying`, `check_limit` in `write::ask_work`), and each request of a conversation's turn (`TurnSocket::turn`). It comes to `Fault::Limit`, which, as a missing key, stops such work until its screen opens again (`Fault::is_stalling`) and is not asked again this time (`Fault::is_permanent`); the screens say `LIMIT REACHED`. Polls of background work asked before, and the key's checks, go on. A ledger that cannot be read holds the work back as a failure this time. A cost is known once its answer is read, so work under way as the limit is reached may take the month past it.

## Ledger

`.r1metal/openai/ledger` (`Ledger` in `r1_funcore::openai::ledger`) records every cost: a text file, one line per month, newest first.

```
2026-10 15 6390 15360 6585 636180 3 240 352380 18000 3 2100 2 70000 1 50000 2 30000 12 40000 5 50000 4 900 1 800 1 6000 1 5000 2 400 3 3000 2 4000 6 1500 1 900 4 1200
```

| Fields | Content |
|---|---|
| 1 | month, local time |
| 2 to 5 | pictures; their text-in, image-in and image-out tokens |
| 6 | the month's total in millionths of a dollar, at the prices in force when each item was paid |
| 7, 8 | notes with a complete, stored transcript; seconds of their audio transcribed, failed notes included |
| 9, 10 | cost of pictures, cost of the notes' transcription |
| 11, 12 | summaries of notes, cost |
| 13, 14 | rundowns, their cost with that of the requests for the rundowns, read by the language model with its web searches and said |
| 15, 16 | translator sessions, cost |
| 17, 18 | readings aloud of the news, cost |
| 19, 20 | conversation turns, the conversation's cost |
| 21, 22 | the conversation's web searches, cost |
| 23, 24 | names said to the Pokédex, cost |
| 25, 26 | summaries of translated conversations, cost |
| 27, 28 | readings aloud of a note's summary, cost |
| 29, 30 | readings aloud of a translation's summary, cost |
| 31, 32 | messages said for Signal, cost |
| 33, 34 | replies of Signal's auto mode, cost |
| 35, 36 | readings aloud of what came on Signal, cost |
| 37, 38 | the language model's steps for the browser, their cost with that of their web searches and of the requests said |
| 39, 40 | readings aloud of the browser's page, cost |
| 41, 42 | requests to the bell the language model read, their cost with that of the requests said |

Each field pair is a kind of `Kind` in `Kind`'s order; the cost screen groups them by app ([Interface](interface.md#cost)). A cost is entered by the one `Ledger::add` as a `ledger::Entry` tells: one of a kind (`Entry::one`), or work beside what the kind counts, which adds its cost and counts none (`Entry::work`). When the phone unlinks the R1, Signal's kinds (fields 31 to 36, `ledger::SIGNAL`) are set to nought in every month and the ledger is kept; each month keeps its total, which then holds money no kind tells of (`Ledger::forget`, [Signal](signal.md#unlink)).

| Event | Adds |
|---|---|
| A picture's response is parsed, before the picture is saved; also a response without a picture that has token counts (`image::book_failed`, `Ledger::add_failed_picture`) | one picture and its cost |
| A summary, a note's or a translation's (`Entry::summary`); a rundown of a day's edition (`Kind::Rundown`), so an edition adds one for each rundown made; a reply of Signal's auto mode (`write::write_auto_reply`, `Kind::Auto`); a step of the language model's for the browser, a request it resolved or a page it wrote (`write::write_page`, `Kind::Web`); a request to the bell the language model read (`write::write_bell_calls`, `Kind::Bell`); each also where it ended without text and has token counts or made a web search, as the answer that failed at once or the poll that found it failed is read (`Cost::book`, `Ledger::add_failed_work`); a translator session with a nonzero cost (`Ledger::add_session`) | one, and its cost; a step of the browser's costs its tokens and a cent for each web search among its answer's items (`summary::Cost`), one that came to nothing too |
| A reading aloud with at least one billed passage, however many passages (`Reading::finish`), of its kind (`ReadingSource::ledger_kind`): the news, a note's summary, a translation's summary, what came on Signal, or the browser's page | one, and its cost |
| A note's audio sent for transcription (a note over 20 minutes goes in pieces; `Subject::Note`, `Ledger::add_sound`) | seconds and cost to notes, no count |
| A note's complete transcript is stored | one note, no cost |
| A turn's first response | one turn, and its cost |
| A turn's later responses, its transcription (`Subject::Conversation`), its voice, a recollection (`Entry::work(Kind::Chat)`, `Ledger::add_reading`) | cost to the conversation, no turn |
| A web search of a conversation's answer, a failed one's too (`Answer::add_to_ledger`, `Kind::Search`) | one, and its cost |
| A name said to the Pokédex is transcribed (`Subject::PokedexName`, `Kind::Pokedex`) | one, and its cost |
| A message said for Signal is transcribed (`Subject::Message`, `Kind::Dictation`) | one, and its cost |
| A request said to the browser is transcribed (`Subject::WebRequest`, `Entry::work(Kind::Web)`) | cost to the browser's steps, no count |
| A request said to the bell is transcribed (`Subject::BellRequest`, `Entry::work(Kind::Bell)`) | cost to the bell's requests, no count |
| A request for the rundowns is read by the language model (`write::write_rundown_lines`), at its tokens and a cent for each web search among its answer's items, one that came to nothing too; a request said for them is transcribed (`Subject::RundownsRequest`); both as `Entry::work(Kind::Rundown)` | cost to the rundowns, no count |

Parsing: a line is read from its month and its first 5 numbers (fields 2 to 6, `REQUIRED_FIELDS`); a later field that is not a number, or is missing, counts as 0, so a line that ends at field 40 reads with the bell's fields as 0, and words after field 42 are ignored. A line without a month or those 5 numbers is skipped, and so is a line of 6, 8, 17 or 22 words, month included, the lengths of the ledger's older forms (`OLD_FORM_WORDS`). Two lines for the same month are summed. `money::dollars` formats every amount for screens and log: rounded to the cent, `$0.00` for zero, `<$0.01` for a nonzero amount under half a cent. The cost screen shows the ledger ([Interface](interface.md#cost)).
