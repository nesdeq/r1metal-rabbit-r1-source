# Storage

The data partition, the safe that encrypts what it keeps, its layout and files, the settings, backup and restore, and the read-only USB drive that shows it to a computer while the owner has it switched on.

## The data partition

`userdata`, the last and largest eMMC partition (107.4 GiB), holds an f2fs filesystem mounted at `/data` (`r1_funcore::storage::PARTITION`, `MOUNT_POINT`). No image is flashed to it.

- The `storage` thread finds the partition by name on the boot device, never on a card, and reads both superblocks. An f2fs labelled `r1metal` is mounted as it is (`r1_funcore::storage::progress::identify`). Anything else is discarded, formatted and mounted. An r1metal filesystem that fails to mount is left alone, and the debug screen's `DATA` line shows the error.
- The formatter is `r1_funcore::fs::f2fs`. It takes partition size, first sector, label, UUID, time and kernel version, and returns the blocks of an empty filesystem, byte for byte what `mkfs.f2fs` of f2fs-tools 1.17.0 writes with a label and no features (no compression, encryption or quotas). It refuses the sizes that tool refuses. Zero blocks are written one 2 MiB segment at a time (`f2fs::zero_writes`). The superblocks are written last, so an interrupted format formats again at the next boot.
- Before it mounts the filesystem, the thread gives both superblocks the `encrypt` feature where they lack it, as `fsck.f2fs -O encrypt` does, and changes nothing else (`f2fs::allow_encryption`; a superblock with a checksum is left alone). New filesystems get it the same way.
- Nothing is kept on the partition before the safe is open, and no directory is made before: at every start, once it is open and behind a restore's files ([Backup and restore](#backup-and-restore)), each public root is made or brought under the safe's key ([The safe](#the-safe)), then the missing directories are made, the public ones (`storage::DIRECTORIES`) and the private ones (`partition::PRIVATE_DIRECTORIES`).
- A reboot or power off runs `sync` but does not unmount; f2fs mounts from its last checkpoint.
- A recording (a note, a conversation message, a video) starts only with 64 MiB free, and a running video stops below that (`RESERVE_BYTES`, `storage::has_room`, `storage::is_full`).

`storage::Data` (`crates/impshell/src/storage/data.rs`):

- `names` lists what a directory holds of one kind, sorted (`paths::Listed`): its files written whole, every file, those half written included, for Signal's unlink, which takes them all away, the files of one extension, or its folders; a directory that is not there holds none. `trim` hands every free block of the partition back to the medium (`FITRIM`), as the safe's making does.
- `is_ready`, `is_settled`, `wait_settled` and `wait_decided` check or wait for the bring-up state, which the debug screen's `DATA` line also shows (`r1_funcore::storage::progress::Progress`). The safe is part of the bring-up: until it is open, and what was kept plain is under its key (`Progress::Encrypting`), the partition counts as not mounted, and no file of it is reached (`NOT_MOUNTED`). While the thread formats or moves files under the key the system does not suspend (`Progress::is_at_work`, `Storage::is_off`). The threads that read what an earlier run kept wait for it: the settings, the logs, Signal, the modem, the news, the router for the tunnel's config, the conversation as it is first used, the notes and the gallery as they count what home shows, and the USB drive; the battery gauge reads its memory at the first sample that finds it mounted, waiting for nothing ([Battery](battery.md#files)).
- Every file is named by a directory, a path from the partition's root, and its name. Every way to a file asks of both first that they name nothing outside where they name: a directory of plain names one under another, and a plain name (`paths::to_directory_path`, `paths::to_file_path`), so that a name built from what the model, the console or a message said cannot lead out of its directory; such a name is refused as invalid. `put` writes the new content to the file's name plus `.new` (`paths::new_name`; a listing of the files written whole leaves such a file out, `paths::is_half_written`), syncs it, renames it over the old file and syncs the directory (`write_over` in `storage/files.rs`), so a power cut leaves either the old or the new content; `put` makes a missing directory, while `put_into` writes the same way into an entry's folder only while the folder is there and never makes it (false where it is gone). `read`, `text`, `names` and `remove` read, list and remove such files. Every file in `.r1metal` is written this way except the logs, the uses, which grow one line at a time and are written whole only as they lose Signal's lines, and the conversation's thread, which grows one item at a time and is rewritten only when the service compacts it.
- Each part is handed the partition narrowed to the directories it uses (`Storage::within`, bound in `app/start.rs`), and nothing that holds a `Data` makes a wider one: a directory that is none of them and lies under none of them is refused as invalid before the partition is looked at (`paths::check_scope`). What names no directory (`is_ready`, `is_settled`, `wait_settled`, `wait_decided`, `id`, `free`, `room`, `trim`) answers every part; the about screen is handed no directory. The USB drive and the debug shell alone are handed the partition whole (`Storage::data`), and only a part so handed is told where it is mounted (`export_root`, `paths::check_full_scope`), has a passphrase checked against the safe (`check_passphrase`) and stages and confirms a restore (`stage`, `commit`).
- `append` appends to a file and syncs it. `discard` removes an entry's folder and everything in it; the folder must be a single plain name, asked first as a file's name is (`paths::to_file_path`).
- `room` tells the partition's size and its free bytes, for the about screen; `export_root` is where the mounted partition lies, for the USB drive. Both answer only while the partition is mounted.
- `storage::stem_now` names a new file by local time, `yyyymmdd_hhmmss`, adding `_1`, `_2` where the name is taken (`file_names::first_free`, which also names saved files and log archives).

## The safe

Everything the partition keeps of the owner is encrypted by the filesystem under one key, the safe's (fscrypt, policy version 2: contents in AES-256-XTS, names in AES-256-CTS padded to 32 bytes; `r1_funcore::fs::fscrypt`, `storage/safe.rs`): the private directory `.r1metal`, which holds the keys, the passwords, Signal's account and conversations, the settings and the logs, and the public roots `camera`, `audio`, `news` and `chat` (`backup::PUBLIC_ROOTS`), which hold the photos, videos, notes, translations, editions and the robot's memory and conversation. Until the key is added nothing under it can be read, written or listed by its name; once added, the kernel keeps it until the next restart, through every sleep.

**The key** is 64 random bytes (`crypto::new_safe_key`), the same through every change of passphrase. It is kept only sealed under the passphrase, in two key files at the partition's root, `.r1metal.key` and `.r1metal.key2`, two copies of one text (`safe::KEY_FILES`, `safe::Slot`):

```
r1metal safe 1
work 16 8 1
salt 32 HEX DIGITS
nonce 24 HEX DIGITS
key 160 HEX DIGITS
```

scrypt stretches the passphrase with the file's random salt of 16 bytes to a key of 32 bytes (N = 2^16, r = 8, p = 1, 64 MiB; `r1_funcore::storage::safe::WORK`, AWS-LC's `EVP_PBE_scrypt`), and AES-256-GCM under that key and a random nonce of 12 bytes seals the safe's key, 64 bytes and the seal's tag of 16, the three lines before the nonce bound into the seal (`crypto::seal_safe_key`, `crypto::open_safe_key`): a file whose mark, work, salt, nonce or key was changed opens nothing, and a text another key file would not be written as, one that names other work among them, is none (`Slot::parse`). Either file opens the safe; a passphrase is tried once on each seal the files hold (`safe::distinct`). Each is replaced at one stroke, written beside itself, synced and renamed over the old one (`write_over`), and where one is missing or holds another seal than the one that opened, both are written anew from that one (`safe::needs_rewrite`). USB hosts are shown neither (`paths::is_shown_to_hosts`), no backup takes them (`backup::is_backed_up`), and the debug shell reaches them only with its gate open ([Console](console.md#the-gate)).

A passphrase and a key are held only as their own types, which overwrite themselves and all the room they hold as they are let go, however that is (`safe::Passphrase`, `fscrypt::Key`): the window's, the orders the window hands the storage thread, the key a key file gives and the one scrypt draws; the 32 bytes scrypt stretches a passphrase to are zeroed once the library has them (`crypto::seal_key`). While it is typed, a passphrase is the text of the window's keys, which has room for the most it may hold from its beginning, so that typing never moves it and leaves no copy behind (`Keyboard::begin`), and which the window takes into a `Passphrase` whenever it ends it or lets the keys go. A passphrase the console hands over for the shell's gate comes as a line of the console and is a `Passphrase` from its parse on ([Console](console.md#the-gate)).

At every start, once the partition is mounted, the storage thread:

1. Takes away a `.r1metal.new` that a start cut short left, and asks for the free blocks to be handed back to the medium (`safe: what a start cut short left is gone`).
2. Reads the key files and tells whether a passphrase is set (`safe::to_source`, `Source::is_made`, `Progress::Shut { is_made }`, `safe: shut` or `safe: not made`): one is where a key file reads or `.r1metal` is encrypted under a policy of version 2. A key file that holds no sealed key is told in the log and passed over (`safe: .r1metal.key2 holds no sealed key`).
3. Waits for the passphrase the safe's window hands over (`Storage::ask_safe`; [Interface](../app/interface.md#the-passphrase)).
4. **The key** (`take_key`), by what it found (`safe::Source`):

   | Found | The key |
   |---|---|
   | a key file reads | the one the passphrase opens in the first file it opens; where it opens none, the passphrase is wrong (`Refusal::Wrong`, `safe: a wrong passphrase`) |
   | no key file reads, `.r1metal` is a safe an earlier firmware made | that firmware's key, 64 bytes scrypt draws straight from the passphrase and a salt of `r1metal safe ` and the filesystem's UUID (`safe::salt`, `crypto::safe_key`), is added and its identifier compared with the one in `.r1metal`'s policy: another, and the passphrase is wrong; the same, and a new key is drawn and sealed under the passphrase into both key files (`safe: a new key, sealed under the passphrase in the key files`) |
   | no key file reads, no safe is made | a new key, sealed under the passphrase chosen into both key files, before anything is encrypted under it |

5. **The private directory** (`settle_private`): the key is added to the filesystem (`FS_IOC_ADD_ENCRYPTION_KEY`) and the identifier the kernel gives it compared with the one in `.r1metal`'s policy. The same, and the safe is open. Where `.r1metal` is under the key an earlier firmware drew from the passphrase, as the start that sealed the new key leaves it and as a start cut short between the key files written and the directory made anew does, the directory is made anew under the new key and the earlier key taken off the filesystem (`FS_IOC_REMOVE_ENCRYPTION_KEY`, `safe: made anew under the key of the key files, the key drawn from the passphrase taken away`); under any other key the opening fails (`the private directory is under another key than the key files hold`). Where there is none or a plain one, it is made.
6. **Making** (`make_with`): creates `.r1metal.new`, encrypts it under the key (`FS_IOC_SET_ENCRYPTION_POLICY`) and copies every directory and file of the `.r1metal` there is into it, each synced. It then drops the kernel's caches and reads the copy back from the medium, decrypted, comparing it byte for byte with the original (`safe::is_same_tree`); a difference fails the making and changes nothing (`the encrypted copy reads back otherwise than what it copies`). The copy takes `.r1metal`'s place at one stroke (`renameat2` with `RENAME_EXCHANGE`), the directory it copied is removed and the filesystem is asked to hand its free blocks back to the medium (`FITRIM`); the log tells how many files went in and how many bytes it handed back (`safe: made, N files encrypted into it`). A making cut short leaves the `.r1metal` it copied, and the next start makes it anew.
7. Tells the safe open with the time the key took (`safe: open, the key drawn in N ms`). A wrong passphrase is told `Wrong`, any other failure `Failed`, and the window asks again; the key files and the directory are looked at anew for every passphrase.
8. **The public roots** (`safe::enclose`): each of `camera`, `audio`, `news` and `chat` that is missing is made under the safe's key. One that is there plain, as an earlier firmware kept it or a restore left it, is moved under the key file by file while the bring-up tells `Progress::Encrypting`: `ROOT.new` is made under the key, or taken as a start cut short left it, and each file is copied into it, synced with its directory, its pages let go, read back from the medium decrypted and compared with the original a segment of 2 MiB at a time (`safe::is_same_file`, `safe::COMPARED_BYTES`), as the files of a private directory made anew are, and only then removed where it came from (`move_tree`, `move_file`); what is neither a directory nor a file is told in the log and removed. Then the emptied plain root goes, the new one takes its name, and the log tells `safe: camera under the key, N files encrypted into it`; after a move the filesystem is asked to hand its free blocks back to the medium. A root that does not go whole stays as far as it went, the log tells why (`safe: audio is not under the key whole, the next start goes on: REASON`), the start goes on with the other roots, and the next start moves the rest, a copy cut short made anew.
9. Makes the missing directories and tells `Mounted`, then serves changes of the passphrase for as long as the app runs (`safe::serve_changes`).

Before step 1 the thread settles what a restore left, and between steps 7 and 8 it puts a confirmed restore's files in place ([Backup and restore](#backup-and-restore)).

**A new passphrase** seals the same key anew (`safe::change`; [Interface](../app/interface.md#the-passphrase)): the storage thread opens a key file with the passphrase in use, seals the key it gives under the new passphrase with a new salt and nonce, and writes both key files. Nothing the safe holds is copied or touched, the app runs on, and no cable is needed. A passphrase in use that opens no key file is told `Wrong` and changes nothing (`safe: a wrong passphrase in use`); where no key file reads (`safe::check_slots`, `safe: no key file reads`) or the files cannot be written the change is told `Failed`; a change made logs `safe: the passphrase in use is right, the key is sealed under the new one`, and the filesystem is asked to hand its free blocks back to the medium, as after a making (`safe::serve_changes`). A change cut short between the two files leaves one under each passphrase: either opens the safe, and the opening writes both alike from the file that opened.

Three wrong passphrases in one start power the device off; the count begins anew at the next start. Nothing recovers a forgotten passphrase, nor a key whose two files are both gone: `r1 flash --wipe` erases `userdata`, the next start formats it, and everything on the partition is lost.

Outside the key: none of the owner's files. The private directory, the four public roots and what a restore stages of them lie under it. What fscrypt leaves readable without the key stays so: how many files and directories there are, their sizes, their times and the shape of the tree, their names as ciphertext. The two key files are plain text: they hold the key sealed, and whoever reads them can try passphrases against them, each at scrypt's work. A computer on the cable is shown the files decrypted, through the open safe, only while the owner has `DRIVE` under `USB DRIVE` switched on ([USB drive](#usb-drive)). A replaced or removed file's blocks are freed, not overwritten: f2fs mounted without `discard` hands none back when the safe is made (`0 bytes` in the log), so the blocks of what was plain, and of a key file sealed under a passphrase since changed, may hold their content until they are written over. Signal's unlink asks for the free blocks to be handed back too, after it has taken the account's files away ([Signal](../app/signal.md#unlink)). A key given at flash time is stored in plain text in the `flashed` file of the boot slot that flash wrote, until that slot is written again; `r1` builds that image in memory and writes no file of it. The bootloader is unlocked: whoever holds the device can flash a firmware that records the passphrase as it is typed next.

## Layout

| Path | Content |
|---|---|
| `camera/pictures/STAMP_original.jpg`, `STAMP_pixel.jpg` | a photo, 2448x2448, and its pixel art, 1024x1024 ([Camera](../app/camera.md)); Signal reads the one it sends ([Signal](../app/signal.md#attachments)) |
| `camera/videos/STAMP.mp4` | a video; Signal reads the one it sends |
| `audio/notes/STAMP/` | `STAMP.m4a`, `STAMP_transcript.md`, `STAMP_summary.md`, `STAMP_summary.pdf` ([Audio](../app/audio.md)); Signal reads the sound of a note it sends |
| `audio/translations/STAMP/` | `STAMP_conversation.md`, `STAMP_summary.md`, `STAMP_summary.pdf` |
| `news/yyyymmdd/` | `yyyymmdd_NAME.md` and `.pdf` for each rundown made that day, `NAME` the rundown's name (`yyyymmdd_news.md`, `yyyymmdd_finance.md`; [News](../app/news.md#the-days-rundowns)) |
| `chat/core.md`, `chat/knowledge.md` | the robot's memory ([Chat](../app/chat.md)) |
| `chat/talk/yyyymmdd.md`, `chat/days/yyyymmdd.md` | a day's conversation; that day's recollection |
| `.r1metal.key`, `.r1metal.key2` | the safe's key sealed under the passphrase, two copies of one text ([The safe](#the-safe)); hidden from USB hosts, taken by no backup |
| `.r1metal/` | keys and settings, the private directory; hidden from USB hosts |
| `.r1metal/interface/settings` | the settings, below |
| `.r1metal/interface/flash` | the stamp of the flash whose choices (logging, key) were applied ([Logs](logs.md#the-logging-switch)) |
| `.r1metal/interface/bell` | every alarm, reminder and timer that is set, a line each: `entry clock N TEXT` for one that rings when the clock on screen shows second `N` of its own time, `entry span N TEXT` for a timer that runs out at second `N` of UTC, the text of what it is for behind it where it has one; a line that does not read is passed over (`r1_funcore::domain::bell`, [Interface](../app/interface.md#the-bell)) |
| `.r1metal/interface/rundowns` | the rundowns the owner keeps, in their order: `rundown NAME on\|off` for each, whether an edition makes it; `about NAME WORDS`, what it is about; `icon NAME WORD`, the icon it goes by where it keeps one; `feed NAME LINE` for each feed of its own, a feeds list's line; a line that does not read, or that names no rundown of the file, is passed over (`r1_funcore::domain::news::topics`, [News](../app/news.md#the-rundowns-kept)) |
| `.r1metal/battery/gauge`, `usage` | what the gauge has learned about the cell; the usage history ([Battery](battery.md)) |
| `.r1metal/wifi/networks` | per known network a `name NAME` and a `password PASSWORD` line, most recently chosen first; from a damaged file, every complete entry is read (`r1_funcore::wifi::known`) |
| `.r1metal/wifi/secret` | 32 random bytes, created once, from which each network's MAC address is derived ([Network](network.md)) |
| `.r1metal/wifi/place` | per known network: its name, whether it has a password, the frequency of its last join and its frequency on the other band; no password |
| `.r1metal/clock/place`, `where` | the place service's last UTC offset; the location it returned (country, region, city and time zone, one per line), never logged ([Sleep and clock](sleep.md#clock)) |
| `.r1metal/openai/key` | the OpenAI key on one line; replaces any earlier key once OpenAI accepts it, and is removed once OpenAI rejects it ([OpenAI](../app/openai.md#the-key)) |
| `.r1metal/openai/ledger` | the record of what the services cost ([OpenAI](../app/openai.md#ledger)) |
| `.r1metal/chat/state`, `thread` | the conversation's state and its thread |
| `.r1metal/magic/STAMP.jpg` | a photo sent to the pixel cam whose pixel art has not arrived yet |
| `.r1metal/news/tried`, `pending`, `silent` | the day last attempted, a line `yyyymmdd`, and a line `yyyymmdd NAME` for each rundown asked for anew that day; the rundowns in progress; the feeds that gave no story when their rundown's were last read, a line `feed NAME LINE` each ([News](../app/news.md#the-rundowns-kept)) |
| `.r1metal/logs/` | the logs ([Logs](logs.md)) |
| `.r1metal/signal/` | the account the R1 is linked to, its keys, identities, contacts, send log (`sent`), blocked list (`blocked`), sender certificates (`certificates`), call log (`calls`), sessions, sender keys (`sender-keys/`), conversations and the plaintexts of attachments (`attachments/`); reached by the debug shell only once the safe's passphrase is given ([Console](console.md#the-gate)), and taken away whole when the phone unlinks the R1 ([Signal](../app/signal.md#files)) |
| `.r1metal/cell/sim` | the SIM's PIN as `pin N`, once the SIM took it while `keep_pin` is on; empty otherwise (`r1_funcore::cell::sim`) |
| `.r1metal/cell/attaches` | the UTC second of each attach of the last 24 h, a line each (`r1_funcore::cell::attaches`) |
| `.r1metal/cell/nvram/`, `factory/`, `seeded` | the modem's NVRAM, one folder per drive; the factory's copy `Restore` reads; the drives filled whole, a letter a line (`r1_funcore::cell::drives`, [Modem](../hardware/modem.md#in-r1metal)) |
| `.r1metal/router/tunnel` | the config of the tunnel a computer on the cable goes through, a line each: `private KEY`, `address A`, `dns A`, `mtu N` where the config names one, `peer KEY`, `preshared KEY` where it has one, `endpoint HOST:PORT`, `keepalive N` where it asks for one, the keys in base64 (`r1_funcore::net::wireguard::config`); a file that does not read counts as none kept; reached by the debug shell only once the safe's passphrase is given ([Network](network.md#the-tunnel)) |
| `.r1metal/analytics/uses` | every deed done, a line each with its UTC second and its word (`1767225600 photo`), whatever the logging ([Interface](../app/interface.md#home-and-the-menus)) |
| `.restore.new/`, `.restore/` | a restore's files while they are staged, and once they are confirmed, until the next start puts them in place; hidden from USB hosts ([Backup and restore](#backup-and-restore)) |

A test of `r1_funcore::fixtures` lists every path of `.r1metal`, the robot's memory and its conversation's days as this table does, so that an updated device finds what an earlier one kept. Nothing deletes news editions; a day of the conversation, a fact of the memory and the thread go only by a click on their `FORGET` ([Chat](../app/chat.md#memory)); the uses lose only the lines of `signal`, when the phone unlinks the R1 ([Signal](../app/signal.md#unlink)). Notes, translations, photos and videos are deleted from their screens. The Pokédex stores nothing here: a spoken name is recorded into an in-memory file (`sound::Memo`), and lookup results stay in memory ([Interface](../app/interface.md#pokédex)). Nor does the browser: a request said is recorded the same way, and its pages stay in memory; its part is handed the clock's directory alone, where it reads the place, `.r1metal/clock/where`, for the model's web search ([Interface](../app/interface.md#web)). The job that reads a request for the rundowns is handed the same directory for the same search and writes nothing: the rundowns a request changes are the interface's, and the settings' thread keeps them ([News](../app/news.md#requests)).

**Settings** (`r1_funcore::domain::settings`), one line each:

| Line | Values | Default |
|---|---|---|
| `brightness N` | percent, 10 to 100 in steps of 10 | 40 |
| `volume N` | percent, 0 to 100 in steps of 10 | 70 |
| `sounds N` | percent the interface's own sounds play at, the robot's searching, the bell's ring and the notice, 0 to 100 in steps of 10; at 0 none plays ([Interface](../app/interface.md#brightness-and-volume)) | 40 |
| `robot on\|off` | | on |
| `offset N` | minutes ahead of UTC | 0 |
| `logging off\|on\|debug` | | on |
| `theme WORD` | `stage`, `dial`, `deck`, `terminal`, `companion`, `rail` | `dial` |
| `signal_wake N` | minutes from Signal's last fetch or stay to its next fetch, awake or asleep: 0 (never), 5, 10, 15, 60 | 60 |
| `signal_aloud on\|off` | Signal's messages that come in a conversation shown in the voice are read aloud | on |
| `auto_replies N` | replies auto mode gives once switched on: 5, 10, 20, 50, 100 | 20 |
| `auto_gap N` | seconds between two of auto mode's replies at the least: 10, 30, 60, 120, 300 | 30 |
| `auto_hourly N` | auto mode's replies an hour at the most: 10, 20, 30, 60 | 30 |
| `colours WORD L` | one line per theme, palette `A` to `P` | C for every theme |
| `limit N\|none` | the most OpenAI's services may cost a month, in dollars: 1, 2, 5, 10, 20, 25, 50, 100, or none ([OpenAI](../app/openai.md#spending-limit)) | 25 |
| `model USE MODEL EFFORT` | one line per use of the language model, `robot`, `recollection`, `notes` (the summaries of notes and translations; a `translator` line of an earlier firmware is passed over), `news`, `auto`, `web`, `bell` (the reading of a request to the bell), `rundowns` (the reading of a request for the rundowns): its model, `gpt-6-luna`, `gpt-6.1-terra`, `gpt-6.1-sol`, `gpt-6-astra`, and its effort, `none`, `low`, `medium`, `high`, `xhigh`, `max` ([OpenAI](../app/openai.md#language-models)) | `gpt-6.1-terra high` for every use |
| `tool WORD on\|off` | one line per tool of the robot's, `web`, `device`, `notes`, `news`, `camera`, `signal` ([Chat](../app/chat.md#tools)) | on, `signal` off |
| `news N\|off` | the hour of the clock on screen the daily edition is made at, 0 to 23, or none ([News](../app/news.md)) | 8 |
| `signal_said text\|voice` | a message said for Signal is written down into a draft, or kept as a voice note ([Signal](../app/signal.md#attachments)) | `text` |
| `signal_text on\|off` | Signal's conversations show text alone | off |
| `pokedex WORD` | the language the Pokédex hears names in and writes its entries in, `en`, `de`, `fr`, `es`, `it` ([Interface](../app/interface.md#pokédex)) | `en` |
| `cellular on\|off` | MOBILE DATA's `MODEM`: the modem runs and stands in where WiFi is not to be had ([Network](network.md#mobile-data)) | off |
| `keep_pin on\|off` | MOBILE DATA's `KEEP PIN`: a PIN the SIM takes is kept in `.r1metal/cell/sim` and handed to it at the modem's next starts ([Interface](../app/interface.md#mobile-data)) | on |
| `usb_drive on\|off` | USB DRIVE's `DRIVE`: a computer on the cable is shown the files as a drive ([USB drive](#usb-drive)) | off |
| `usb_modem on\|off` | MOBILE DATA's `USB MODEM`: a computer on the cable is lent the modem's connection ([Network](network.md#router)) | off |
| `vpn on\|off` | MOBILE DATA's `VPN`: that computer's packets leave through the tunnel alone ([Network](network.md#the-tunnel)) | off |

Of the lines of a key, or of a theme for `colours`, a use for `model` and a tool for `tool`, the last whose value reads counts; where there is no such line (none, an unreadable value or an unknown word), the default stands. A value between steps is rounded to the nearest step, but for `limit`, `signal_wake` and auto mode's, which only their steps read as. An effort a model does not take reads as the least it takes above it. A file of an earlier firmware lacks the `colours rail` line and the lines from `limit` on, and reads as a new device has them. Its `alarm N` and `timer N` lines, one alarm and one timer in seconds of UTC, are the bell's: while the bell has no file of its own they are read as its entries, the alarm by the time the clock showed for it at the file's offset, and kept in the bell's file at once (`Bell::read`); the settings' file is written without them. Its `rundown news on|off` and `rundown finance on|off` lines are the rundowns': while the rundowns have no file of their own they switch the two a new device has, which are kept in the rundowns' file at once (`Topics::read`), and the settings' file is written without them too.

The settings thread reads its files as soon as the partition is mounted, however late that is, and writes a file whenever its content changes; the bell's file and the rundowns' are two of them. It also keeps the uses, and takes every use of a word out of them where asked (`Keeper::uses`, `uses::without_event`), answering once that is done. It gives the interface network names and whether a password is stored, never a password. A reboot or power off waits for a write in progress.

## Backup and restore

`r1 backup FILE` copies what the partition keeps into a file on the computer, and `r1 restore FILE` puts a backup's files on an R1 in place of what it keeps ([r1metal](../../README.md#use)). Both work over the debug shell with its gate open ([Console](console.md#the-gate)). What a backup takes, what stays the device's own and a restore's steps are `r1_funcore::domain::backup`; the device's side is `storage/restore.rs`, the computer's `crates/flasher/src/backup/`.

| | Paths from the partition's root |
|---|---|
| Taken (`backup::is_backed_up`) | `camera`, `audio`, `news` and `chat` (`PUBLIC_ROOTS`) and `.r1metal`, but no file half written (`.new`); never the safe's key files, so a restored R1 keeps its own key and passphrase |
| The device's own, taken by no backup and kept through a restore (`backup::device_entries`) | `.r1metal/battery`, `.r1metal/cell`, `.r1metal/logs`, `.r1metal/interface/flash` |

**Backup** (`backup` in `backup/mod.rs`):

1. `r1` asks for the passphrase of the R1's safe and opens the gate with it (`unlock`).
2. It lists the directories taken (`ls`), copies every file (`read`) and takes the SHA-256 of what it copied.
3. The R1 runs on meanwhile. `r1` lists again and asks the R1 for the sum of each file (`sum`): a file that changed or came is copied again, one that went is recorded as gone. The passes go on until one finds every file as it is recorded (`copy_all`), so the backup holds the files as they all stood at one moment. A file or directory the R1 lists and refuses to read in two passes running ends the backup with an error.
4. It shuts the gate (`lock`) and gives the file its name; until then it is written as `FILE.part`.

A Ctrl-C ends the copying, or a restore's staging, at the next part of a file, and the gate is shut behind it; a second one ends `r1` at once, and the gate then stays open until the cable is pulled or the app starts again.

**The file** is an age file (age-encryption.org/v1) of one scrypt recipient under the safe's passphrase (`backup/age.rs`):

- The passphrase is stretched by scrypt (N = 2^18, r = 8, p = 1) with 16 random bytes of salt, and the key that gives wraps the file's random key with ChaCha20-Poly1305. An HMAC-SHA-256 under a key drawn from the file key holds the header.
- The payload is sealed with ChaCha20-Poly1305 in chunks of 64 KiB, each by its number and the last marked as the last, so a file that was cut short or changed, or whose chunks changed places, is refused.
- A file that asks for more work than N = 2^22, or that is no age file of one scrypt recipient, is refused before its passphrase is stretched.
- The tests hold it to the file `scrypt` of age's test vectors: that file opens, and written with its key, salt and nonce, the header and the payload are its bytes.

The payload is the backup's records (`backup/records.rs`): the line `r1metal backup 1`, then record after record, each begun by a byte for its kind.

| Kind | Record |
|---|---|
| 1 | a file: its path from the partition's root behind its length (16 bit, little endian), then its bytes in parts of at most 49152, each behind its length (32 bit, little endian), then a length of 0 |
| 2 | a file that went while the backup was taken: its path behind its length |
| 0 | the end, which nothing follows |

Of two records of one path the later counts. A path that names nothing a backup takes (`backup::is_backed_up`) is refused as it is read.

**Restore** (`restore` in `backup/mod.rs`):

1. `r1` asks for the backup's passphrase and reads the backup whole, so that one that does not open or is cut short ends the restore before the R1 is asked anything. It asks for the passphrase of the R1's safe, Enter where it is the backup's, and opens the gate.
2. `stage` makes `/data/.restore.new` in place of whatever was staged or confirmed before, and in it `.r1metal` and each of `camera`, `audio`, `news` and `chat`, every one encrypted under the safe's key by the safe's own policy before a file is staged in it (`backup::staged_enclosed`, `safe::make_enclosed`): what is staged is encrypted as what it takes the place of.
3. `r1` writes each file the backup holds at its end under `/data/.restore.new` (`mkdir`, `append`) and holds it to the R1's sum of it (`sum`).
4. `commit` takes what is the device's own out of the staged files and gives the directory its confirmed name, `/data/.restore` (`backup::commit_steps`). `r1` shuts the gate and sends `reboot`.

A confirmed restore's files take the partition's place at the next start, in the storage thread, before any part reads the partition (`Found::settle_steps`, `Found::apply_steps`, run by `storage/restore.rs`):

1. Before the safe is looked at, staged files no commit confirmed go.
2. Once the passphrase is typed and the safe is open, the device's own entries are moved from `.r1metal` into the staged `.r1metal`; then `.r1metal` is moved to `.restore/replaced` and the staged one into its place. A staged `.r1metal` that is not encrypted under the safe's key is left out, and the log says so.
3. Each of `camera`, `audio`, `news` and `chat` that the backup holds takes its root's place whole: the root is removed and the staged one moved there. A root the backup holds nothing of stays: its staged root is empty, as the staging made it, and counts as not staged (`backup::is_staged`).
4. `.restore` goes with what is left in it and the free blocks are handed back to the medium. Then every public root is under the safe's key, one a restore left plain moved under it file by file ([The safe](#the-safe)), and the public and the private directories are made where one is missing.

Every step is a removal or a rename on the one filesystem. A start cut short anywhere is taken up by the next from what `.restore` still holds; where it was cut between `.r1metal` moved away and its successor moved in, the next start moves the successor in before it looks at the safe, which the R1's passphrase opens as before: the staged `.r1metal` is under the R1's own key, so a restore changes no passphrase. A step that fails leaves the partition unmounted for that run, the debug screen's `DATA` line tells the error, and the next start tries again. The log tells what a restore put in place (`data: a restore put in place: camera, chat, .r1metal`).

The staged files need room on the partition beside what it keeps. With the backup come the OpenAI key, the known networks with their passwords and the secret their MAC addresses are derived from, the tunnel's config with its private key, and Signal's account as the backup's R1 held it, with its device ID and keys: of two R1s that hold one account so, only one may run with it ([Signal](../app/signal.md#privacy)), and of two that hold one tunnel's config only one may run the tunnel at a time, as its peer knows one end by that key.

## USB drive

On a USB cable the R1 is a composite gadget: the console, a removable write-protected drive (`r1metal R1`, `r1_funcore::hw::usb::inquiry`) with one exFAT volume labelled `r1metal`, the screen recording function, and the network adapter of the router ([Network](network.md#router)). The drive holds a medium only while the owner has it shown: `DRIVE` under `USB DRIVE` in the settings, kept as `usb_drive`, off on a new device ([Interface](../app/interface.md#settings)). Off, a host finds a drive without a medium; a host that is shown the medium reads every file under the safe's key decrypted, through the open safe.

The volume is not stored anywhere; it is generated:

- While the drive is shown and a host has configured the device (`USB_STATE` `CONFIGURED`; `HostChanges::is_offered`), the `drive` thread (`usb::drive::Drive`) lists `/data` and builds the volume's metadata in memory (`r1_funcore::fs::exfat`: partition table, boot region, allocation table, bitmap, up-case table, directories), keeping each file's entries and path. The main loop hands the thread the setting as it changes (`Drive::set_shown` in `App::follow_settings`), so switching it on with a host there puts the medium in at once.
- At `DISCONNECTED`, and as the setting is switched off, the medium is ejected. A host that disconnects and reconnects between two checks gets a fresh medium (`r1_funcore::hw::usb::Medium`).
- The kernel's mass storage function reads `/drive/volume` from a FUSE mount (`usb/fuse.rs`). The `drive fuse` thread answers the reads: metadata sectors from memory, file sectors from the file on `/data`.
- A host sees the files as they were when it connected. After an app restart a host keeps the old medium until the cable is reconnected. A file that has gone, cannot be read or has become shorter since reads as zeros from where it fails, and the log tells it with the path once for all the blocks a host reads of it (`drive: opening /data/camera/pictures/a.jpg: ...`, `OpenFile::is_failure_new`); a reply the kernel does not take is told once until one is taken again (`drive: answering the kernel: ...`).
- Volume size and cluster size follow the filesystem; the serial number is the first four bytes of its UUID (`r1_funcore::hw::usb::serial`).
- Left out, and counted in the log: symbolic links and special files; names that are not UTF-8 or that a host could not open (a control character, one of `" * / : < > ? \ |`, over 255 UTF-16 units, a trailing dot or space); a name that differs only in case from an earlier one; anything that would grow a directory past exFAT's 256 MiB. `.r1metal`, the safe's key files, written or half written, and a restore's `.restore.new` and `.restore` are left out and not counted (`paths::is_shown_to_hosts`).
- File times are local time, each with the UTC offset (exFAT stores it in quarter hours); an offset that is not a whole number of quarter hours shifts the time instead.
- Linux mounts the volume through udisks and `fsck.exfat` passes; macOS mounts it read-only at `/Volumes/r1metal` through FSKit's exFAT.
